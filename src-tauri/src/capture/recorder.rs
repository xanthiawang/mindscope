use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::screenshot::{capture_screen, get_active_window_info, check_screen_permission, mark_permission_granted};
use super::ocr;
use super::video;
use super::db;
use super::settings;
use super::audio;

// Global meeting state — frontend polls this via Tauri command or HTTP endpoint
static MEETING_ACTIVE: AtomicBool = AtomicBool::new(false);
static MEETING_APP: Mutex<Option<String>> = Mutex::new(None);
static MEETING_START: AtomicI64 = AtomicI64::new(0);
// User manually stopped auto-recording this session — don't restart until meeting ends
static MEETING_AUDIO_SUPPRESSED: AtomicBool = AtomicBool::new(false);

/// Append a single line to ~/.mindscope/data/meeting_debug.log to trace every
/// write to MEETING_ACTIVE. Used while hunting the manual-mic-transcript bug.
/// Safe to call from any thread.
pub fn log_meeting_event(reason: &str, new_value: bool) {
    use std::io::Write;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let line = format!(
        "{} MEETING_ACTIVE={} reason={}\n",
        now, new_value, reason
    );
    let path = dirs_next::home_dir()
        .unwrap_or_default()
        .join(".mindscope")
        .join("data")
        .join("meeting_debug.log");
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

// Current audio session — activity/task based (one meeting = one session, manual recording = another)
static CURRENT_SESSION_ID: Mutex<Option<String>> = Mutex::new(None);
static CURRENT_SESSION_TYPE: Mutex<Option<String>> = Mutex::new(None);

/// Start a new audio session. Called when a meeting is detected or user manually toggles mic.
/// `session_type` is the app name ("Zoom", "Tencent Meeting", ...) or "manual".
pub fn start_audio_session(session_type: &str) {
    let id = timestamp_now(); // unique session ID = start timestamp
    if let Ok(mut s) = CURRENT_SESSION_ID.lock() { *s = Some(id); }
    if let Ok(mut t) = CURRENT_SESSION_TYPE.lock() { *t = Some(session_type.to_string()); }
    log::info!("MindScope: Audio session started ({})", session_type);
}

/// End the current audio session. Called when meeting ends or user manually stops.
pub fn end_audio_session() {
    if let Ok(mut s) = CURRENT_SESSION_ID.lock() { *s = None; }
    if let Ok(mut t) = CURRENT_SESSION_TYPE.lock() { *t = None; }
    log::info!("MindScope: Audio session ended");
}

/// Get the current session (id, type) if one is active.
pub fn get_current_session() -> (String, String) {
    let id = CURRENT_SESSION_ID.lock().ok().and_then(|g| g.clone()).unwrap_or_default();
    let ty = CURRENT_SESSION_TYPE.lock().ok().and_then(|g| g.clone()).unwrap_or_default();
    (id, ty)
}

/// Called when user manually stops audio during a meeting — prevents auto-restart
pub fn suppress_auto_audio() {
    MEETING_AUDIO_SUPPRESSED.store(true, Ordering::Relaxed);
}

/// Check if audio is currently suppressed (used by audio streaming loop)
pub fn is_audio_suppressed() -> bool {
    MEETING_AUDIO_SUPPRESSED.load(Ordering::Relaxed)
}

/// Get the current meeting state: (active, app_name, start_time_epoch_micros)
pub fn get_meeting_state() -> (bool, String, i64) {
    let active = MEETING_ACTIVE.load(Ordering::Relaxed);
    let app = MEETING_APP.lock().unwrap().clone().unwrap_or_default();
    let start = MEETING_START.load(Ordering::Relaxed);
    (active, app, start)
}

/// Mark the start of a manual recording session (mic button pressed).
/// Publishes MEETING_ACTIVE/MEETING_START so the Transcript tab polls can
/// show the live transcript exactly like an auto-detected meeting.
pub fn mark_manual_meeting_start(label: &str) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i64;
    MEETING_ACTIVE.store(true, Ordering::Relaxed);
    MEETING_START.store(ts, Ordering::Relaxed);
    if let Ok(mut m) = MEETING_APP.lock() { *m = Some(label.to_string()); }
    log_meeting_event(&format!("mark_manual_meeting_start({})", label), true);
}

/// Mark the end of a manual recording session (mic button released).
pub fn mark_manual_meeting_end() {
    MEETING_ACTIVE.store(false, Ordering::Relaxed);
    MEETING_START.store(0, Ordering::Relaxed);
    if let Ok(mut m) = MEETING_APP.lock() { *m = None; }
    log_meeting_event("mark_manual_meeting_end", false);
}

/// Dump all meeting-related state for the /debug/meeting endpoint.
pub fn dump_debug_state() -> serde_json::Value {
    let (active, app, start) = get_meeting_state();
    let (session_id, session_type) = get_current_session();
    let suppressed = MEETING_AUDIO_SUPPRESSED.load(Ordering::Relaxed);
    serde_json::json!({
        "MEETING_ACTIVE": active,
        "MEETING_APP": app,
        "MEETING_START": start,
        "MEETING_AUDIO_SUPPRESSED": suppressed,
        "CURRENT_SESSION_ID": session_id,
        "CURRENT_SESSION_TYPE": session_type,
    })
}

pub struct Recorder {
    running: Arc<AtomicBool>,
    interval_secs: u64,
}

impl Recorder {
    pub fn new(interval_secs: u64) -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            interval_secs,
        }
    }

    pub fn start(&self) -> bool {
        if self.running.load(Ordering::Relaxed) { return false; }
        self.running.store(true, Ordering::Relaxed);
        let running = self.running.clone();
        let interval = self.interval_secs;

        thread::spawn(move || {
            let mut frame_count: u64 = 0;
            let mut last_ocr_text = String::new();
            let mut last_app = String::new();
            let mut encoder_started = false;
            let mut encoder_attempted = false; // don't retry after first failure
            let mut last_frame_path: Option<String> = None;
            let mut skip_count: u64 = 0;
            let frame_buffer = db::FrameBuffer::new(5, 10);
            // Meeting auto-detect: auto-start/stop audio when meeting app is active
            let mut meeting_audio_active = false;
            let mut meeting_start_ts: i64 = 0;
            let mut meeting_app_name = String::new();
            let meeting_apps = ["zoom.us", "Zoom", "FaceTime", "Microsoft Teams",
                "Webex", "Discord", "Tencent Meeting", "TencentMeeting",
                "腾讯会议TencentMeeting", "腾讯会议",
                "DingTalk", "钉钉", "飞书", "Lark", "Skype", "WeMeet"];
            let auto_audio = audio::AudioRecorder::new(2);

            log::info!("MindScope recorder started, interval={}s", interval);

            if let Err(e) = db::init_db() {
                log::error!("MindScope: DB init failed: {}", e);
                return;
            }

            // Bootstrap permission: if DB has frames, we already have permission
            if let Ok(stats) = db::get_storage_stats() {
                if stats.frame_count > 0 {
                    mark_permission_granted();
                }
            }

            while running.load(Ordering::Relaxed) {
                if !check_screen_permission() {
                    // Try a test capture to bootstrap permission
                    let test_dir = db::frames_dir(&chrono_now()[..10]);
                    if capture_screen(&test_dir).is_some() {
                        // capture_screen sets PERMISSION_CONFIRMED on success
                    } else {
                        thread::sleep(Duration::from_secs(5));
                        continue;
                    }
                }

                // Start HEVC encoder on first successful permission check (try once only)
                if !encoder_started && !encoder_attempted {
                    encoder_attempted = true;
                    match video::start_encoder() {
                        Ok(_) => { encoder_started = true; }
                        Err(e) => { log::warn!("MindScope: encoder start failed: {}", e); }
                    }
                }

                let now = chrono_now();
                let date = &now[..10];
                let output_dir = db::frames_dir(date);
                let (app_name, window_name, _bundle_id, _browser_url) = get_active_window_info();
                let same_context = app_name == last_app && frame_count > 0;

                // Meeting auto-detect: use Swift helper + frontmost app name
                let log_path = db::data_dir().join("debug.log");
                let _ = std::fs::OpenOptions::new()
                    .create(true).append(true).open(&log_path)
                    .and_then(|mut f| {
                        use std::io::Write;
                        writeln!(f, "frame={} app={} win={}", frame_count, app_name, window_name)
                    });

                let helper_result = if frame_count % 3 == 0 {
                    is_meeting_process_running()
                } else { false };

                let app_match = {
                    let app_lower = app_name.to_lowercase();
                    meeting_apps.iter().any(|&m| {
                        let ml = m.to_lowercase();
                        app_lower.contains(&ml) || ml.contains(&app_lower)
                    }) || window_name.to_lowercase().contains("会议")
                };

                let in_meeting = helper_result || meeting_audio_active || app_match;

                let _ = std::fs::OpenOptions::new()
                    .create(true).append(true).open(&log_path)
                    .and_then(|mut f| {
                        use std::io::Write;
                        writeln!(f, "  helper={} app_match={} in_meeting={}", helper_result, app_match, in_meeting)
                    });

                // Honor manual suppression: user stopped auto-recording
                let suppressed = MEETING_AUDIO_SUPPRESSED.load(Ordering::Relaxed);

                // If user suppressed while recording, stop IMMEDIATELY (every frame, not just % 3)
                if suppressed && meeting_audio_active {
                    auto_audio.stop();
                    meeting_audio_active = false;
                    // Also kill any lingering ffmpeg children
                    #[cfg(target_os = "windows")]
                    super::platform::kill_audio_processes();
                    #[cfg(not(target_os = "windows"))]
                    let _ = std::process::Command::new("pkill").args(["-f", "ffmpeg.*avfoundation"]).status();
                }

                // Is a manual recording session already running? (toggle_audio
                // sets MEETING_ACTIVE via mark_manual_meeting_start but does
                // NOT set the loop-local meeting_audio_active.)
                let manual_session_running =
                    MEETING_ACTIVE.load(Ordering::Relaxed) && !meeting_audio_active;

                if in_meeting && !meeting_audio_active && !suppressed && !manual_session_running {
                    let has_mic = audio::check_mic_permission();
                    let log_path = db::data_dir().join("debug.log");
                    let _ = std::fs::OpenOptions::new()
                        .create(true).append(true).open(&log_path)
                        .and_then(|mut f| {
                            use std::io::Write;
                            writeln!(f, ">>> MEETING START! mic={} app={}", has_mic, app_name)
                        });
                    if has_mic {
                        // Detect which meeting app is running to tag the session
                        let session_type = detect_meeting_app_name().unwrap_or_else(|| app_name.clone());
                        start_audio_session(&session_type);
                        auto_audio.start();
                        meeting_audio_active = true;
                        meeting_start_ts = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default().as_micros() as i64;
                        meeting_app_name = app_name.clone();
                        MEETING_ACTIVE.store(true, Ordering::Relaxed);
                        MEETING_START.store(meeting_start_ts, Ordering::Relaxed);
                        if let Ok(mut m) = MEETING_APP.lock() { *m = Some(meeting_app_name.clone()); }
                        log_meeting_event(&format!("auto_start({})", app_name), true);
                        log::info!("MindScope: Meeting detected ({}), auto-started audio", app_name);
                    }
                } else if !in_meeting && meeting_audio_active {
                    // AUTO session ended (meeting app closed). Clean up the
                    // auto state. If a manual recording has piggybacked on
                    // top (toggle_audio called while auto was still running),
                    // preserve the global flags so its Transcript tab stays
                    // alive — we can detect that because toggle_audio leaves
                    // CURRENT_SESSION_TYPE == "manual".
                    auto_audio.stop();
                    meeting_audio_active = false;

                    let (_, cur_type) = get_current_session();
                    let manual_owned = cur_type == "manual";

                    if !manual_owned {
                        end_audio_session();
                        MEETING_AUDIO_SUPPRESSED.store(false, Ordering::Relaxed);
                        MEETING_ACTIVE.store(false, Ordering::Relaxed);
                        MEETING_START.store(0, Ordering::Relaxed);
                        if let Ok(mut m) = MEETING_APP.lock() { *m = None; }
                        log_meeting_event("auto_cleanup_meeting_ended", false);
                        let end_ts = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default().as_micros() as i64;
                        log::info!("MindScope: Meeting ended, generating vault notes...");
                        let m_app = meeting_app_name.clone();
                        let m_start = meeting_start_ts;
                        let m_win = window_name.clone();
                        thread::spawn(move || {
                            generate_meeting_vault(m_start, end_ts, &m_app, &m_win);
                        });
                    } else {
                        log_meeting_event("auto_cleanup_SKIPPED_manual_owned", MEETING_ACTIVE.load(Ordering::Relaxed));
                    }
                    // If manual_owned: manual session is running on its own ffmpeg child —
                    // leave MEETING_ACTIVE/START/APP and CURRENT_SESSION_* alone.
                } else if !in_meeting && suppressed && !meeting_audio_active {
                    // Suppression is set but the auto loop was never itself
                    // recording. Only clear the suppression if no manual
                    // session is currently running — a manual recording
                    // publishes MEETING_ACTIVE=true via mark_manual_meeting_start,
                    // so treat that as "hands off the global flags".
                    if !MEETING_ACTIVE.load(Ordering::Relaxed) {
                        MEETING_AUDIO_SUPPRESSED.store(false, Ordering::Relaxed);
                    }
                }

                // App exclusion check
                let app_settings = settings::load_settings();
                if app_settings.excluded_apps.iter().any(|a| a.eq_ignore_ascii_case(&app_name)) {
                    thread::sleep(Duration::from_secs(interval));
                    continue;
                }

                match capture_screen(&output_dir) {
                    Some(image_path) => {
                        mark_permission_granted();

                        // Frame dedup: histogram + perceptual hash comparison
                        if same_context {
                            if let Some(ref prev_path) = last_frame_path {
                                let prev = std::path::Path::new(prev_path);
                                if prev.exists() && frames_are_similar(&image_path, prev) {
                                    // Similar frame, skip (delete the captured file)
                                    let _ = std::fs::remove_file(&image_path);
                                    skip_count += 1;
                                    if skip_count % 20 == 0 {
                                        log::info!("MindScope: skipped {} duplicate frames", skip_count);
                                    }
                                    thread::sleep(Duration::from_secs(interval));
                                    continue;
                                }
                            }
                        }
                        last_frame_path = Some(image_path.to_string_lossy().to_string());

                        // OCR with bounding boxes every 3rd frame or on app change
                        let ocr_result = if frame_count % 3 == 0 || !same_context {
                            let mut result = ocr::extract_with_regions(&image_path);
                            result.text = super::pii::sanitize(&result.text);
                            if !result.text.is_empty() { last_ocr_text = result.text.clone(); }
                            result
                        } else {
                            ocr::OcrResult { text: last_ocr_text.clone(), regions: vec![] }
                        };

                        last_app = app_name.clone();

                        let ts_micros = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_micros() as i64;

                        // Pipe frame to HEVC encoder
                        if encoder_started {
                            video::encode_frame(&image_path);
                        }

                        // Buffer frame for batched DB write (flushes every 5 frames or 10s)
                        let path_ref = image_path.to_string_lossy().to_string();
                        frame_buffer.buffer_frame(ts_micros, &app_name, &window_name, &ocr_result, &path_ref);

                        // Flush buffer if thresholds met (count or time)
                        if let Err(e) = frame_buffer.maybe_flush() {
                            log::error!("MindScope: frame buffer flush failed: {}", e);
                        }

                        // Check encoder output — update DB with video:// refs
                        // The encoder deletes source JPEGs after encoding, so we update the path
                        for enc_frame in video::drain_encoder_output() {
                            let video_ref = format!("video://{}#{}", enc_frame.segment, enc_frame.frame);
                            let _ = db::update_latest_frame_path(&video_ref);
                        }

                        frame_count += 1;
                        if frame_count % 10 == 0 {
                            log::info!("MindScope: captured {} frames", frame_count);
                        }
                    }
                    None => {}
                }

                thread::sleep(Duration::from_secs(interval));
            }

            // Final flush of any remaining buffered frames before shutdown
            if let Err(e) = frame_buffer.flush_frames() {
                log::error!("MindScope: final frame buffer flush failed: {}", e);
            }
            video::stop_encoder();
            log::info!("MindScope recorder stopped");
        });

        true
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
}

pub fn timestamp_now() -> String { chrono_now() }

/// Get the name of the detected meeting app (e.g. "Zoom", "Tencent Meeting").
/// Reads the Swift helper output which includes the app name.
pub fn detect_meeting_app_name() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        return super::platform::get_meeting_app_name();
    }

    #[cfg(not(target_os = "windows"))]
    {
        let helper = dirs_next::home_dir().unwrap_or_default()
            .join(".mindscope").join("bin").join("is_meeting");
        if !helper.exists() { return None; }
        let output = std::process::Command::new(helper.to_str().unwrap_or("")).output().ok()?;
        let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !result.starts_with("MEETING|") { return None; }
        let parts: Vec<&str> = result.splitn(3, '|').collect();
        parts.get(1).map(|s| s.to_string())
    }
}

/// Smart meeting detection using compiled Swift helper.
/// Checks both meeting app running AND microphone actively in use.
/// Returns: "MEETING|name|bundle" if in call, "APP_OPEN|..." if app open but not calling,
///          "MIC_ACTIVE" if mic in use by unknown app, "NONE" if idle.
fn is_meeting_active() -> (bool, String) {
    #[cfg(target_os = "windows")]
    {
        let name = super::platform::get_meeting_app_name();
        return (name.is_some(), name.unwrap_or_default());
    }

    #[cfg(not(target_os = "windows"))]
    {
        let helper = dirs_next::home_dir().unwrap_or_default()
            .join(".mindscope").join("bin").join("is_meeting");
        if !helper.exists() { return (false, String::new()); }

        if let Ok(output) = std::process::Command::new(helper.to_str().unwrap_or(""))
            .output()
        {
            let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if result.starts_with("MEETING|") {
                let parts: Vec<&str> = result.splitn(3, '|').collect();
                let app_name = parts.get(1).unwrap_or(&"Meeting").to_string();
                return (true, app_name);
            }
        }
        (false, String::new())
    }
}

// Keep backwards compat
fn is_meeting_process_running() -> bool {
    is_meeting_active().0
}

/// Check if a meeting is actively in progress
/// Uses window title scanning — Zoom shows "Zoom Meeting" window only during calls
#[cfg(target_os = "macos")]
fn is_meeting_running(_meeting_apps: &[&str]) -> bool {
    // Fast check: use lsappinfo to get window list (faster than AppleScript)
    let output = std::process::Command::new("osascript")
        .args(["-e", r#"
tell application "System Events"
    set result to ""
    repeat with p in (every process whose background only is false)
        try
            set appName to name of p
            repeat with w in (every window of p)
                set winName to name of w
                set result to result & appName & "|" & winName & linefeed
            end repeat
        end try
    end repeat
    return result
end tell
"#])
        .output()
        .ok();

    if let Some(out) = output {
        let windows = String::from_utf8_lossy(&out.stdout).to_lowercase();
        // Parse window lines and check each one
        for line in windows.lines() {
            let line = line.trim();
            if line.is_empty() { continue; }
            // Format: "appname|windowtitle"
            let parts: Vec<&str> = line.splitn(2, '|').collect();
            let (app, title) = if parts.len() == 2 { (parts[0], parts[1]) } else { continue };

            // Zoom: window title must contain "zoom meeting" (not just "zoom workplace")
            if app.contains("zoom") && title.contains("zoom meeting") { return true; }
            // FaceTime: any window with a person's name (active call)
            if app.contains("facetime") && !title.is_empty() && title != "facetime" { return true; }
            // Teams: window with "meeting" or "call" in title
            if app.contains("teams") && (title.contains("meeting") || title.contains("call")) { return true; }
            // 飞书/Lark with meeting window
            if (app.contains("飞书") || app.contains("lark")) && (title.contains("会议") || title.contains("meeting")) { return true; }
            // 钉钉 with meeting window
            if (app.contains("钉钉") || app.contains("dingtalk")) && (title.contains("会议") || title.contains("meeting")) { return true; }
            // 腾讯会议 / Tencent Meeting / WeMeet — any window with "会议" or meeting-related title
            if (app.contains("tencentmeeting") || app.contains("tencent meeting") || app.contains("腾讯会议") || app.contains("wemeet"))
                && (title.contains("会议") || title.contains("meeting") || !title.is_empty()) { return true; }
        }
    }
    false
}

/// Generate meeting notes and write to vault after a meeting ends
fn generate_meeting_vault(start_ts: i64, end_ts: i64, app_name: &str, window_name: &str) {
    // 1. Gather screen OCR text from the meeting period
    let frames = db::get_frames_for_date(&chrono_now()[..10]).unwrap_or_default();
    let meeting_frames: Vec<_> = frames.iter()
        .filter(|f| f.timestamp >= start_ts && f.timestamp <= end_ts)
        .collect();

    let mut screen_context = String::new();
    let mut last_text = String::new();
    for f in &meeting_frames {
        let text = f.ocr_text.split("\n---REGIONS---\n").next().unwrap_or("");
        if text.len() > 20 && text != last_text {
            screen_context.push_str(&format!("{}\n", &text[..text.len().min(200)]));
            last_text = text.to_string();
        }
    }

    // 2. Gather audio transcripts from the meeting period
    let date = &chrono_now()[..10];
    let audio_segments = audio::load_audio_segments(date);
    let mut transcript = String::new();
    for seg in &audio_segments {
        if !seg.transcript.is_empty() {
            transcript.push_str(&seg.transcript);
            transcript.push(' ');
        }
    }

    // 3. Call Claude to generate structured meeting notes
    let prompt = format!(
        "Generate concise meeting notes in markdown. No meta-commentary.\n\
         App: {}\nWindow: {}\n\
         Screen text during meeting:\n{}\n\
         Audio transcript:\n{}\n\n\
         Format:\n## Summary\n(2-3 sentences)\n\n## Key Points\n- point1\n- point2\n\n## Action Items\n- [ ] item1\n- [ ] item2\n\n\
         If data is sparse, write what you can. Keep it short.",
        app_name, window_name,
        &screen_context[..screen_context.len().min(2000)],
        &transcript[..transcript.len().min(2000)]
    );

    let vault_dir = dirs_next::home_dir().unwrap_or_default().join(".mindscope").join("vault");
    let _ = std::fs::create_dir_all(&vault_dir);

    let notes = match call_claude_cli(&prompt) {
        Some(response) => response,
        None => {
            // Fallback: write raw data without AI
            format!("## Summary\nMeeting on {} ({})\n\n## Transcript\n{}\n\n## Screen Notes\n{}",
                app_name, window_name, transcript.chars().take(1000).collect::<String>(),
                screen_context.chars().take(500).collect::<String>())
        }
    };

    // 4. Write meet.YYYY.MM.DD.md to vault
    let date_dots = date.replace('-', ".");
    let filename = format!("meet.{}.md", date_dots);
    let filepath = vault_dir.join(&filename);

    // Don't overwrite existing meeting notes, append a suffix
    let filepath = if filepath.exists() {
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default().as_secs();
        vault_dir.join(format!("meet.{}.{}.md", date_dots, ts % 10000))
    } else {
        filepath
    };

    let content = format!(
        "---\ntitle: {}\ndate: {}\napp: {}\nupdated: {}\n---\n\n{}",
        window_name.chars().take(60).collect::<String>(),
        date,
        app_name,
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default().as_secs(),
        notes
    );

    match std::fs::write(&filepath, &content) {
        Ok(_) => log::info!("MindScope: Vault meeting notes saved to {:?}", filepath),
        Err(e) => log::error!("MindScope: Failed to write vault notes: {}", e),
    }
}

/// Call Claude CLI for cheap background tasks (meeting notes, OCR summaries).
/// Routes through Haiku to minimize cost — these are "read + summarize" jobs
/// that don't need Sonnet-level reasoning.
fn call_claude_cli(prompt: &str) -> Option<String> {
    // Run Claude inside the MindScope vault so it picks up CLAUDE.md + skills
    let vault = dirs_next::home_dir().unwrap_or_default().join(".mindscope").join("vault");
    let cwd = if vault.exists() { vault } else { dirs_next::home_dir().unwrap_or_default() };
    let claude_path = super::platform::find_claude_cli()
        .unwrap_or_else(|| "claude".to_string());
    let output = std::process::Command::new(&claude_path)
        .args(["-p", prompt, "--model", "claude-haiku-4-5"])
        .current_dir(&cwd)
        .output()
        .ok()?;
    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !text.is_empty() { Some(text) } else { None }
    } else {
        None
    }
}

/// Compute 256-bin grayscale histogram from a 16x16 downsampled image.
/// Returns None if the image can't be opened.
fn compute_histogram(path: &std::path::Path) -> Option<[u32; 256]> {
    let img = image::open(path).ok()?;
    let small = img.resize_exact(16, 16, image::imageops::FilterType::Triangle).to_luma8();
    let mut hist = [0u32; 256];
    for p in small.pixels() {
        hist[p.0[0] as usize] += 1;
    }
    Some(hist)
}

/// Compute histogram correlation coefficient between two 256-bin histograms.
/// Returns a value in [-1.0, 1.0] where 1.0 means identical distributions.
/// Uses the Pearson correlation formula (same as OpenCV's HISTCMP_CORREL).
fn histogram_correlation(h1: &[u32; 256], h2: &[u32; 256]) -> f64 {
    let n = 256.0;
    let mean1: f64 = h1.iter().map(|&v| v as f64).sum::<f64>() / n;
    let mean2: f64 = h2.iter().map(|&v| v as f64).sum::<f64>() / n;

    let mut num = 0.0;
    let mut den1 = 0.0;
    let mut den2 = 0.0;
    for i in 0..256 {
        let d1 = h1[i] as f64 - mean1;
        let d2 = h2[i] as f64 - mean2;
        num += d1 * d2;
        den1 += d1 * d1;
        den2 += d2 * d2;
    }
    let den = (den1 * den2).sqrt();
    if den < 1e-10 { 1.0 } else { num / den }
}

/// Perceptual hash for exact-duplicate detection (secondary check).
/// Downsample to 16x16 grayscale, threshold against mean → 256-bit hash.
fn perceptual_hash(path: &std::path::Path) -> Option<[u64; 4]> {
    let img = image::open(path).ok()?;
    let small = img.resize_exact(16, 16, image::imageops::FilterType::Nearest).to_luma8();
    let pixels: Vec<u8> = small.pixels().map(|p| p.0[0]).collect();
    let mean: u8 = (pixels.iter().map(|&p| p as u32).sum::<u32>() / 256) as u8;
    let mut hash = [0u64; 4];
    for (i, &p) in pixels.iter().enumerate() {
        if p > mean {
            hash[i / 64] |= 1u64 << (i % 64);
        }
    }
    Some(hash)
}

/// Advanced frame similarity check inspired by Screenpipe's frame comparison.
/// 1. Perceptual hash for exact-duplicate early exit (fast path)
/// 2. Histogram correlation for near-duplicate detection (threshold: 0.97)
/// Returns true if frames are similar enough to skip.
fn frames_are_similar(path1: &std::path::Path, path2: &std::path::Path) -> bool {
    // Step 1: Perceptual hash early exit — if hashes match exactly, frames are identical
    if let (Some(h1), Some(h2)) = (perceptual_hash(path1), perceptual_hash(path2)) {
        if h1 == h2 {
            return true;
        }
    }

    // Step 2: Histogram correlation — catches near-duplicates (minor noise, compression artifacts)
    if let (Some(hist1), Some(hist2)) = (compute_histogram(path1), compute_histogram(path2)) {
        let corr = histogram_correlation(&hist1, &hist2);
        if corr > 0.97 {
            return true;
        }
    }

    false
}

fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let days = secs / 86400;
    let tod = secs % 86400;
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let mut year = 1970u64;
    let mut rem = days;
    loop {
        let dy = if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 { 366 } else { 365 };
        if rem < dy { break; }
        rem -= dy;
        year += 1;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let md = if leap { [31,29,31,30,31,30,31,31,30,31,30,31] } else { [31,28,31,30,31,30,31,31,30,31,30,31] };
    let mut month = 0u64;
    for (i, &d) in md.iter().enumerate() {
        if rem < d { month = i as u64 + 1; break; }
        rem -= d;
    }
    if month == 0 { month = 12; }
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", year, month, rem + 1, h, m, s)
}
