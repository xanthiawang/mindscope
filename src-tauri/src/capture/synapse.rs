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
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

static SYNAPSE_LOOP_RUNNING: AtomicBool = AtomicBool::new(false);
static SYNAPSE_IS_SYNCING: AtomicBool = AtomicBool::new(false);

/// Returns ~/.mindscope/synapse/ — where skills, CLAUDE.md, and .claude/ live
fn synapse_dir() -> PathBuf {
    dirs_next::home_dir().unwrap_or_default().join(".mindscope").join("synapse")
}

/// Returns ~/.mindscope/vault/ — the user's vault (working memory, meetings)
fn vault_dir() -> PathBuf {
    dirs_next::home_dir().unwrap_or_default().join(".mindscope").join("vault")
}

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
    let legacy = dirs_next::home_dir().unwrap_or_default().join(".mindscope").join("cortex");
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

    // Copy CLAUDE.md into synapse dir
    let src_claude_md = bundled.join("CLAUDE.md");
    let dst_claude_md = synapse.join("CLAUDE.md");
    if src_claude_md.exists() && !dst_claude_md.exists() {
        let _ = fs::copy(&src_claude_md, &dst_claude_md);
    }

    // Copy CLAUDE.md into vault dir so Claude CLI running there picks it up
    let vault_claude_md = vault.join("CLAUDE.md");
    if src_claude_md.exists() && !vault_claude_md.exists() {
        let _ = fs::copy(&src_claude_md, &vault_claude_md);
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
            if let Err(e) = run_synapse_update() {
                log::warn!("MindScope synapse loop error: {}", e);
            }
            thread::sleep(Duration::from_secs(30 * 60));
        }
    });

    log::info!("MindScope synapse: background loop started (30min cadence)");
}

/// One synapse update pass. Called by the loop and by the manual Refresh button.
pub fn run_synapse_update() -> Result<String, String> {
    if SYNAPSE_IS_SYNCING.load(Ordering::Relaxed) {
        return Err("synapse update already in progress".into());
    }
    SYNAPSE_IS_SYNCING.store(true, Ordering::Relaxed);
    let result = do_synapse_update();
    SYNAPSE_IS_SYNCING.store(false, Ordering::Relaxed);
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
    if content.len() <= WORKING_MEMORY_HARD_CAP {
        return;
    }

    log::warn!(
        "MindScope synapse: working-memory.md bloated ({} chars), auto-trimming",
        content.len()
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
    if out.len() > WORKING_MEMORY_HARD_CAP + 1000 {
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
    if content.len() <= WARM_MEMORY_HARD_CAP {
        return;
    }

    log::warn!(
        "MindScope synapse: warm-memory.md bloated ({} chars), auto-trimming",
        content.len()
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

    let claude_path = find_claude_cli()
        .ok_or("Claude CLI not found — install via `brew install claude`")?;

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

    log::info!("MindScope synapse: invoking Claude CLI (Haiku, tool-free mode)");

    // --allowedTools "": prevent Claude CLI from registering ANY tools with the API.
    // File contents are in the prompt; updated files come back as fenced text.
    // This eliminates the "400 tool use concurrency" error that fires when
    // Claude Desktop or Claude Code sessions are running simultaneously.
    let output = Command::new(&claude_path)
        .args(["-p", &prompt, "--model", "claude-haiku-4-5", "--allowedTools", ""])
        .current_dir(&vault)
        .env("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin")
        .output()
        .map_err(|e| format!("failed to run claude: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(format!("claude exited with status {}: {}", output.status, stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

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
        if safe_wm.len() <= WORKING_MEMORY_HARD_CAP + 500 {
            let wm_path = vault.join("_working-memory.md");
            fs::write(&wm_path, &safe_wm)
                .map_err(|e| format!("failed to write _working-memory.md: {}", e))?;
            log::info!("MindScope synapse: updated _working-memory.md ({} → {} chars)",
                       original_wm.len(), safe_wm.len());
            applied += 1;
        } else {
            log::warn!("MindScope synapse: _working-memory.md update exceeds cap ({}), skipping",
                       safe_wm.len());
        }
    }

    // Process _warm-memory.md
    if let Some(new_warm) = extract_fenced_block(response, "warm-memory") {
        let safe_warm = preserve_user_zone(original_warm, &new_warm);
        if safe_warm.len() <= WARM_MEMORY_HARD_CAP + 500 {
            let warm_path = vault.join("_warm-memory.md");
            fs::write(&warm_path, &safe_warm)
                .map_err(|e| format!("failed to write _warm-memory.md: {}", e))?;
            log::info!("MindScope synapse: updated _warm-memory.md ({} → {} chars)",
                       original_warm.len(), safe_warm.len());
            applied += 1;
        } else {
            log::warn!("MindScope synapse: _warm-memory.md update exceeds cap ({}), skipping",
                       safe_warm.len());
        }
    }

    if applied == 0 {
        log::warn!("MindScope synapse: no fenced blocks found in response, no files updated");
    }

    Ok(())
}

fn find_claude_cli() -> Option<PathBuf> {
    let candidates = [
        "/opt/homebrew/bin/claude",
        "/usr/local/bin/claude",
        "/opt/local/bin/claude",
    ];
    for path in &candidates {
        let p = PathBuf::from(path);
        if p.exists() { return Some(p); }
    }
    if let Ok(out) = Command::new("which").arg("claude").output() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() { return Some(PathBuf::from(s)); }
    }
    None
}

fn gather_recent_activity() -> String {
    use std::collections::HashMap;
    use super::db;

    let date = super::recorder::timestamp_now()[..10].to_string();
    let frames = db::get_frames_for_date(&date).unwrap_or_default();
    if frames.is_empty() {
        return "(no screen activity recorded today yet)".to_string();
    }

    let now_us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i64;
    let cutoff = now_us - (2 * 3600 * 1_000_000);

    let recent: Vec<_> = frames.iter().filter(|f| f.timestamp >= cutoff).collect();

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
        if f.ocr_text.len() > 30 {
            let clean = f.ocr_text.split("---REGIONS---").next().unwrap_or(&f.ocr_text);
            let snippet: String = clean.chars().take(150).collect();
            ocr_samples.push(format!("[{}] {}", f.app_name, snippet));
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
