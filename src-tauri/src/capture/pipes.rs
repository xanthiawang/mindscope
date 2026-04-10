//! Pipeline/Automation system — simplified version of Screenpipe's pipe architecture
//! Pipes are YAML-defined automations that query screen data and run through Claude CLI

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn pipes_dir() -> PathBuf {
    let dir = dirs_next::home_dir().unwrap_or_default()
        .join(".mindscope").join("pipes");
    let _ = fs::create_dir_all(&dir);
    dir
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipeConfig {
    pub name: String,
    #[serde(default = "default_schedule")]
    pub schedule: String,        // "manual", "every 30m", "daily 18:00"
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub prompt: String,           // LLM prompt template
    #[serde(default = "default_output")]
    pub output: String,           // "clipboard", "file", "notification"
    #[serde(default)]
    pub context_query: Option<String>,  // search query for context gathering
    #[serde(default)]
    pub context_hours: Option<u32>,     // how many hours of history to include (default 24)
}

fn default_schedule() -> String { "manual".into() }
fn default_output() -> String { "clipboard".into() }
fn default_true() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipeInfo {
    pub id: String,
    pub config: PipeConfig,
    pub last_run: Option<String>,
    pub last_result: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipeResult {
    pub pipe_id: String,
    pub timestamp: String,
    pub output: String,
    pub success: bool,
}

/// List all pipes from ~/.mindscope/pipes/
pub fn list_pipes() -> Vec<PipeInfo> {
    let dir = pipes_dir();
    let mut pipes = Vec::new();

    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() { continue; }

            let id = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            let config_path = path.join("pipe.yaml");
            if !config_path.exists() { continue; }

            if let Ok(content) = fs::read_to_string(&config_path) {
                if let Ok(config) = serde_yaml::from_str::<PipeConfig>(&content) {
                    // Load last run result
                    let result_path = path.join("last_result.json");
                    let last_result = fs::read_to_string(&result_path).ok()
                        .and_then(|c| serde_json::from_str::<PipeResult>(&c).ok());

                    pipes.push(PipeInfo {
                        id,
                        config,
                        last_run: last_result.as_ref().map(|r| r.timestamp.clone()),
                        last_result: last_result.map(|r| r.output),
                    });
                }
            }
        }
    }

    pipes.sort_by(|a, b| a.id.cmp(&b.id));
    pipes
}

/// Create a new pipe
pub fn create_pipe(id: &str, config: PipeConfig) -> Result<(), String> {
    let pipe_dir = pipes_dir().join(id);
    fs::create_dir_all(&pipe_dir).map_err(|e| format!("Failed to create pipe dir: {}", e))?;

    let yaml = serde_yaml::to_string(&config).map_err(|e| format!("YAML error: {}", e))?;
    fs::write(pipe_dir.join("pipe.yaml"), yaml).map_err(|e| format!("Write error: {}", e))?;

    log::info!("MindScope: Created pipe '{}'", id);
    Ok(())
}

/// Enable/disable a pipe
pub fn set_pipe_enabled(id: &str, enabled: bool) -> Result<(), String> {
    let config_path = pipes_dir().join(id).join("pipe.yaml");
    let content = fs::read_to_string(&config_path).map_err(|e| e.to_string())?;
    let mut config: PipeConfig = serde_yaml::from_str(&content).map_err(|e| e.to_string())?;
    config.enabled = enabled;
    let yaml = serde_yaml::to_string(&config).map_err(|e| e.to_string())?;
    fs::write(&config_path, yaml).map_err(|e| e.to_string())?;
    Ok(())
}

/// Run a pipe: gather context from DB, build prompt, call Claude CLI
pub fn run_pipe(id: &str) -> Result<PipeResult, String> {
    let config_path = pipes_dir().join(id).join("pipe.yaml");
    let content = fs::read_to_string(&config_path).map_err(|e| e.to_string())?;
    let config: PipeConfig = serde_yaml::from_str(&content).map_err(|e| e.to_string())?;

    // Gather context from screen history
    let hours = config.context_hours.unwrap_or(24);
    let context = gather_context(&config.context_query, hours);

    // Build prompt with context
    let full_prompt = if context.is_empty() {
        config.prompt.clone()
    } else {
        format!("Based on this screen activity history:\n\n{}\n\n{}", context, config.prompt)
    };

    // Call Claude CLI
    let output = call_claude(&full_prompt)?;

    // Route output
    match config.output.as_str() {
        "clipboard" => {
            #[cfg(target_os = "macos")]
            {
                let _ = std::process::Command::new("pbcopy")
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                    .and_then(|mut child| {
                        use std::io::Write;
                        if let Some(ref mut stdin) = child.stdin {
                            let _ = stdin.write_all(output.as_bytes());
                        }
                        child.wait()
                    });
            }
        }
        "file" => {
            let out_path = pipes_dir().join(id).join("output.txt");
            let _ = fs::write(&out_path, &output);
        }
        "notification" => {
            #[cfg(target_os = "macos")]
            {
                let short = if output.len() > 200 { &output[..200] } else { &output };
                let script = format!(
                    "display notification \"{}\" with title \"MindScope: {}\"",
                    short.replace('"', "'"), config.name.replace('"', "'")
                );
                let _ = std::process::Command::new("osascript").args(["-e", &script]).status();
            }
        }
        _ => {}
    }

    let now = chrono_now();
    let result = PipeResult {
        pipe_id: id.to_string(),
        timestamp: now,
        output: output.clone(),
        success: true,
    };

    // Save last result
    let result_path = pipes_dir().join(id).join("last_result.json");
    if let Ok(json) = serde_json::to_string_pretty(&result) {
        let _ = fs::write(&result_path, json);
    }

    Ok(result)
}

/// Gather screen history context from DB
fn gather_context(query: &Option<String>, hours: u32) -> String {
    let cutoff = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i64 - (hours as i64 * 3600 * 1_000_000);

    // If query provided, search; otherwise get recent frames
    let frames = if let Some(q) = query {
        super::db::search_frames(q, 20).unwrap_or_default()
    } else {
        // Get today's frames (last N hours)
        let today = chrono_now();
        let date = &today[..10];
        super::db::get_frames_for_date(date).unwrap_or_default()
            .into_iter()
            .filter(|f| f.timestamp >= cutoff)
            .collect()
    };

    if frames.is_empty() { return String::new(); }

    // Build context string — summarize frames
    let mut context = String::new();
    let mut last_app = String::new();
    for frame in frames.iter().take(50) {
        let ts_secs = frame.timestamp / 1_000_000;
        let hour = (ts_secs % 86400) / 3600;
        let min = (ts_secs % 3600) / 60;

        if frame.app_name != last_app {
            context.push_str(&format!("\n[{:02}:{:02}] {} - {}\n", hour, min, frame.app_name, frame.window_name));
            last_app = frame.app_name.clone();
        }

        // Extract just the text portion (before regions separator)
        let text = if let Some(idx) = frame.ocr_text.find("\n---REGIONS---\n") {
            &frame.ocr_text[..idx]
        } else {
            &frame.ocr_text
        };

        if !text.is_empty() && text.len() > 10 {
            // Truncate long OCR text
            let short = if text.len() > 200 { &text[..200] } else { text };
            context.push_str(&format!("  {}\n", short));
        }
    }

    context
}

/// Call Claude CLI inside the MindScope vault (picks up CLAUDE.md + skills)
fn call_claude(prompt: &str) -> Result<String, String> {
    let vault = dirs_next::home_dir().unwrap_or_default().join(".mindscope").join("vault");
    let cwd = if vault.exists() { vault } else { dirs_next::home_dir().unwrap_or_default() };

    let result = std::process::Command::new("/opt/homebrew/bin/claude")
        .args(["-p", prompt])
        .current_dir(&cwd)
        .env("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin")
        .output();

    match result {
        Ok(output) if output.status.success() => {
            let response = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if response.is_empty() {
                Err("Empty response from Claude".into())
            } else {
                Ok(response)
            }
        }
        Ok(output) => Err(format!("Claude error: {}", String::from_utf8_lossy(&output.stderr).chars().take(200).collect::<String>())),
        Err(e) => Err(format!("Claude not found: {}", e)),
    }
}

fn chrono_now() -> String {
    super::recorder::timestamp_now()
}

/// Create default example pipes
pub fn ensure_default_pipes() {
    let dir = pipes_dir();

    // Daily summary pipe
    let summary_dir = dir.join("daily-summary");
    if !summary_dir.exists() {
        let _ = create_pipe("daily-summary", PipeConfig {
            name: "Daily Summary".into(),
            schedule: "manual".into(),
            enabled: true,
            prompt: "Summarize my screen activity today. Group by app and highlight key tasks. Be concise.".into(),
            output: "clipboard".into(),
            context_query: None,
            context_hours: Some(8),
        });
    }

    // Meeting notes pipe
    let meeting_dir = dir.join("meeting-notes");
    if !meeting_dir.exists() {
        let _ = create_pipe("meeting-notes", PipeConfig {
            name: "Meeting Notes".into(),
            schedule: "manual".into(),
            enabled: true,
            prompt: "Extract meeting notes from the screen activity. List attendees, topics discussed, decisions made, and action items.".into(),
            output: "clipboard".into(),
            context_query: Some("Zoom Teams Meet FaceTime".into()),
            context_hours: Some(2),
        });
    }
}

// ===== Pipe Scheduler =====

/// Parsed schedule representation
enum ParsedSchedule {
    Manual,
    EveryMinutes(u64),
    DailyAt { hour: u32, minute: u32 },
}

/// Parse a schedule string like "manual", "every 30m", "daily 18:00"
fn parse_schedule(s: &str) -> ParsedSchedule {
    let s = s.trim().to_lowercase();
    if s == "manual" || s.is_empty() {
        return ParsedSchedule::Manual;
    }
    // "every Nm" — extract N
    if s.starts_with("every ") && s.ends_with('m') {
        let num_part = s[6..s.len() - 1].trim();
        if let Ok(n) = num_part.parse::<u64>() {
            if n > 0 {
                return ParsedSchedule::EveryMinutes(n);
            }
        }
    }
    // "daily HH:MM"
    if s.starts_with("daily ") {
        let time_part = s[6..].trim();
        if let Some((hh, mm)) = time_part.split_once(':') {
            if let (Ok(h), Ok(m)) = (hh.parse::<u32>(), mm.parse::<u32>()) {
                if h < 24 && m < 60 {
                    return ParsedSchedule::DailyAt { hour: h, minute: m };
                }
            }
        }
    }
    ParsedSchedule::Manual
}

/// Get the last_run timestamp (epoch secs) for a pipe from its last_result.json
fn get_last_run_epoch(pipe_id: &str) -> Option<u64> {
    let result_path = pipes_dir().join(pipe_id).join("last_result.json");
    let content = fs::read_to_string(&result_path).ok()?;
    let result: PipeResult = serde_json::from_str(&content).ok()?;
    // Parse timestamp string "YYYY-MM-DDTHH:MM:SS" back to epoch seconds
    parse_timestamp_to_epoch(&result.timestamp)
}

/// Parse a "YYYY-MM-DDTHH:MM:SS" timestamp to epoch seconds (UTC)
fn parse_timestamp_to_epoch(ts: &str) -> Option<u64> {
    // Expected format: "2026-04-09T14:30:00" or similar
    if ts.len() < 19 { return None; }
    let year: u64 = ts[0..4].parse().ok()?;
    let month: u64 = ts[5..7].parse().ok()?;
    let day: u64 = ts[8..10].parse().ok()?;
    let hour: u64 = ts[11..13].parse().ok()?;
    let min: u64 = ts[14..16].parse().ok()?;
    let sec: u64 = ts[17..19].parse().ok()?;

    // Days from epoch to start of year
    let mut days: u64 = 0;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    // Days in months of current year
    let leap = is_leap(year);
    let month_days = if leap {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    for m in 0..(month as usize - 1).min(11) {
        days += month_days[m];
    }
    days += day - 1;

    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Get current UTC hour and minute from SystemTime
fn current_utc_hm() -> (u32, u32) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let tod = secs % 86400;
    ((tod / 3600) as u32, ((tod % 3600) / 60) as u32)
}

/// Check if a pipe is due to run based on its schedule and last_run time
fn is_pipe_due(pipe: &PipeInfo) -> bool {
    if !pipe.config.enabled {
        return false;
    }

    let schedule = parse_schedule(&pipe.config.schedule);
    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let last_run_epoch = get_last_run_epoch(&pipe.id).unwrap_or(0);

    match schedule {
        ParsedSchedule::Manual => false,
        ParsedSchedule::EveryMinutes(n) => {
            let interval_secs = n * 60;
            now_epoch >= last_run_epoch + interval_secs
        }
        ParsedSchedule::DailyAt { hour, minute } => {
            let (cur_h, cur_m) = current_utc_hm();
            // Check if we're in the right minute window (within 2 min of target)
            let target_min_of_day = hour * 60 + minute;
            let cur_min_of_day = cur_h * 60 + cur_m;
            let in_window = cur_min_of_day >= target_min_of_day
                && cur_min_of_day < target_min_of_day + 2;
            // And haven't run in the last 23 hours (prevent double-fire)
            let not_recently_run = now_epoch >= last_run_epoch + 23 * 3600;
            in_window && not_recently_run
        }
    }
}

static SCHEDULER_RUNNING: AtomicBool = AtomicBool::new(false);

/// Start the pipe scheduler background thread.
/// Checks every 60 seconds if any scheduled pipe is due to run.
/// Safe to call multiple times — only one scheduler will run.
pub fn start_scheduler() {
    if SCHEDULER_RUNNING.swap(true, Ordering::SeqCst) {
        // Already running
        return;
    }

    std::thread::spawn(|| {
        log::info!("MindScope: Pipe scheduler started (60s check interval)");
        loop {
            std::thread::sleep(Duration::from_secs(60));

            let pipes = list_pipes();
            for pipe in &pipes {
                if is_pipe_due(pipe) {
                    log::info!("MindScope: Scheduler running pipe '{}'", pipe.id);
                    match run_pipe(&pipe.id) {
                        Ok(_) => log::info!("MindScope: Scheduled pipe '{}' completed", pipe.id),
                        Err(e) => log::error!("MindScope: Scheduled pipe '{}' failed: {}", pipe.id, e),
                    }
                }
            }
        }
    });
}
