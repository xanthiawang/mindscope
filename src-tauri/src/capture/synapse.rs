//! Synapse module — MindScope's background AI knowledge loop.
//!
//! Responsibilities:
//! - Bootstrap: on first run, seed ~/.mindscope/synapse/ and ~/.mindscope/vault/
//!   with skill definitions, CLAUDE.md, and working-memory template from app
//!   resources.
//! - Background loop: periodically invoke Claude CLI in the vault directory to
//!   refresh _working-memory.md based on recent screen activity + audio
//!   transcripts + meeting notes.
//! - Manual trigger: Tauri command for on-demand updates (called from Brief
//!   panel's Refresh button).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use super::claude_cli;
use super::paths;
use super::text;
use super::util::LockExt;

static SYNAPSE_LOOP_RUNNING: AtomicBool = AtomicBool::new(false);
static SYNAPSE_IS_SYNCING: AtomicBool = AtomicBool::new(false);
/// Frame watermark (count, max id) + audio segment count seen by the last pass —
/// the loop skips a pass when nothing new was recorded.
static LAST_ACTIVITY_MARK: Mutex<Option<(i64, i64, usize)>> = Mutex::new(None);
static LAST_RUN_SECS: AtomicI64 = AtomicI64::new(0);

/// `$MINDSCOPE_HOME/synapse/` — where skills, CLAUDE.md, and .claude/ live
fn synapse_dir() -> PathBuf { paths::synapse_dir() }

/// `$MINDSCOPE_HOME/vault/` — the user's vault (working memory, meetings)
fn vault_dir() -> PathBuf { paths::vault_dir() }

/// Find the bundled resources directory.
/// In a built app: MindScope.app/Contents/Resources/resources/synapse/
/// In dev: src-tauri/resources/synapse/
fn bundled_resources() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(macos_dir) = exe.parent() {
            if let Some(contents_dir) = macos_dir.parent() {
                let resources = contents_dir.join("Resources").join("resources").join("synapse");
                if resources.exists() { return Some(resources); }
                let alt = contents_dir.join("Resources").join("synapse");
                if alt.exists() { return Some(alt); }
            }
        }
    }
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources").join("synapse");
    if dev.exists() { return Some(dev); }
    None
}

/// Recursively copy a directory tree, skipping files that already exist.
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    if !src.exists() { return Ok(()); }
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_tree(&src_path, &dst_path)?;
        } else if !dst_path.exists() {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Bootstrap the synapse directory on first run.
/// Copies skills, CLAUDE.md, and seeds working memory if missing.
/// Also migrates from legacy ~/.mindscope/cortex/ if it exists (previous name).
/// Safe to call every startup — won't overwrite existing user edits.
pub fn bootstrap() {
    // Migrate legacy cortex directory if it exists
    let legacy = paths::mindscope_home().join("cortex");
    if legacy.exists() {
        let _ = fs::remove_dir_all(&legacy);
        log::info!("MindScope synapse: removed legacy cortex directory");
    }

    let bundled = match bundled_resources() {
        Some(p) => p,
        None => {
            log::warn!("MindScope synapse: bundled resources not found");
            return;
        }
    };

    let synapse = synapse_dir();
    let vault = vault_dir();

    let _ = fs::create_dir_all(&synapse);
    let _ = fs::create_dir_all(&vault);

    // Copy skills → ~/.mindscope/synapse/.claude/skills/
    let claude_skills = synapse.join(".claude").join("skills");
    let _ = fs::create_dir_all(&claude_skills);
    let src_skills = bundled.join("skills");
    if src_skills.exists() {
        if let Err(e) = copy_tree(&src_skills, &claude_skills) {
            log::warn!("MindScope synapse: failed to copy skills: {}", e);
        }
    }

    // Skills that older builds shipped but that describe tools/files MindScope
    // never had (manifests, lockfiles, git commits). Remove so Claude doesn't
    // read stale instructions.
    let _ = fs::remove_dir_all(claude_skills.join("sync"));

    // CLAUDE.md: the bundled copy is the source of truth for the tool-free
    // protocol, so (re)write it unless the user customised it.
    let src_claude_md = bundled.join("CLAUDE.md");
    for dst in [synapse.join("CLAUDE.md"), vault.join("CLAUDE.md")] {
        if !src_claude_md.exists() { break; }
        let user_edited = fs::read_to_string(&dst)
            .map(|c| !c.contains("MindScope Synapse") || c.contains("USER-EDITED"))
            .unwrap_or(false);
        if !dst.exists() || !user_edited {
            let _ = fs::copy(&src_claude_md, &dst);
        }
    }

    // Seed _working-memory.md into vault (only if missing)
    let wm_dst = vault.join("_working-memory.md");
    if !wm_dst.exists() {
        let wm_src = bundled.join("seed-vault").join("_working-memory.md");
        if wm_src.exists() {
            let _ = fs::copy(&wm_src, &wm_dst);
            log::info!("MindScope synapse: seeded _working-memory.md");
        }
    }

    // Seed _warm-memory.md into vault (only if missing) — warm tier
    let wmem_dst = vault.join("_warm-memory.md");
    if !wmem_dst.exists() {
        let wmem_src = bundled.join("seed-vault").join("_warm-memory.md");
        if wmem_src.exists() {
            let _ = fs::copy(&wmem_src, &wmem_dst);
            log::info!("MindScope synapse: seeded _warm-memory.md");
        }
    }

    // One-shot migration: rename legacy _context-model.md → _warm-memory.md
    // so existing users don't end up with orphaned files after upgrade.
    let legacy_cm = vault.join("_context-model.md");
    if legacy_cm.exists() && !wmem_dst.exists() {
        let _ = fs::rename(&legacy_cm, &wmem_dst);
        log::info!("MindScope synapse: migrated _context-model.md → _warm-memory.md");
    } else if legacy_cm.exists() {
        // Both exist (unlikely) — remove the legacy one to avoid confusion.
        let _ = fs::remove_file(&legacy_cm);
    }

    // Link skills into vault/.claude/skills for Claude CLI in vault cwd
    let vault_claude_dir = vault.join(".claude");
    let vault_claude_skills_dir = vault_claude_dir.join("skills");
    let _ = fs::create_dir_all(&vault_claude_dir);

    // Remove stale symlink pointing at legacy cortex path
    if let Ok(meta) = fs::symlink_metadata(&vault_claude_skills_dir) {
        if meta.file_type().is_symlink() {
            if let Ok(target) = fs::read_link(&vault_claude_skills_dir) {
                if target.to_string_lossy().contains("cortex") {
                    let _ = fs::remove_file(&vault_claude_skills_dir);
                }
            }
        }
    }

    if !vault_claude_skills_dir.exists() && src_skills.exists() {
        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink(&claude_skills, &vault_claude_skills_dir);
        }
        #[cfg(not(unix))]
        {
            let _ = copy_tree(&src_skills, &vault_claude_skills_dir);
        }
    }

    log::info!("MindScope synapse: bootstrap complete at {:?}", synapse);
}

/// Run the synapse update loop in a background thread.
/// Every 30 minutes, ask Claude CLI to refresh _working-memory.md.
pub fn start_synapse_loop() {
    if SYNAPSE_LOOP_RUNNING.load(Ordering::Relaxed) { return; }
    SYNAPSE_LOOP_RUNNING.store(true, Ordering::Relaxed);

    thread::spawn(|| {
        thread::sleep(Duration::from_secs(120));
        loop {
            if has_new_activity() {
                if let Err(e) = run_synapse_update() {
                    log::warn!("MindScope synapse loop error: {}", e);
                }
            } else {
                log::info!("MindScope synapse: no new activity since last pass, skipping");
            }
            thread::sleep(Duration::from_secs(30 * 60));
        }
    });

    log::info!("MindScope synapse: background loop started (30min cadence)");
}

/// Current activity watermark: (frame count, max frame id, audio segments today).
fn activity_mark() -> (i64, i64, usize) {
    let (count, max_id) = super::db::frame_watermark();
    let audio = super::audio::count_audio_segments(&super::timeutil::today());
    (count, max_id, audio)
}

/// Did anything get recorded since the last completed pass?
fn has_new_activity() -> bool {
    let now = activity_mark();
    match *LAST_ACTIVITY_MARK.lock_or_recover() {
        Some(prev) => prev != now,
        None => true,
    }
}

/// Seconds since the last completed pass (0 = never).
pub fn last_run_secs() -> i64 { LAST_RUN_SECS.load(Ordering::Relaxed) }

/// One synapse update pass. Called by the loop and by the manual Refresh button.
pub fn run_synapse_update() -> Result<String, String> {
    // swap: exactly one caller wins even if two trigger at the same instant.
    if SYNAPSE_IS_SYNCING.swap(true, Ordering::SeqCst) {
        return Err("synapse update already in progress".into());
    }
    let mark = activity_mark();
    let result = do_synapse_update();
    if result.is_ok() {
        *LAST_ACTIVITY_MARK.lock_or_recover() = Some(mark);
        LAST_RUN_SECS.store(super::timeutil::now_secs(), Ordering::Relaxed);
    }
    SYNAPSE_IS_SYNCING.store(false, Ordering::SeqCst);
    result
}

/// Hard cap for _working-memory.md. If exceeded, auto-trim oldest Today's
/// Activity rows before asking Claude to do anything.
const WORKING_MEMORY_HARD_CAP: usize = 4000;
/// Hard cap for _warm-memory.md.
const WARM_MEMORY_HARD_CAP: usize = 8000;

/// Pre-flight guard: if _working-memory.md has blown past the hard cap,
/// brute-force trim the oldest rows in `## Today's Activity` and the oldest
/// entries in `## Recent People`. This prevents unbounded growth and gives
/// Claude a sane starting point for its section-scoped edits.
fn trim_working_memory(path: &Path) {
    let content = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return,
    };
    if content.chars().count() <= WORKING_MEMORY_HARD_CAP {
        return;
    }

    log::warn!(
        "MindScope synapse: working-memory.md bloated ({} chars), auto-trimming",
        content.chars().count()
    );

    // Split into sections by "## " heading. Trim the table rows inside
    // "Today's Activity" first, then "Recent People" bullets if still over.
    let mut out = String::with_capacity(content.len());
    let mut in_activity = false;
    let mut in_people = false;
    let mut activity_data_rows = 0usize; // excludes header + separator
    let mut people_bullets = 0usize;

    for line in content.lines() {
        if line.starts_with("## ") {
            in_activity = line.contains("Today's Activity");
            in_people = line.contains("Recent People");
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if in_activity && line.trim_start().starts_with('|') && !line.contains("---") {
            activity_data_rows += 1;
            // Keep only the 4 most-recent rows (assume chronological; Claude
            // appends, so "latest" = later in file). We approximate by keeping
            // the header + sep (first 2 rows) and the last 4 data rows.
            // Strategy: on first pass just count. Second pass will filter.
            // Since this is a streaming single-pass, fall back to a simpler
            // rule: drop data rows until we're under cap.
            if content.len() > WORKING_MEMORY_HARD_CAP && activity_data_rows > 4 {
                continue; // skip (drop) this row
            }
        }

        if in_people && line.trim_start().starts_with("- ") {
            people_bullets += 1;
            if content.len() > WORKING_MEMORY_HARD_CAP + 500 && people_bullets > 3 {
                continue;
            }
        }

        out.push_str(line);
        out.push('\n');
    }

    // If still over, hard-truncate with a warning marker.
    if out.chars().count() > WORKING_MEMORY_HARD_CAP + 1000 {
        let mut truncated: String = out.chars().take(WORKING_MEMORY_HARD_CAP).collect();
        truncated.push_str("\n\n<!-- synapse: auto-truncated due to size cap -->\n");
        out = truncated;
    }

    if let Err(e) = fs::write(path, &out) {
        log::warn!("MindScope synapse: failed to write trimmed working memory: {}", e);
    }
}

/// Pre-flight guard for _warm-memory.md. Simpler: if over cap, truncate
/// from the bottom preserving the frontmatter + first 4 sections.
fn trim_warm_memory(path: &Path) {
    let content = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return,
    };
    if content.chars().count() <= WARM_MEMORY_HARD_CAP {
        return;
    }

    log::warn!(
        "MindScope synapse: warm-memory.md bloated ({} chars), auto-trimming",
        content.chars().count()
    );

    let truncated: String = content.chars().take(WARM_MEMORY_HARD_CAP).collect();
    let marker = "\n\n<!-- synapse: auto-truncated due to size cap -->\n";
    let final_content = format!("{}{}", truncated, marker);
    if let Err(e) = fs::write(path, final_content) {
        log::warn!("MindScope synapse: failed to write trimmed warm memory: {}", e);
    }
}

fn do_synapse_update() -> Result<String, String> {
    let vault = vault_dir();
    if !vault.exists() {
        return Err("vault directory does not exist".into());
    }

    if !claude_cli::available() {
        return Err("Claude CLI not found — install `claude` (npm i -g @anthropic-ai/claude-code) or set claude_path in settings.json".into());
    }

    // Pre-flight: enforce hard size caps BEFORE asking Claude to touch anything.
    trim_working_memory(&vault.join("_working-memory.md"));
    trim_warm_memory(&vault.join("_warm-memory.md"));

    let activity = gather_recent_activity();

    // Read vault files in Rust so Claude doesn't need Read/Edit tools.
    // This avoids the "400 tool use concurrency" error that occurs when
    // Claude Desktop or Claude Code is running simultaneously.
    let wm_path = vault.join("_working-memory.md");
    let warm_path = vault.join("_warm-memory.md");
    let wm_content = fs::read_to_string(&wm_path).unwrap_or_default();
    let warm_content = fs::read_to_string(&warm_path).unwrap_or_default();

    // Tool-free prompt: file contents are inlined, Claude outputs the
    // FULL updated file contents, Rust writes them back.
    // No Read/Edit tools needed → no concurrency conflict with Claude Desktop.
    let prompt = format!(
        "You are the MindScope Synapse loop. You will be given the current vault files \
         and recent screen/audio activity. Your job is to output the UPDATED file contents.\n\n\
         ═══════════════ CURRENT _working-memory.md ═══════════════\n\
         {}\n\
         ═══════════════ CURRENT _warm-memory.md ═══════════════\n\
         {}\n\
         ═══════════════ RECENT ACTIVITY (last 2 hours) ═══════════════\n\
         {}\n\
         ══════════════════════════════════════════════════════════════\n\n\
         RULES:\n\
         1. Only update content inside these managed sections:\n\
            - _working-memory.md: ## Current Focus, ## Today's Activity, ## Recent People, ## Source Sync Status\n\
            - _warm-memory.md: ## Active Follow-Ups, ## Project Momentum, ## Collaborator State, ## Needs Triage, ## Recent Decisions\n\
         2. NEVER modify ## User Notes or anything below USER-OWNED ZONE. Copy them EXACTLY as-is.\n\
         3. Hard caps: _working-memory.md ≤ 4000 chars, _warm-memory.md ≤ 8000 chars.\n\
         4. Today's Activity: max 8 rows, drop oldest. Recent People: max 6 entries.\n\
         5. Never invent data. If activity is empty/sparse, keep existing content mostly unchanged.\n\
         6. Be concise. Max 80 chars per focus line.\n\
         7. Copy ALL frontmatter (--- block) and structural comments EXACTLY.\n\n\
         OUTPUT FORMAT — respond with exactly two fenced blocks, nothing else:\n\n\
         ```working-memory\n\
         (entire updated _working-memory.md content here)\n\
         ```\n\n\
         ```warm-memory\n\
         (entire updated _warm-memory.md content here)\n\
         ```",
        wm_content, warm_content, activity
    );

    // Tool-free call: file contents are in the prompt; updated files come back
    // as fenced text (no Read/Edit tools → no conflict with a running Claude Code).
    let mut opts = claude_cli::CallOpts::background();
    opts.cwd = Some(vault.clone());
    let stdout = claude_cli::run("synapse", &prompt, opts)?;

    // Parse the two fenced blocks and write them back to disk.
    apply_fenced_output(&vault, &stdout, &wm_content, &warm_content)?;

    // Post-flight: enforce caps again (idempotent safety net).
    trim_working_memory(&wm_path);
    trim_warm_memory(&warm_path);
    log::info!("MindScope synapse: update complete");
    Ok(stdout)
}

/// Extract a fenced code block by its language tag from Claude's response.
/// Looks for ```tag\n...\n``` and returns the inner content.
fn extract_fenced_block(response: &str, tag: &str) -> Option<String> {
    let opener = format!("```{}", tag);
    let start = response.find(&opener)?;
    let content_start = response[start..].find('\n')? + start + 1;
    // Find the closing ``` — must be on its own line
    let rest = &response[content_start..];
    let end = rest.find("\n```")
        .map(|p| content_start + p)
        .or_else(|| {
            // Also try ``` at end of string
            if rest.ends_with("```") {
                Some(content_start + rest.len() - 3)
            } else {
                None
            }
        })?;
    Some(response[content_start..end].to_string())
}

/// Parse Claude's fenced-block response and write updated files back to vault.
/// Validates: USER-OWNED ZONE preserved, size caps respected.
fn apply_fenced_output(
    vault: &Path,
    response: &str,
    original_wm: &str,
    original_warm: &str,
) -> Result<(), String> {

    // Helper: ensure USER-OWNED ZONE content is preserved exactly
    fn preserve_user_zone(original: &str, updated: &str) -> String {
        let zone_marker = "## User Notes";
        let orig_zone = original.find(zone_marker)
            .map(|pos| &original[pos..]);
        let updated_zone_pos = updated.find(zone_marker);

        match (orig_zone, updated_zone_pos) {
            (Some(orig_tail), Some(pos)) => {
                // Replace whatever Claude wrote below ## User Notes with original
                format!("{}{}", &updated[..pos], orig_tail)
            }
            (Some(orig_tail), None) => {
                // Claude dropped the zone entirely — append it
                format!("{}\n\n{}", updated.trim_end(), orig_tail)
            }
            _ => updated.to_string(),
        }
    }

    let mut applied = 0;

    // Process _working-memory.md
    if let Some(new_wm) = extract_fenced_block(response, "working-memory") {
        let safe_wm = preserve_user_zone(original_wm, &new_wm);
        if safe_wm.chars().count() <= WORKING_MEMORY_HARD_CAP + 500 {
            let wm_path = vault.join("_working-memory.md");
            fs::write(&wm_path, &safe_wm)
                .map_err(|e| format!("failed to write _working-memory.md: {}", e))?;
            log::info!("MindScope synapse: updated _working-memory.md ({} → {} chars)",
                       original_wm.chars().count(), safe_wm.chars().count());
            applied += 1;
        } else {
            log::warn!("MindScope synapse: _working-memory.md update exceeds cap ({}), skipping",
                       safe_wm.chars().count());
        }
    }

    // Process _warm-memory.md
    if let Some(new_warm) = extract_fenced_block(response, "warm-memory") {
        let safe_warm = preserve_user_zone(original_warm, &new_warm);
        if safe_warm.chars().count() <= WARM_MEMORY_HARD_CAP + 500 {
            let warm_path = vault.join("_warm-memory.md");
            fs::write(&warm_path, &safe_warm)
                .map_err(|e| format!("failed to write _warm-memory.md: {}", e))?;
            log::info!("MindScope synapse: updated _warm-memory.md ({} → {} chars)",
                       original_warm.chars().count(), safe_warm.chars().count());
            applied += 1;
        } else {
            log::warn!("MindScope synapse: _warm-memory.md update exceeds cap ({}), skipping",
                       safe_warm.chars().count());
        }
    }

    if applied == 0 {
        log::warn!("MindScope synapse: no fenced blocks found in response, no files updated");
    }

    Ok(())
}

fn gather_recent_activity() -> String {
    use std::collections::HashMap;
    use super::db;

    let date = super::timeutil::today();
    let now_us = super::timeutil::now_micros();
    let cutoff = now_us - (2 * 3600 * 1_000_000);
    let frames = db::get_frames_in_range(cutoff, now_us, 0).unwrap_or_default();
    if frames.is_empty() {
        return "(no screen activity recorded in the last 2 hours)".to_string();
    }
    let recent: Vec<_> = frames.iter().collect();

    let mut app_counts: HashMap<String, u64> = HashMap::new();
    for f in &recent {
        if f.app_name.is_empty() || f.app_name == "Unknown" { continue; }
        *app_counts.entry(f.app_name.clone()).or_insert(0) += 1;
    }
    let mut apps: Vec<(String, u64)> = app_counts.into_iter()
        .map(|(a, c)| (a, (c * 3) / 60))
        .filter(|(_, m)| *m >= 1)
        .collect();
    apps.sort_by(|a, b| b.1.cmp(&a.1));

    let mut ocr_samples: Vec<String> = Vec::new();
    for f in recent.iter().rev().take(20) {
        if text::char_len(&f.ocr_text) > 30 {
            ocr_samples.push(format!("[{}] {}", f.app_name, text::truncate_chars(&f.ocr_text, 150)));
        }
    }

    let mut out = String::new();
    out.push_str("## App Time (last 2 hours)\n");
    for (app, mins) in apps.iter().take(8) {
        out.push_str(&format!("- {}: {}m\n", app, mins));
    }
    out.push_str("\n## Screen Text Samples\n");
    for s in ocr_samples.iter().take(5) {
        out.push_str(&format!("- {}\n", s));
    }

    let audio_segments = super::audio::load_audio_segments(&date);
    if !audio_segments.is_empty() {
        out.push_str("\n## Meeting Transcripts (today)\n");
        for seg in audio_segments.iter().rev().take(10) {
            if seg.transcript.is_empty() { continue; }
            let snippet: String = seg.transcript.chars().take(200).collect();
            out.push_str(&format!("- {} [{}]: {}\n", seg.timestamp, seg.session_type, snippet));
        }
    }

    out
}

/// Check if synapse is currently syncing (for UI indicators).
pub fn is_syncing() -> bool {
    SYNAPSE_IS_SYNCING.load(Ordering::Relaxed)
}
