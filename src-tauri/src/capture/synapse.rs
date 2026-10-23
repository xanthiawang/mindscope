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
use super::memory;
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

    // Import any existing cold vault markdown into the structured fact store
    // (read-only on disk, once). Safe to call every startup.
    memory::migrate_if_needed();

    log::info!("MindScope synapse: bootstrap complete at {:?}", synapse);
}

/// Run the synapse update loop in a background thread.
/// Every 30 minutes, ask Claude CLI to refresh _working-memory.md.
pub fn start_synapse_loop() {
    if SYNAPSE_LOOP_RUNNING.load(Ordering::Relaxed) { return; }
    SYNAPSE_LOOP_RUNNING.store(true, Ordering::Relaxed);

    thread::spawn(|| {
        thread::sleep(Duration::from_secs(120));
        let mut deep_pass_day = String::new();
        loop {
            if has_new_activity() {
                if let Err(e) = run_synapse_update() {
                    log::warn!("MindScope synapse loop error: {}", e);
                }
            } else {
                log::info!("MindScope synapse: no new activity since last pass, skipping");
            }
            // Once per local day, run the heavier "dreaming" deep pass: merge /
            // dedup / decay the fact store, re-render cold files, enforce the
            // byte ceiling. Only this pass rewrites long-term files.
            let today = super::timeutil::today();
            if deep_pass_day != today {
                deep_pass_day = today;
                memory::deep_pass();
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

// Size enforcement now lives in `memory::compress_hot_warm`, which drops whole
// oldest entries (never cuts mid-item). The 4000/8000-char caps are still named
// in the prompt below so Claude keeps its output tight.

fn do_synapse_update() -> Result<String, String> {
    let vault = vault_dir();
    if !vault.exists() {
        return Err("vault directory does not exist".into());
    }

    if !claude_cli::available() {
        return Err("Claude CLI not found — install `claude` (npm i -g @anthropic-ai/claude-code) or set claude_path in settings.json".into());
    }

    // Pre-flight: entry-aware compression (drops whole oldest entries, never
    // mid-item) BEFORE asking Claude to touch anything.
    memory::compress_hot_warm(&vault);

    let activity = gather_recent_activity();
    let today = super::timeutil::today();
    let activity_sources = vec![format!("day:{}", today)];

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
         Also extract durable COLD-TIER facts worth remembering about specific \
         people, projects or meetings seen in the activity (not transient app \
         usage). Each fact: one atomic statement, a subject name, and kind.\n\n\
         OUTPUT FORMAT — respond with exactly these fenced blocks, nothing else:\n\n\
         ```working-memory\n\
         (entire updated _working-memory.md content here)\n\
         ```\n\n\
         ```warm-memory\n\
         (entire updated _warm-memory.md content here)\n\
         ```\n\n\
         ```facts-json\n\
         [{{\"kind\":\"person|project|meeting\",\"subject\":\"Name\",\"text\":\"one fact\"}}]\n\
         ```",
        wm_content, warm_content, activity
    );

    // Tool-free call: file contents are in the prompt; updated files come back
    // as fenced text (no Read/Edit tools → no conflict with a running Claude Code).
    let mut opts = claude_cli::CallOpts::background();
    opts.cwd = Some(vault.clone());
    let stdout = claude_cli::run("synapse", &prompt, opts)?;

    // Apply only the managed sections (section-scoped splice), preserving
    // frontmatter, comments, unmanaged sections and the user-owned zone verbatim.
    apply_fenced_output(&vault, &stdout, &wm_content, &warm_content)?;

    // Consolidate extracted cold-tier facts into the structured store
    // (mem0-style ADD/UPDATE/DELETE/NONE). The markdown cold files are rendered
    // from the store by the daily deep pass, not here.
    if let Some(facts_block) = extract_fenced_block(&stdout, "facts-json") {
        let candidates = memory::parse_candidates_json(&facts_block, &activity_sources);
        if !candidates.is_empty() {
            let r = memory::consolidate_candidates(&candidates);
            log::info!(
                "MindScope synapse: consolidated {} candidate fact(s) → {} add, {} update, {} delete, {} none",
                candidates.len(), r.added, r.updated, r.deleted, r.noop
            );
        }
    }

    // Post-flight: entry-aware compression (idempotent safety net).
    memory::compress_hot_warm(&vault);
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

/// Managed sections per hot/warm file. Only these are overwritten from Claude's
/// output; everything else (frontmatter, comments, unmanaged sections, the
/// user-owned zone) is copied from the ORIGINAL verbatim.
const WM_MANAGED: &[&str] = &["Current Focus", "Today's Activity", "Recent People", "Source Sync Status"];
const WARM_MANAGED: &[&str] =
    &["Active Follow-Ups", "Project Momentum", "Collaborator State", "Needs Triage", "Recent Decisions", "Decision Patterns"];

/// Apply Claude's fenced output by SECTION-SCOPED SPLICE rather than whole-file
/// replacement: for each managed `## Heading`, the original file's body is
/// replaced with Claude's body for that heading; any heading Claude omitted, and
/// every unmanaged line, is kept from the original. This makes the edit derive
/// from per-section decisions and structurally cannot clobber the user zone.
fn apply_fenced_output(
    vault: &Path,
    response: &str,
    original_wm: &str,
    original_warm: &str,
) -> Result<(), String> {
    let mut applied = 0;

    if let Some(new_wm) = extract_fenced_block(response, "working-memory") {
        let spliced = splice_managed_sections(original_wm, &new_wm, WM_MANAGED);
        let path = vault.join("_working-memory.md");
        fs::write(&path, &spliced).map_err(|e| format!("failed to write _working-memory.md: {}", e))?;
        log::info!("MindScope synapse: spliced _working-memory.md ({} → {} chars)",
                   original_wm.chars().count(), spliced.chars().count());
        applied += 1;
    }

    if let Some(new_warm) = extract_fenced_block(response, "warm-memory") {
        let spliced = splice_managed_sections(original_warm, &new_warm, WARM_MANAGED);
        let path = vault.join("_warm-memory.md");
        fs::write(&path, &spliced).map_err(|e| format!("failed to write _warm-memory.md: {}", e))?;
        log::info!("MindScope synapse: spliced _warm-memory.md ({} → {} chars)",
                   original_warm.chars().count(), spliced.chars().count());
        applied += 1;
    }

    if applied == 0 {
        log::warn!("MindScope synapse: no fenced blocks found in response, no files updated");
    }
    Ok(())
}

/// The body lines of a `## heading` section in `content` (exclusive of the
/// heading line, up to the next `## ` or EOF). None if the heading is absent.
fn section_body(content: &str, heading: &str) -> Option<String> {
    let mut lines = content.lines().peekable();
    while let Some(line) = lines.next() {
        if line.strip_prefix("## ").map(|h| h.trim() == heading).unwrap_or(false) {
            let mut body = Vec::new();
            while let Some(peek) = lines.peek() {
                if peek.starts_with("## ") {
                    break;
                }
                body.push(*peek);
                lines.next();
            }
            return Some(body.join("\n"));
        }
    }
    None
}

/// Rebuild `original`, replacing each managed section's body with the body of
/// the same section from `updated` (when present). All other content is kept
/// from `original` verbatim.
fn splice_managed_sections(original: &str, updated: &str, managed: &[&str]) -> String {
    let mut out: Vec<String> = Vec::new();
    let orig_lines: Vec<&str> = original.lines().collect();
    let mut i = 0;
    while i < orig_lines.len() {
        let line = orig_lines[i];
        if let Some(h) = line.strip_prefix("## ") {
            let h = h.trim();
            if managed.contains(&h) {
                if let Some(body) = section_body(updated, h) {
                    out.push(line.to_string());
                    let trimmed = body.trim_end_matches('\n');
                    out.push(trimmed.to_string());
                    // Skip the original body for this section.
                    i += 1;
                    while i < orig_lines.len() && !orig_lines[i].starts_with("## ") {
                        i += 1;
                    }
                    continue;
                }
            }
        }
        out.push(line.to_string());
        i += 1;
    }
    let mut result = out.join("\n");
    if original.ends_with('\n') && !result.ends_with('\n') {
        result.push('\n');
    }
    result
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splice_replaces_only_managed_sections() {
        let original = "---\nid: wm\nupdated: 0\n---\n\n\
            ## Current Focus\n\n**Primary**: old focus\n\n\
            ## Unmanaged\n\nkeep me\n\n\
            ## User Notes\n\n- my private note\n";
        let updated = "## Current Focus\n\n**Primary**: NEW focus\n\n\
            ## User Notes\n\n- CLAUDE TRIED TO CLOBBER THIS\n";
        let out = splice_managed_sections(original, updated, &["Current Focus"]);
        assert!(out.contains("NEW focus"), "managed section updated");
        assert!(!out.contains("old focus"));
        assert!(out.contains("## Unmanaged\n\nkeep me"), "unmanaged section preserved");
        assert!(out.contains("- my private note"), "user zone preserved");
        assert!(!out.contains("CLOBBER"), "user zone never taken from Claude output");
        assert!(out.contains("id: wm"), "frontmatter preserved");
    }

    #[test]
    fn splice_keeps_section_when_update_omits_it() {
        let original = "## Today's Activity\n\n| a |\n\n## Recent People\n\n- Amy\n";
        let updated = "## Today's Activity\n\n| b |\n"; // no Recent People
        let out = splice_managed_sections(original, updated, &["Today's Activity", "Recent People"]);
        assert!(out.contains("| b |"), "present section updated");
        assert!(out.contains("- Amy"), "omitted section kept from original");
    }

    #[test]
    fn section_body_extracts_until_next_heading() {
        let c = "## A\n\nbody a\nline 2\n\n## B\n\nbody b\n";
        assert_eq!(section_body(c, "A").unwrap().trim(), "body a\nline 2");
        assert_eq!(section_body(c, "B").unwrap().trim(), "body b");
        assert!(section_body(c, "Missing").is_none());
    }
}
