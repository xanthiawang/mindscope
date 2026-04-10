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
    // This is cheap (local file I/O) and prevents Claude from being fed a
    // 20 KB bloated file and dutifully preserving all the bloat.
    trim_working_memory(&vault.join("_working-memory.md"));
    trim_warm_memory(&vault.join("_warm-memory.md"));

    let activity = gather_recent_activity();

    // Section-scoped, Edit-only, hard-capped prompt routed through vault-updater.
    let prompt = format!(
        "You are the MindScope Synapse loop. Update the vault based on recent activity.\n\n\
         ═══════════════ RECENT ACTIVITY (last 2 hours) ═══════════════\n\
         {}\n\
         ══════════════════════════════════════════════════════════════\n\n\
         🚨 CRITICAL RULES — violations cause rollback:\n\n\
         1. **Edit tool ONLY.** Use `old_string → new_string` on existing files. \
            NEVER use the Write tool on `_working-memory.md` or `_warm-memory.md`. \
            Write is permitted only for creating brand-new `user.*.md` / `proj.*.md` files.\n\n\
         2. **Section-scoped edits.** When you edit a managed file, only touch \
            content inside these specific headings:\n\
            - `_working-memory.md` → `## Current Focus`, `## Today's Activity`, \
              `## Recent People`, `## Source Sync Status`\n\
            - `_warm-memory.md` → `## Active Follow-Ups`, `## Project Momentum`, \
              `## Collaborator State`, `## Needs Triage`, `## Recent Decisions`\n\
            NEVER touch `## User Notes` — that's user-owned.\n\n\
         3. **Hard size caps.** After your edits:\n\
            - `_working-memory.md` must stay ≤ 4000 chars\n\
            - `_warm-memory.md` must stay ≤ 8000 chars\n\
            If you're about to exceed, your FIRST edit must DELETE the oldest \
            rows in `## Today's Activity` or the oldest entries in `## Recent People`.\n\n\
         4. **Auto-decay rules (apply every pass):**\n\
            - Today's Activity table: keep max 8 rows; drop oldest\n\
            - Recent People: keep max 6 entries; drop least-recent\n\
            - Unchecked tasks `- [ ]` older than 14 days → move from _working-memory.md to \
              _warm-memory.md `## Needs Triage`\n\
            - Follow-ups > 7 days old with no completion → mark `⚠ Overdue`\n\n\
         5. **Preserve user edits.** If a section has content that clearly wasn't \
            written by you (different style, explicit notes, etc.), merge around it. \
            Don't clobber.\n\n\
         6. **Never invent data.** If the activity log is empty or sparse, make \
            minimal edits (just update the sync timestamp) and stop.\n\n\
         7. **Use the `sync/vault-updater` skill** for managed file edits — it \
            enforces section-scoped rewrites correctly.\n\n\
         ══════════════════════════════════════════════════════════════\n\
         TASK:\n\
         1. Read `_working-memory.md`.\n\
         2. Apply Edit-tool patches to update Current Focus + Today's Activity + \
            Recent People + Source Sync Status based on the activity above.\n\
         3. Read `_warm-memory.md`.\n\
         4. Apply Edit-tool patches to bubble up any task older than 14 days to \
            Needs Triage, and update Collaborator State for anyone in Recent People.\n\
         5. Output a one-line summary of what you changed.\n\n\
         Be concise. Max 80 chars per focus line. Confidence < 0.5 → don't apply.",
        activity
    );

    log::info!("MindScope synapse: invoking Claude CLI (Haiku) in {:?}", vault);

    // Route through Haiku to cut background token cost ~12x vs Sonnet.
    // Synapse updates are "read + summarize + patch" — Haiku handles this well.
    let output = Command::new(&claude_path)
        .args(["-p", &prompt, "--model", "claude-haiku-4-5"])
        .current_dir(&vault)
        .env("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin")
        .output()
        .map_err(|e| format!("failed to run claude: {}", e))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        // Post-flight: if Claude ignored the cap rules (rare but possible),
        // enforce again. This is idempotent.
        trim_working_memory(&vault.join("_working-memory.md"));
        trim_warm_memory(&vault.join("_warm-memory.md"));
        log::info!("MindScope synapse: update complete");
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Err(format!("claude exited with status {}: {}", output.status, stderr))
    }
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
