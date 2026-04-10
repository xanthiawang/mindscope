//! Cortex module — ports Agentic Cortex into MindScope.
//!
//! Responsibilities:
//! - Bootstrap: on first run, seed ~/.mindscope/cortex/ and ~/.mindscope/vault/ with
//!   skill definitions, CLAUDE.md, and working-memory template from app resources.
//! - Background loop: periodically invoke Claude CLI in the vault directory to
//!   run the command-center skill, which updates _working-memory.md based on
//!   recent screen activity + audio transcripts + meeting notes.
//! - Manual trigger: Tauri command for on-demand updates.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

static CORTEX_SYNC_RUNNING: AtomicBool = AtomicBool::new(false);
static CORTEX_IS_SYNCING: AtomicBool = AtomicBool::new(false);

/// Returns ~/.mindscope/cortex/ — where skills, CLAUDE.md, and .claude/ live
fn cortex_dir() -> PathBuf {
    dirs_next::home_dir().unwrap_or_default().join(".mindscope").join("cortex")
}

/// Returns ~/.mindscope/vault/ — the user's vault (working memory, meetings, etc.)
fn vault_dir() -> PathBuf {
    dirs_next::home_dir().unwrap_or_default().join(".mindscope").join("vault")
}

/// Find the bundled resources directory.
/// In a built app: MindScope.app/Contents/Resources/resources/cortex/
/// In dev: src-tauri/resources/cortex/
fn bundled_resources() -> Option<PathBuf> {
    // Try app bundle path first
    if let Ok(exe) = std::env::current_exe() {
        // exe = .../MindScope.app/Contents/MacOS/mindscope
        if let Some(macos_dir) = exe.parent() {
            if let Some(contents_dir) = macos_dir.parent() {
                let resources = contents_dir.join("Resources").join("resources").join("cortex");
                if resources.exists() {
                    return Some(resources);
                }
                // Tauri 2 sometimes uses a different structure
                let alt = contents_dir.join("Resources").join("cortex");
                if alt.exists() {
                    return Some(alt);
                }
            }
        }
    }

    // Dev fallback
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources").join("cortex");
    if dev.exists() {
        return Some(dev);
    }

    None
}

/// Recursively copy a directory tree, skipping files that already exist at the destination.
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

/// Bootstrap the cortex directory on first run.
/// Copies skills, CLAUDE.md, and seeds working memory if missing.
/// Safe to call every startup — won't overwrite existing user edits.
pub fn bootstrap() {
    let bundled = match bundled_resources() {
        Some(p) => p,
        None => {
            log::warn!("MindScope cortex: bundled resources not found");
            return;
        }
    };

    let cortex = cortex_dir();
    let vault = vault_dir();

    // Create cortex directory structure
    let _ = fs::create_dir_all(&cortex);
    let _ = fs::create_dir_all(&vault);

    // Copy skills → ~/.mindscope/cortex/.claude/skills/
    // (Claude CLI reads skills from .claude/skills/ in cwd)
    let claude_skills = cortex.join(".claude").join("skills");
    let _ = fs::create_dir_all(&claude_skills);
    let src_skills = bundled.join("skills");
    if src_skills.exists() {
        if let Err(e) = copy_tree(&src_skills, &claude_skills) {
            log::warn!("MindScope cortex: failed to copy skills: {}", e);
        }
    }

    // Copy CLAUDE.md → ~/.mindscope/cortex/CLAUDE.md (Claude CLI reads from cwd)
    let src_claude_md = bundled.join("CLAUDE.md");
    let dst_claude_md = cortex.join("CLAUDE.md");
    if src_claude_md.exists() && !dst_claude_md.exists() {
        let _ = fs::copy(&src_claude_md, &dst_claude_md);
    }

    // Also symlink (or copy) CLAUDE.md into vault dir so cortex loop has access
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
            log::info!("MindScope cortex: seeded _working-memory.md");
        }
    }

    // Link skills into vault/.claude/skills so Claude CLI running in vault finds them
    let vault_claude_skills_dir = vault.join(".claude").join("skills");
    let _ = fs::create_dir_all(&vault.join(".claude"));
    if !vault_claude_skills_dir.exists() && src_skills.exists() {
        // Symlink to cortex's skills directory to keep single source of truth
        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink(&claude_skills, &vault_claude_skills_dir);
        }
        #[cfg(not(unix))]
        {
            let _ = copy_tree(&src_skills, &vault_claude_skills_dir);
        }
    }

    log::info!("MindScope cortex: bootstrap complete at {:?}", cortex);
}

/// Run the cortex update loop in a background thread.
/// Every 30 minutes, ask Claude CLI to refresh _working-memory.md.
pub fn start_cortex_loop() {
    if CORTEX_SYNC_RUNNING.load(Ordering::Relaxed) { return; }
    CORTEX_SYNC_RUNNING.store(true, Ordering::Relaxed);

    thread::spawn(|| {
        // Initial delay so we don't hammer Claude right at startup
        thread::sleep(Duration::from_secs(120));

        loop {
            if let Err(e) = run_cortex_update() {
                log::warn!("MindScope cortex loop error: {}", e);
            }
            // Sleep 30 minutes between updates
            thread::sleep(Duration::from_secs(30 * 60));
        }
    });

    log::info!("MindScope cortex: background loop started (30min cadence)");
}

/// One cortex update pass.
/// Calls `claude -p "..."` with cwd set to the vault directory so it picks up
/// CLAUDE.md + .claude/skills/.
pub fn run_cortex_update() -> Result<String, String> {
    if CORTEX_IS_SYNCING.load(Ordering::Relaxed) {
        return Err("cortex update already in progress".into());
    }
    CORTEX_IS_SYNCING.store(true, Ordering::Relaxed);

    let result = do_cortex_update();

    CORTEX_IS_SYNCING.store(false, Ordering::Relaxed);
    result
}

fn do_cortex_update() -> Result<String, String> {
    let vault = vault_dir();
    if !vault.exists() {
        return Err("vault directory does not exist".into());
    }

    // Check if claude CLI is available
    let claude_path = find_claude_cli().ok_or("Claude CLI not found — install via `brew install claude`")?;

    // Gather a compact activity summary to inject into the prompt
    let activity = gather_recent_activity();

    let prompt = format!(
        "You are running in the MindScope vault directory. Your job is to update \
         `_working-memory.md` based on the user's recent activity.\n\n\
         Recent activity (last 2 hours):\n{}\n\n\
         Please:\n\
         1. Read `_working-memory.md`\n\
         2. Update the `## Current Focus` section with what the user appears to be working on\n\
         3. Update the `## Today's Activity` table with the latest apps/durations\n\
         4. If new people are mentioned, update `## Recent People`\n\
         5. Set `## Source Sync Status` last synced time to now\n\
         6. Write the updated file back\n\n\
         Be concise. Don't invent data not present in the activity log. \
         Don't write more than 80 chars per focus line. \
         Preserve any manual edits the user made.",
        activity
    );

    log::info!("MindScope cortex: invoking Claude CLI in {:?}", vault);

    let output = Command::new(&claude_path)
        .args(["-p", &prompt])
        .current_dir(&vault)
        .env("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin")
        .output()
        .map_err(|e| format!("failed to run claude: {}", e))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        log::info!("MindScope cortex: update complete");
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Err(format!("claude exited with status {}: {}", output.status, stderr))
    }
}

/// Locate the Claude CLI binary
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
    // Try PATH via `which`
    if let Ok(out) = Command::new("which").arg("claude").output() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() {
            return Some(PathBuf::from(s));
        }
    }
    None
}

/// Build a compact activity summary for injection into the cortex prompt.
/// Uses last 2 hours of frames + any meeting segments from today.
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

    // App time breakdown
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

    // Collect sample OCR snippets
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

    // Include today's meeting session transcripts (brief)
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

/// Check if cortex is currently syncing (for UI indicators).
pub fn is_syncing() -> bool {
    CORTEX_IS_SYNCING.load(Ordering::Relaxed)
}
