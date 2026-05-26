//! Vault auto-sync — connects screen recording data to the MindScope knowledge vault.
//!
//! Works in tandem with the Synapse module (`synapse.rs`): this file handles
//! incremental updates from raw data (frames, audio segments), while Synapse
//! runs the AI loop that enriches them with semantic context.
//!
//! Functions:
//! - generate_daily_brief() — reads vault state + recent frames for a quick summary
//! - generate_daily_journal(date) — auto-generates daily.journal.YYYY.MM.DD.md
//! - update_working_memory() — patches "Today's Activity" in _working-memory.md
//! - refresh_people_contacts() — updates "Last contact" in user.*.md files
//! - start_vault_sync_loop() — background thread for periodic sync

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use super::audio;
use super::db;
use super::recorder;

// Guard: only one sync loop thread at a time
static SYNC_RUNNING: AtomicBool = AtomicBool::new(false);

/// Find the MindScope vault directory (~/.mindscope/vault/).
/// Created by synapse::bootstrap() on first launch.
fn vault_dir() -> Option<PathBuf> {
    let home = dirs_next::home_dir()?;
    let ms = home.join(".mindscope").join("vault");
    if ms.exists() {
        return Some(ms);
    }
    None
}

/// Today's date as "YYYY-MM-DD"
fn today_date() -> String {
    recorder::timestamp_now()[..10].to_string()
}

/// Current hour (0-23) from timestamp
fn current_hour() -> u64 {
    let ts = recorder::timestamp_now();
    // Format: "YYYY-MM-DDThh:mm:ss"
    ts[11..13].parse().unwrap_or(0)
}

// ─── 1. Daily brief ──────────────────────────────────────────────

/// Read recent screen activity and vault data, return a structured brief.
/// Sections: Today (app time), Now (current activity), Meetings, Focus/Tasks (if vault exists)
pub fn generate_daily_brief() -> String {
    let strip_md = |s: &str| -> String {
        s.replace("**", "").replace("[[", "").replace("]]", "").replace("|", " - ")
    };

    let mut brief = String::new();
    let ms_vault = dirs_next::home_dir().unwrap_or_default().join(".mindscope").join("vault");

    // ─── Section 1: Now — currently active app and latest context ───
    let (current_app, current_window, _, _) = super::screenshot::get_active_window_info();
    if !current_app.is_empty() && current_app != "Unknown" {
        brief.push_str(&format!("## Now\n{}", current_app));
        if !current_window.is_empty() && current_window != current_app {
            brief.push_str(&format!(" — {}", current_window));
        }
        brief.push_str("\n\n");
    }

    // ─── Section 2: Today — app time breakdown (top 5 apps) ───
    let today_apps = app_time_breakdown(24);
    if !today_apps.is_empty() {
        brief.push_str("## Today\n");
        for (app, minutes) in today_apps.iter().take(5) {
            brief.push_str(&format!("• {} — {}m\n", app, minutes));
        }
        brief.push_str("\n");
    }

    // ─── Section 3: Meetings — recent meetings from vault (last 3) ───
    let recent_meetings = list_recent_meetings(&ms_vault, 3);
    if !recent_meetings.is_empty() {
        brief.push_str("## Recent Meetings\n");
        for m in recent_meetings {
            brief.push_str(&format!("• {}\n", strip_md(&m)));
        }
        brief.push_str("\n");
    }

    // ─── Section 4: Focus — auto-inferred from recent activity ───
    // 1. If _working-memory.md exists (seeded by synapse), read it
    // 2. Otherwise, use Claude CLI to infer Focus from recent screen activity
    let wm_path = ms_vault.join("_working-memory.md");
    let mut focus_written = false;
    if wm_path.exists() {
        if let Ok(content) = fs::read_to_string(&wm_path) {
            let focus = extract_section(&content, "Current Focus").unwrap_or_default();
            let tasks = extract_section(&content, "Live Tasks").unwrap_or_default();
            if !focus.is_empty() {
                let short: String = focus.lines().take(2).collect::<Vec<_>>().join("\n");
                brief.push_str(&format!("## Focus\n{}\n\n", strip_md(&short)));
                focus_written = true;
            }
            if !tasks.is_empty() {
                let short: String = tasks.lines().filter(|l| l.starts_with("- ")).take(4)
                    .collect::<Vec<_>>().join("\n");
                brief.push_str(&format!("## Tasks\n{}\n\n", strip_md(&short)));
            }
        }
    }

    // Fallback: infer Focus from recent OCR + meeting transcripts via Claude
    if !focus_written {
        if let Some(inferred) = infer_focus_from_activity() {
            brief.push_str(&format!("## Focus\n{}\n\n", strip_md(&inferred)));
        }
    }

    // ─── Section 5: Stats — total capture stats for today ───
    let stats = today_stats();
    if !stats.is_empty() {
        brief.push_str(&format!("## Stats\n{}\n", stats));
    }

    if brief.is_empty() {
        brief = "No activity recorded yet. Start using your Mac and check back in a few minutes.".to_string();
    }
    brief
}

/// Get app time breakdown for the last N hours.
/// Returns Vec<(app_name, minutes)> sorted by time desc.
fn app_time_breakdown(hours: u64) -> Vec<(String, u64)> {
    let date = today_date();
    let frames = db::get_frames_for_date(&date).unwrap_or_default();
    if frames.is_empty() { return Vec::new(); }

    let now_us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i64;
    let cutoff = now_us - (hours as i64 * 3600 * 1_000_000);

    let mut app_counts: HashMap<String, u64> = HashMap::new();
    for f in frames.iter().filter(|f| f.timestamp >= cutoff) {
        if f.app_name.is_empty() || f.app_name == "Unknown" { continue; }
        *app_counts.entry(f.app_name.clone()).or_insert(0) += 1;
    }

    // Each frame = capture_interval seconds (default 3s)
    let mut sorted: Vec<(String, u64)> = app_counts.into_iter()
        .map(|(app, count)| (app, (count * 3) / 60)) // convert to minutes
        .filter(|(_, m)| *m >= 1) // skip sub-minute entries
        .collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    sorted
}

/// List recent meeting titles from vault (last N meetings, any day).
/// Returns formatted "Date Time — title" strings.
fn list_recent_meetings(vault: &Path, limit: usize) -> Vec<String> {
    if !vault.exists() { return Vec::new(); }
    let mut meetings: Vec<(String, String)> = Vec::new(); // (sort_key, display)

    if let Ok(entries) = fs::read_dir(vault) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("meet.") || !name.ends_with(".md") { continue; }
            if let Ok(content) = fs::read_to_string(entry.path()) {
                // Parse frontmatter: title, date, app, duration
                let title = content.lines().find(|l| l.starts_with("title:"))
                    .map(|l| l[6..].trim().to_string())
                    .unwrap_or_default();
                let date = content.lines().find(|l| l.starts_with("date:"))
                    .map(|l| l[5..].trim().to_string())
                    .unwrap_or_default();
                let app = content.lines().find(|l| l.starts_with("app:"))
                    .map(|l| l[4..].trim().to_string())
                    .unwrap_or_default();

                // Extract first real content line from Summary section
                let summary = content.split("## Summary").nth(1)
                    .and_then(|s| s.lines().find(|l| {
                        let t = l.trim();
                        !t.is_empty() && !t.starts_with("#") && !t.starts_with("-")
                    }))
                    .map(|l| l.trim().chars().take(60).collect::<String>())
                    .unwrap_or_default();

                // Build display string: "Apr 10 · Tencent Meeting · first summary line"
                let mut parts: Vec<String> = Vec::new();
                if !date.is_empty() { parts.push(format_date_short(&date)); }
                if !app.is_empty() { parts.push(app); }
                let prefix = parts.join(" · ");

                let display = if !title.is_empty() {
                    format!("{} — {}", prefix, title)
                } else if !summary.is_empty() {
                    format!("{} — {}", prefix, summary)
                } else if !prefix.is_empty() {
                    prefix
                } else {
                    // Last resort: filename
                    name.trim_start_matches("meet.").trim_end_matches(".md").to_string()
                };
                meetings.push((name, display));
            }
        }
    }

    // Sort by filename desc (newest first)
    meetings.sort_by(|a, b| b.0.cmp(&a.0));
    meetings.into_iter().take(limit).map(|(_, d)| d).collect()
}

/// Format "2026-04-10" → "Apr 10" (short display)
fn format_date_short(date: &str) -> String {
    if date.len() < 10 { return date.to_string(); }
    let months = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
    let month: usize = date[5..7].parse().unwrap_or(1);
    let day: u32 = date[8..10].parse().unwrap_or(1);
    let month_name = months.get(month.saturating_sub(1)).copied().unwrap_or("");
    format!("{} {}", month_name, day)
}

/// Today's capture stats: frames captured, active apps, time span.
fn today_stats() -> String {
    let date = today_date();
    let frames = db::get_frames_for_date(&date).unwrap_or_default();
    if frames.is_empty() { return String::new(); }

    let app_set: std::collections::HashSet<&str> = frames.iter()
        .filter(|f| !f.app_name.is_empty() && f.app_name != "Unknown")
        .map(|f| f.app_name.as_str())
        .collect();

    let total_frames = frames.len();
    let total_apps = app_set.len();
    // Active span = first to last frame, in minutes
    let span_min = if let (Some(first), Some(last)) = (frames.first(), frames.last()) {
        (last.timestamp - first.timestamp) / 1_000_000 / 60
    } else { 0 };

    format!("{} frames · {} apps · {}m active", total_frames, total_apps, span_min)
}

/// Infer the user's current focus from the last 2 hours of screen activity
/// by asking Claude CLI to summarize recent OCR text + app usage.
/// Cached to ~/.mindscope/data/focus_cache.json for 15 minutes to avoid
/// hammering the CLI on every Brief open.
fn infer_focus_from_activity() -> Option<String> {
    use std::io::Write;

    let cache_path = db::data_dir().join("focus_cache.txt");
    let cache_ttl_secs: u64 = 15 * 60; // 15 min

    // Check cache first
    if let Ok(meta) = fs::metadata(&cache_path) {
        if let Ok(modified) = meta.modified() {
            if let Ok(age) = modified.elapsed() {
                if age.as_secs() < cache_ttl_secs {
                    if let Ok(cached) = fs::read_to_string(&cache_path) {
                        let trimmed = cached.trim();
                        if !trimmed.is_empty() { return Some(trimmed.to_string()); }
                    }
                }
            }
        }
    }

    // Gather the last 2 hours of screen activity
    let date = today_date();
    let frames = db::get_frames_for_date(&date).unwrap_or_default();
    if frames.is_empty() { return None; }

    let now_us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i64;
    let cutoff = now_us - (2 * 3600 * 1_000_000);

    let recent: Vec<_> = frames.iter().filter(|f| f.timestamp >= cutoff).collect();
    if recent.len() < 3 { return None; }

    // Build a compact summary: app transitions + sample OCR snippets
    let mut sample_text = String::new();
    let mut last_app = String::new();
    let mut ocr_samples: Vec<&str> = Vec::new();
    for f in &recent {
        if f.app_name != last_app && !f.app_name.is_empty() {
            sample_text.push_str(&format!("[{}] ", f.app_name));
            last_app = f.app_name.clone();
        }
        // Sample OCR text — pick non-empty, reasonable length
        if !f.ocr_text.is_empty() && f.ocr_text.len() > 20 {
            ocr_samples.push(&f.ocr_text);
        }
    }

    // Take first 5 OCR samples, truncated
    let ocr_snippet: String = ocr_samples.iter().take(5)
        .map(|t| {
            // Strip the ---REGIONS--- block if present
            let clean = t.split("---REGIONS---").next().unwrap_or(t);
            clean.chars().take(200).collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" | ");

    let prompt = format!(
        "Based on the following 2-hour screen activity, write a ONE-LINE Focus summary \
         (max 80 chars) describing what the user is currently working on. No prefixes, \
         no markdown, no quotes — just the sentence.\n\n\
         App transitions: {}\n\n\
         Recent screen text samples: {}\n\n\
         Focus:",
        sample_text.chars().take(500).collect::<String>(),
        ocr_snippet.chars().take(1500).collect::<String>(),
    );

    let response = call_claude(&prompt)?;
    let focus_line = response.lines().next().unwrap_or("").trim().to_string();
    if focus_line.is_empty() || focus_line.len() > 200 { return None; }

    // Cache it
    if let Ok(mut f) = fs::File::create(&cache_path) {
        let _ = f.write_all(focus_line.as_bytes());
    }

    Some(focus_line)
}

// ─── 2. Daily journal ────────────────────────────────────────────

/// Auto-generate daily.journal.YYYY.MM.DD.md from today's data.
pub fn generate_daily_journal(date: &str) {
    let vdir = match vault_dir() {
        Some(d) => d,
        None => {
            log::warn!("MindScope vault_sync: no vault directory found");
            return;
        }
    };

    let frames = db::get_frames_for_date(date).unwrap_or_default();
    if frames.is_empty() {
        log::info!("MindScope vault_sync: no frames for {}, skipping journal", date);
        return;
    }

    // Group by app, compute time per app
    let app_times = compute_app_times(&frames);
    let mut app_summary = String::new();
    for (app, mins) in &app_times {
        app_summary.push_str(&format!("- {}: {}m\n", app, mins));
    }

    // Collect meeting notes for the date
    let date_dots = date.replace('-', ".");
    let mut meetings_text = String::new();
    let pattern = vdir.join(format!("meet.{}*.md", date_dots));
    if let Ok(entries) = glob::glob(&pattern.to_string_lossy()) {
        for entry in entries.flatten() {
            if let Ok(content) = fs::read_to_string(&entry) {
                let body = content.split("---").nth(2).unwrap_or("").trim();
                meetings_text.push_str(&format!("\n{}\n", &body[..body.len().min(500)]));
            }
        }
    }

    // Collect audio transcripts
    let audio_segs = audio::load_audio_segments(date);
    let mut transcript = String::new();
    for seg in &audio_segs {
        if !seg.transcript.is_empty() {
            transcript.push_str(&seg.transcript);
            transcript.push(' ');
        }
    }

    // Build prompt for Claude
    let prompt = format!(
        "Write a concise daily journal entry for {}. No meta-commentary.\n\
         App usage:\n{}\n\
         Meeting notes:\n{}\n\
         Audio transcripts:\n{}\n\n\
         Format: Start with a 2-sentence summary. Then ## Highlights (3-5 bullets). \
         Then ## Time Breakdown (restate app usage). Keep it short and factual.",
        date,
        &app_summary[..app_summary.len().min(1000)],
        &meetings_text[..meetings_text.len().min(1500)],
        &transcript[..transcript.len().min(1500)]
    );

    let body = call_claude(&prompt).unwrap_or_else(|| {
        // Fallback: raw data
        format!(
            "## Summary\nActivity recorded on {}.\n\n## App Usage\n{}\n\n## Notes\n{}",
            date,
            app_summary,
            if meetings_text.is_empty() { "(none)".to_string() } else { meetings_text }
        )
    });

    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let content = format!(
        "---\ntitle: Journal {}\ndate: {}\nupdated: {}\n---\n\n{}",
        date, date, epoch, body
    );

    let filename = format!("daily.journal.{}.md", date_dots);
    let filepath = vdir.join(&filename);
    match fs::write(&filepath, &content) {
        Ok(_) => log::info!("MindScope vault_sync: journal saved to {:?}", filepath),
        Err(e) => log::error!("MindScope vault_sync: journal write failed: {}", e),
    }
}

// ─── 3. Update working memory ────────────────────────────────────

/// Update the "Today's Activity" section in _working-memory.md.
pub fn update_working_memory() {
    let vdir = match vault_dir() {
        Some(d) => d,
        None => return,
    };
    let wm_path = vdir.join("_working-memory.md");
    if !wm_path.exists() {
        return;
    }

    let content = match fs::read_to_string(&wm_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let date = today_date();
    let frames = db::get_frames_for_date(&date).unwrap_or_default();
    let app_times = compute_app_times(&frames);

    // Build the replacement section
    let mut table = String::from("## Today's Activity\n\n| Time | App | Duration |\n|------|-----|----------|\n");
    for (app, mins) in &app_times {
        table.push_str(&format!("| today | {} | {}m |\n", app, mins));
    }

    // Replace existing section or append
    let new_content = if let Some(start) = content.find("## Today's Activity") {
        let end = content[start..]
            .find("\n## ")
            .map(|i| start + i)
            .unwrap_or(content.len());
        // Check if "## Today's Activity" is followed by another section
        let end = if start + 1 < content.len() {
            content[start + 1..]
                .find("\n## ")
                .map(|i| start + 1 + i + 1)
                .unwrap_or(content.len())
        } else {
            content.len()
        };
        format!("{}{}\n{}", &content[..start], table, &content[end..])
    } else {
        // No existing section — don't add one, just skip
        return;
    };

    if let Err(e) = fs::write(&wm_path, &new_content) {
        log::error!("MindScope vault_sync: failed to update working memory: {}", e);
    }
}

// ─── 4. Refresh people contacts ──────────────────────────────────

/// Scan today's frames for person names; update "Last contact" in their user.*.md files.
pub fn refresh_people_contacts() {
    let vdir = match vault_dir() {
        Some(d) => d,
        None => return,
    };

    let date = today_date();
    let frames = db::get_frames_for_date(&date).unwrap_or_default();
    if frames.is_empty() {
        return;
    }

    // Collect all window_name + ocr_text into one searchable string
    let mut haystack = String::new();
    for f in &frames {
        haystack.push_str(&f.window_name);
        haystack.push(' ');
        let ocr = f.ocr_text.split("\n---REGIONS---\n").next().unwrap_or("");
        haystack.push_str(ocr);
        haystack.push(' ');
    }
    let haystack_lower = haystack.to_lowercase();

    // Scan user.*.md files
    let pattern = vdir.join("user.*.md").to_string_lossy().to_string();
    for entry in glob::glob(&pattern).unwrap_or_else(|_| glob::glob("").unwrap()) {
        let path = match entry {
            Ok(p) => p,
            Err(_) => continue,
        };
        let filename = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
        if filename == "user" {
            continue;
        }

        // Get person name from frontmatter or filename
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let name = extract_frontmatter_field(&content, "title")
            .unwrap_or_else(|| filename.strip_prefix("user.").unwrap_or(&filename).replace('-', " "));

        if name.len() < 2 {
            continue;
        }

        // Check if name appears in today's activity
        if haystack_lower.contains(&name.to_lowercase()) {
            update_last_contact(&path, &content, &date);
        }
    }
}

/// Update "Last contact" field in a user markdown file.
fn update_last_contact(path: &Path, content: &str, date: &str) {
    // Try to find and replace the "Last contact" line
    let mut updated = false;
    let new_content: String = content
        .lines()
        .map(|line| {
            if line.to_lowercase().contains("last contact") && line.contains(':') {
                updated = true;
                let prefix = &line[..line.find(':').unwrap() + 1];
                format!("{} {}", prefix, date)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    if updated {
        if let Err(e) = fs::write(path, &new_content) {
            log::error!("MindScope vault_sync: failed to update {:?}: {}", path, e);
        }
    }
}

// ─── 5. Background sync loop ────────────────────────────────────

/// Start the vault sync loop in a background thread.
/// - Every 60 minutes: update_working_memory() + refresh_people_contacts()
/// - At 23:00: generate_daily_journal(today)
pub fn start_vault_sync_loop() {
    if SYNC_RUNNING.swap(true, Ordering::SeqCst) {
        log::warn!("MindScope vault_sync: sync loop already running");
        return;
    }

    thread::spawn(|| {
        log::info!("MindScope vault_sync: sync loop started");
        let mut last_hourly = 0u64;
        let mut journal_done_today = String::new();

        loop {
            thread::sleep(Duration::from_secs(60)); // Check every minute

            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            // Hourly sync (every 3600 seconds)
            if now_secs - last_hourly >= 3600 {
                last_hourly = now_secs;
                update_working_memory();
                refresh_people_contacts();
                log::info!("MindScope vault_sync: hourly sync complete");
            }

            // Journal at 23:00
            let hour = current_hour();
            let date = today_date();
            if hour == 23 && journal_done_today != date {
                journal_done_today = date.clone();
                generate_daily_journal(&date);
                log::info!("MindScope vault_sync: daily journal generated for {}", date);
            }
        }
    });
}

// ─── Helpers ─────────────────────────────────────────────────────

/// Extract a markdown section by heading (e.g., "## Current Focus" -> content until next ##)
fn extract_section(content: &str, heading: &str) -> Option<String> {
    let marker = format!("## {}", heading);
    let start = content.find(&marker)?;
    let body_start = start + marker.len();
    let body = &content[body_start..];

    // Find next ## heading
    let end = body.find("\n## ").unwrap_or(body.len());
    let section = body[..end].trim().to_string();
    if section.is_empty() {
        None
    } else {
        Some(section)
    }
}

/// Extract a frontmatter field value
fn extract_frontmatter_field(content: &str, key: &str) -> Option<String> {
    if !content.starts_with("---") {
        return None;
    }
    let end = content[3..].find("---")?;
    let yaml = &content[3..3 + end];
    for line in yaml.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim() == key {
                return Some(v.trim().trim_matches('\'').trim_matches('"').to_string());
            }
        }
    }
    None
}

/// Compute time spent per app from a list of frames.
/// Returns vec of (app_name, minutes) sorted by minutes descending.
fn compute_app_times(frames: &[db::FrameRow]) -> Vec<(String, u64)> {
    let mut app_counts: HashMap<String, u64> = HashMap::new();
    for f in frames {
        if f.app_name.is_empty() || f.is_idle() {
            continue;
        }
        *app_counts.entry(f.app_name.clone()).or_insert(0) += 1;
    }

    // Each frame is ~2 seconds apart (capture interval), convert to minutes
    let mut sorted: Vec<(String, u64)> = app_counts
        .into_iter()
        .map(|(app, count)| (app, count * 2 / 60)) // 2s per frame -> minutes
        .filter(|(_, mins)| *mins > 0)
        .collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    sorted
}

/// Build a short summary of recent app activity (last N hours).
fn recent_app_summary(hours: u64) -> String {
    let date = today_date();
    let frames = db::get_frames_for_date(&date).unwrap_or_default();
    if frames.is_empty() {
        return String::new();
    }

    let now_us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i64;
    let cutoff = now_us - (hours as i64 * 3600 * 1_000_000);

    let recent: Vec<_> = frames.iter().filter(|f| f.timestamp >= cutoff).collect();
    let mut app_counts: HashMap<String, u64> = HashMap::new();
    for f in &recent {
        if f.app_name.is_empty() {
            continue;
        }
        *app_counts.entry(f.app_name.clone()).or_insert(0) += 1;
    }

    let mut sorted: Vec<_> = app_counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));

    sorted
        .iter()
        .take(5)
        .map(|(app, count)| format!("{} ({}m)", app, count * 2 / 60))
        .collect::<Vec<_>>()
        .join(", ")
}

fn normalize_line_endings(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// Call Claude CLI with a prompt. Runs inside the MindScope vault so Claude
/// picks up the bundled CLAUDE.md + .claude/skills/ from synapse bootstrap.
/// Routes through Haiku — vault_sync is background summarization, not reasoning.
fn call_claude(prompt: &str) -> Option<String> {
    let vault = dirs_next::home_dir()?.join(".mindscope").join("vault");
    let cwd = if vault.exists() { vault } else { dirs_next::home_dir()? };

    let claude_path = super::platform::find_claude_cli()
        .unwrap_or_else(|| "claude".to_string());

    let mut cmd = std::process::Command::new(&claude_path);
    cmd.args(["-p", prompt, "--model", "claude-haiku-4-5"])
       .current_dir(&cwd);
    #[cfg(not(target_os = "windows"))]
    cmd.env("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin");
    let output = cmd.output().ok()?;

    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if text.is_empty() { None } else { Some(normalize_line_endings(&text)) }
    } else {
        None
    }
}

/// Helper trait to check idle on FrameRow (no is_idle field exposed, check app_name)
trait FrameIdle {
    fn is_idle(&self) -> bool;
}

impl FrameIdle for db::FrameRow {
    fn is_idle(&self) -> bool {
        self.app_name == "idle" || self.app_name.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_line_endings;

    #[test]
    fn test_normalize_crlf() {
        assert_eq!(normalize_line_endings("a\r\nb\r\nc"), "a\nb\nc");
    }

    #[test]
    fn test_normalize_lf_unchanged() {
        assert_eq!(normalize_line_endings("a\nb\nc"), "a\nb\nc");
    }

    #[test]
    fn test_normalize_mixed() {
        assert_eq!(normalize_line_endings("a\r\nb\nc\r\n"), "a\nb\nc\n");
    }

    #[test]
    fn test_normalize_bare_cr() {
        assert_eq!(normalize_line_endings("a\rb\rc"), "a\nb\nc");
    }
}
