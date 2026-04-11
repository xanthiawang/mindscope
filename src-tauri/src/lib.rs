mod capture;
mod vault;

use std::path::PathBuf;
use std::sync::Mutex;

use tauri::{Emitter, Manager, State};

use capture::audio::{self, AudioRecorder, AudioSegment};
use capture::recorder::Recorder;
use capture::screenshot;
use capture::db;
use capture::video;
use capture::panel;
use capture::whisper;

// App state
struct AppState {
    vault_path: PathBuf,
    recorder: Recorder,
    audio_recorder: AudioRecorder,
}

type ManagedState = Mutex<AppState>;

// --- Permission (NEVER triggers dialog) ---

#[tauri::command]
fn check_permission() -> bool {
    // If we already confirmed, return quickly
    if screenshot::check_screen_permission() {
        return true;
    }
    // Check if DB has any frames (means we captured before = permission granted)
    if let Ok(stats) = db::get_storage_stats() {
        if stats.frame_count > 0 {
            screenshot::mark_permission_granted();
            return true;
        }
    }
    // Check if frames directory has files
    let data_dir = db::data_dir().join("frames");
    if data_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&data_dir) {
            if entries.into_iter().count() > 0 {
                screenshot::mark_permission_granted();
                return true;
            }
        }
    }
    false
}

#[tauri::command]
fn open_permission_settings() {
    screenshot::open_permission_settings();
}

// --- Recording ---

#[tauri::command]
fn start_recording(state: State<'_, ManagedState>) -> bool {
    let state = state.lock().unwrap();
    state.recorder.start()
}

#[tauri::command]
fn stop_recording(state: State<'_, ManagedState>) {
    let state = state.lock().unwrap();
    state.recorder.stop();
}

#[tauri::command]
fn is_recording(state: State<'_, ManagedState>) -> bool {
    let state = state.lock().unwrap();
    state.recorder.is_running()
}

// --- Window resize (bar <-> fullscreen) ---

#[tauri::command]
fn resize_to_bar(app: tauri::AppHandle) {
    // Window is always 640px tall; just reassert bar mode window level + behavior
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_always_on_top(true);
        panel::configure_bar_mode(&window);
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ScreenInfo {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    scale: f64,
    is_main: bool,
    mouse: bool,
    frame_x: f64,
    frame_y: f64,
    frame_w: f64,
    frame_h: f64,
}

/// Get all screens info using compiled Swift helper (JSON output)
fn get_all_screens() -> Vec<ScreenInfo> {
    use std::process::Command;
    let helper = dirs_next::home_dir().unwrap_or_default().join(".mindscope").join("bin").join("screen_info");
    if helper.exists() {
        if let Ok(out) = Command::new(helper.to_str().unwrap()).output() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if let Ok(screens) = serde_json::from_str::<Vec<ScreenInfo>>(&s) {
                if !screens.is_empty() {
                    return screens;
                }
            }
        }
    }
    // Fallback: single screen
    vec![ScreenInfo { x: 0.0, y: 34.0, w: 1440.0, h: 866.0, scale: 2.0, is_main: true, mouse: true, frame_x: 0.0, frame_y: 0.0, frame_w: 1440.0, frame_h: 900.0 }]
}

/// Get the active screen (where mouse is, or main screen as fallback)
fn get_active_screen() -> ScreenInfo {
    let screens = get_all_screens();
    screens.iter().find(|s| s.mouse).cloned()
        .or_else(|| screens.iter().find(|s| s.is_main).cloned())
        .unwrap_or_else(|| screens[0].clone())
}

/// Get the main screen (primary display)
fn get_main_screen() -> ScreenInfo {
    let screens = get_all_screens();
    screens.iter().find(|s| s.is_main).cloned()
        .unwrap_or_else(|| screens[0].clone())
}

#[tauri::command]
fn resize_to_fullscreen(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let screen = get_main_screen();
        // Resize to full screen for rewind mode
        let _ = window.set_size(tauri::LogicalSize::new(screen.frame_w, screen.frame_h));
        let _ = window.set_position(tauri::LogicalPosition::new(screen.frame_x, screen.frame_y));
        let _ = window.set_always_on_top(false);
        panel::configure_fullscreen_mode(&window);
        let _ = window.set_focus();
    }
}

#[tauri::command]
fn resize_to_search(app: tauri::AppHandle) {
    // No-op: window is always fullscreen, CSS handles layout
    let _ = app;
}

/// Expand bar — now a no-op because the window is permanently 640px tall.
/// Kept for backward compatibility with the frontend bindings.
#[tauri::command]
fn expand_bar(_app: tauri::AppHandle) {
    // No resize needed — window is always tall enough to show panels
}

/// Collapse bar — no-op for the same reason as expand_bar.
#[tauri::command]
fn collapse_bar(_app: tauri::AppHandle) {
    // No resize needed
}

/// Hide the window reliably (clickthrough + hide + reset level)
#[tauri::command]
fn hide_window(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        panel::set_clickthrough(&window, true);
        let _ = window.hide();
    }
}

/// Manually trigger a synapse update (refresh working memory via Claude CLI).
#[tauri::command]
async fn synapse_update() -> Result<String, String> {
    tokio::task::spawn_blocking(|| capture::synapse::run_synapse_update())
        .await
        .map_err(|e| format!("task join error: {}", e))?
}

/// Check if synapse is currently syncing.
#[tauri::command]
fn synapse_is_syncing() -> bool {
    capture::synapse::is_syncing()
}

/// Hide all UI — closes panels and hides the window, but keeps background
/// recording/transcription running. Does NOT kill child processes.
#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    // Just hide the window — recording/ffmpeg/hevc continue in background
    if let Some(window) = app.get_webview_window("main") {
        panel::set_clickthrough(&window, true);
        let _ = window.hide();
    }
}

/// Set click-through on/off — frontend calls on mouseenter/mouseleave
#[tauri::command]
fn set_clickthrough(app: tauri::AppHandle, enabled: bool) {
    if let Some(window) = app.get_webview_window("main") {
        panel::set_clickthrough(&window, enabled);
    }
}

/// Set window to bar mode (floating level, collection behavior)
#[tauri::command]
fn set_bar_mode(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        panel::configure_bar_mode(&window);
    }
}

/// Set window to fullscreen overlay mode (screenSaver level)
#[tauri::command]
fn set_fullscreen_mode(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        panel::configure_fullscreen_mode(&window);
    }
}

// --- Audio (user-initiated only, never auto-start) ---

#[tauri::command]
fn check_mic_permission() -> bool {
    audio::check_mic_permission()
}

/// Request mic permission — only call from explicit user button click
#[tauri::command]
fn request_mic_permission() -> bool {
    audio::request_mic_permission()
}

#[tauri::command]
fn start_audio(state: State<'_, ManagedState>) -> Result<bool, String> {
    if !audio::check_mic_permission() {
        return Err("Microphone permission not granted. Click 'Enable' to request permission.".into());
    }
    let state = state.lock().unwrap();
    // Start a manual session (or auto if meeting already active)
    let (existing_id, _) = capture::recorder::get_current_session();
    if existing_id.is_empty() {
        let session_type = capture::recorder::detect_meeting_app_name()
            .unwrap_or_else(|| "manual".to_string());
        capture::recorder::start_audio_session(&session_type);
    }
    Ok(state.audio_recorder.start())
}

#[tauri::command]
fn stop_audio(state: State<'_, ManagedState>) {
    let state = state.lock().unwrap();
    state.audio_recorder.stop();
    // Kill any ffmpeg child processes
    let _ = std::process::Command::new("pkill").args(["-f", "ffmpeg.*avfoundation"]).status();
    // Prevent auto-restart during current meeting
    capture::recorder::suppress_auto_audio();
    // End the session
    capture::recorder::end_audio_session();
}

/// Toggle audio recording on/off (for manual mic button)
#[tauri::command]
fn toggle_audio(state: State<'_, ManagedState>) -> bool {
    let state = state.lock().unwrap();
    let any_recording = state.audio_recorder.is_running() || is_ffmpeg_recording();
    if any_recording {
        state.audio_recorder.stop();
        let _ = std::process::Command::new("pkill").args(["-f", "ffmpeg.*avfoundation"]).status();
        capture::recorder::suppress_auto_audio();
        capture::recorder::end_audio_session();
        // Clear the manual meeting state so the Transcript tab stops showing
        // the "active session" ribbon.
        capture::recorder::mark_manual_meeting_end();
        false
    } else {
        // Starting manually — create a new session
        let session_type = capture::recorder::detect_meeting_app_name()
            .unwrap_or_else(|| "manual".to_string());
        capture::recorder::start_audio_session(&session_type);
        // Publish manual session as an active "meeting" so the frontend
        // Transcript tab polls and renders the live transcript.
        capture::recorder::mark_manual_meeting_start(&session_type);
        state.audio_recorder.start()
    }
}

/// Check if audio is currently recording (manual recorder OR auto ffmpeg)
#[tauri::command]
fn is_audio_recording(state: State<'_, ManagedState>) -> bool {
    let state = state.lock().unwrap();
    state.audio_recorder.is_running() || is_ffmpeg_recording()
}

/// Check if ffmpeg is currently capturing audio
fn is_ffmpeg_recording() -> bool {
    std::process::Command::new("pgrep")
        .args(["-f", "ffmpeg.*avfoundation"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Get meeting state: { active, app_name, recording }
#[tauri::command]
fn get_meeting_state(state: State<'_, ManagedState>) -> serde_json::Value {
    let (active, _app, _start) = capture::recorder::get_meeting_state();
    let recording = state.lock().unwrap().audio_recorder.is_running();
    serde_json::json!({
        "active": active,
        "app": _app,
        "recording": recording
    })
}

#[tauri::command]
fn get_audio_segments(date: String) -> Vec<AudioSegment> {
    audio::load_audio_segments(&date)
}

// --- Timeline (from SQLite) ---

#[tauri::command]
fn get_timeline(date: String) -> Vec<db::FrameRow> {
    db::get_frames_for_date(&date).unwrap_or_default()
}

/// Batch load ALL frame thumbnails for a date — one IPC call, all frames
/// Returns map of image_path → base64 tiny JPEG (resized to 480px wide, ~15KB each)
/// 200 frames × 15KB = 3MB total — fits in memory, enables instant scrubbing
#[tauri::command]
fn get_all_thumbnails(date: String) -> std::collections::HashMap<String, String> {
    let mut result = std::collections::HashMap::new();
    let frames = db::get_frames_for_date(&date).unwrap_or_default();

    // Limit to last 30 frames to avoid CPU overload (was causing 100% CPU + screen freeze)
    let start = if frames.len() > 30 { frames.len() - 30 } else { 0 };
    for frame in &frames[start..] {
        let path = &frame.image_path;
        if path.starts_with("video://") { continue; }
        // Skip huge files (old 7.5MB WebP) — too slow to thumbnail
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.len() > 500_000 { continue; } // Skip files > 500KB
        }
        if let Ok(img) = image::open(path) {
            // Resize to 960px wide thumbnail (sharp enough to read text)
            let thumb = img.thumbnail(960, 600);
            let mut buf = std::io::Cursor::new(Vec::new());
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 50);
            if enc.encode_image(&thumb).is_ok() {
                let b64 = db::base64_encode_bytes(&buf.into_inner());
                result.insert(path.clone(), b64);
            }
        }
    }

    log::info!("MindScope: Loaded {} thumbnails for {}", result.len(), date);
    result
}

#[tauri::command]
fn get_screenshot(image_path: String) -> Option<String> {
    // Handle video:// references (HEVC segments)
    if image_path.starts_with("video://") {
        let rest = &image_path[8..];
        if let Some((seg_path, frame_str)) = rest.rsplit_once('#') {
            let frame_idx: u32 = frame_str.parse().unwrap_or(0);
            return video::extract_frame_base64(seg_path, frame_idx);
        }
        return None;
    }
    // Regular image file
    db::read_image_base64(&image_path)
}

/// Get OCR regions for highlighting — runs OCR on the image
#[tauri::command]
fn get_ocr_regions(image_path: String) -> Vec<capture::ocr::OcrRegion> {
    let path = std::path::Path::new(&image_path);
    if path.exists() {
        capture::ocr::extract_with_regions(path).regions
    } else {
        vec![]
    }
}

// --- AI (Claude CLI via Synapse vault) ---

/// Call Claude CLI with a prompt — runs inside the MindScope vault directory
/// so Claude picks up the bundled CLAUDE.md, skills, and vault knowledge.
/// Falls back gracefully if claude CLI is not installed.
#[tauri::command]
async fn ask_ai(prompt: String) -> Result<String, String> {
    use std::process::Command;

    // Use the MindScope vault as working dir — it contains CLAUDE.md, .claude/skills/,
    // meeting notes, people, and working memory (all set up by synapse::bootstrap()).
    let ms_vault = dirs_next::home_dir().unwrap_or_default().join(".mindscope").join("vault");
    let cwd = if ms_vault.exists() { ms_vault.clone() } else { dirs_next::home_dir().unwrap_or_default() };

    // Enrich prompt with vault context if relevant
    let enriched_prompt = if ms_vault.exists() {
        // Read recent meeting notes for context
        let mut vault_context = String::new();
        if let Ok(entries) = std::fs::read_dir(&ms_vault) {
            let mut files: Vec<_> = entries.flatten().collect();
            files.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
            for entry in files.iter().take(3) {
                if entry.file_name().to_string_lossy().starts_with("meet.") {
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        vault_context.push_str(&content[..content.len().min(500)]);
                        vault_context.push_str("\n---\n");
                    }
                }
            }
        }
        if vault_context.is_empty() {
            prompt.clone()
        } else {
            format!("Vault context (recent meetings):\n{}\n\n{}", vault_context, prompt)
        }
    } else {
        prompt.clone()
    };

    let result = Command::new("/opt/homebrew/bin/claude")
        .args(["-p", &enriched_prompt])
        .current_dir(&cwd)
        .env("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin")
        .output();

    match result {
        Ok(output) if output.status.success() => {
            let response = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if response.is_empty() {
                Err("Empty response".to_string())
            } else {
                Ok(response)
            }
        }
        Ok(output) => {
            let err = String::from_utf8_lossy(&output.stderr);
            Err(format!("Claude error: {}", err.chars().take(200).collect::<String>()))
        }
        Err(e) => Err(format!("Claude not found: {}", e))
    }
}

// --- Search (FTS5) ---

#[tauri::command]
fn search(query: String) -> Vec<db::FrameRow> {
    db::search_frames(&query, 50).unwrap_or_default()
}

// --- Storage ---

#[tauri::command]
fn get_storage_info() -> Option<db::StorageStats> {
    db::get_storage_stats().ok()
}

#[tauri::command]
fn cleanup_old_data(retention_days: u32) -> u32 {
    db::cleanup_old_frames(retention_days).unwrap_or(0)
}

// --- Settings ---

#[tauri::command]
fn get_settings() -> capture::settings::AppSettings {
    capture::settings::load_settings()
}

#[tauri::command]
fn update_settings(settings: serde_json::Value) -> capture::settings::AppSettings {
    let mut current = capture::settings::load_settings();
    if let Some(v) = settings.get("retention_days").and_then(|v| v.as_u64()) { current.retention_days = v as u32; }
    if let Some(v) = settings.get("capture_interval_secs").and_then(|v| v.as_u64()) { current.capture_interval_secs = v; }
    if let Some(v) = settings.get("jpeg_quality").and_then(|v| v.as_f64()) { current.jpeg_quality = v as f32; }
    if let Some(v) = settings.get("idle_threshold_secs").and_then(|v| v.as_u64()) { current.idle_threshold_secs = v; }
    if let Some(v) = settings.get("capture_audio").and_then(|v| v.as_bool()) { current.capture_audio = v; }
    if let Some(v) = settings.get("transcription_engine").and_then(|v| v.as_str()) { current.transcription_engine = Some(v.to_string()); }
    if let Some(v) = settings.get("excluded_apps").and_then(|v| v.as_array()) {
        current.excluded_apps = v.iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect();
    }
    if let Some(v) = settings.get("private_browsing").and_then(|v| v.as_bool()) { current.private_browsing = v; }
    capture::settings::save_settings(&current);
    current
}

// --- Meeting status (real-time polling) ---

#[tauri::command]
fn get_meeting_status() -> serde_json::Value {
    let (active, app_name, start_time) = capture::recorder::get_meeting_state();

    let mut recent_transcripts = Vec::new();
    if active && start_time > 0 {
        let date = {
            let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
            let d = s / 86400;
            let mut y = 1970u64; let mut r = d;
            loop { let dy = if (y%4==0&&y%100!=0)||y%400==0 {366} else {365}; if r<dy {break;} r-=dy; y+=1; }
            let l = (y%4==0&&y%100!=0)||y%400==0;
            let md = if l {[31,29,31,30,31,30,31,31,30,31,30,31]} else {[31,28,31,30,31,30,31,31,30,31,30,31]};
            let mut m = 0u64;
            for (i,&v) in md.iter().enumerate() { if r<v {m=i as u64+1; break;} r-=v; }
            if m==0 {m=12;} format!("{:04}-{:02}-{:02}",y,m,r+1)
        };
        let segments = audio::load_audio_segments(&date);
        let mut matched: Vec<_> = segments.into_iter()
            .filter(|seg| !seg.transcript.is_empty())
            .collect();
        if matched.len() > 10 {
            matched = matched.split_off(matched.len() - 10);
        }
        for seg in matched {
            let time_str = if seg.timestamp.len() >= 16 {
                seg.timestamp[11..16].to_string()
            } else {
                seg.timestamp.clone()
            };
            recent_transcripts.push(serde_json::json!({
                "time": time_str,
                "speaker": "Speaker",
                "text": seg.transcript,
            }));
        }
    }

    serde_json::json!({
        "active": active,
        "app_name": app_name,
        "start_time": start_time,
        "recent_transcripts": recent_transcripts,
    })
}

// --- Apps list ---

#[tauri::command]
fn get_all_apps() -> Vec<String> {
    db::get_all_apps().unwrap_or_default()
}

// --- Whisper ---

#[tauri::command]
fn is_whisper_available() -> bool {
    whisper::is_model_available()
}

#[tauri::command]
async fn download_whisper_model() -> Result<(), String> {
    whisper::download_model()
}

// --- Pipes (Automation) ---

#[tauri::command]
fn list_pipes() -> Vec<capture::pipes::PipeInfo> {
    capture::pipes::list_pipes()
}

#[tauri::command]
fn create_pipe(id: String, config: capture::pipes::PipeConfig) -> Result<(), String> {
    capture::pipes::create_pipe(&id, config)
}

#[tauri::command]
fn set_pipe_enabled(id: String, enabled: bool) -> Result<(), String> {
    capture::pipes::set_pipe_enabled(&id, enabled)
}

#[tauri::command]
async fn run_pipe(id: String) -> Result<capture::pipes::PipeResult, String> {
    capture::pipes::run_pipe(&id)
}

// --- Vault Sync (auto-sync screen data to knowledge vault) ---

#[tauri::command]
fn get_daily_brief() -> String {
    capture::vault_sync::generate_daily_brief()
}

#[tauri::command]
async fn generate_journal(date: String) -> Result<(), String> {
    capture::vault_sync::generate_daily_journal(&date);
    Ok(())
}

// --- Vault (people/meetings/projects) ---

use vault::parser;
use vault::types::*;

#[tauri::command]
fn list_people(state: State<'_, ManagedState>) -> Vec<Person> {
    let s = state.lock().unwrap();
    parser::parse_people(&s.vault_path)
}

#[tauri::command]
fn list_meetings(state: State<'_, ManagedState>) -> Vec<Meeting> {
    let s = state.lock().unwrap();
    parser::parse_meetings(&s.vault_path)
}

#[tauri::command]
fn list_projects(state: State<'_, ManagedState>) -> Vec<Project> {
    let s = state.lock().unwrap();
    parser::parse_projects(&s.vault_path)
}

// ===== App Entry =====

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let vault_path = dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".mindscope")
        .join("vault");

    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_log::Builder::default().level(log::LevelFilter::Info).build())
        .manage(Mutex::new(AppState {
            vault_path,
            recorder: Recorder::new(2),  // 2-second interval (was 5)
            audio_recorder: AudioRecorder::new(2),
        }))
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Init DB
            if let Err(e) = db::init_db() {
                log::error!("MindScope: DB init failed: {}", e);
            }

            // Start Axum API server (ported from Screenpipe's pattern)
            std::thread::spawn(|| {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    if let Err(e) = capture::frame_server::start(9457).await {
                        log::error!("MindScope: API server failed: {}", e);
                    }
                });
            });

            // Create default pipes if not exist
            capture::pipes::ensure_default_pipes();

            // Start pipe scheduler (checks every 60s for scheduled pipes)
            capture::pipes::start_scheduler();

            // Start vault auto-sync loop (hourly working memory + daily journal)
            capture::vault_sync::start_vault_sync_loop();

            // Bootstrap synapse — copies skills, CLAUDE.md, seed working memory
            // on first run. Safe to call every startup (won't overwrite edits).
            capture::synapse::bootstrap();

            // Start synapse background loop — every 30 min, refresh working memory
            // via Claude CLI running against ~/.mindscope/vault/
            capture::synapse::start_synapse_loop();

            // Auto-start screen recording only
            // Audio recording is disabled by default — it triggers
            // macOS microphone permission dialogs. Users can enable
            // it manually via Settings > Audio.
            {
                let state: State<'_, ManagedState> = app.state();
                let s = state.lock().unwrap();
                let started = s.recorder.start();
                let _ = std::fs::write(
                    db::data_dir().join("debug.log"),
                    format!("Recorder start result: {}\n", started)
                );
                // s.audio_recorder.start(); // Disabled: triggers permission dialog
            }

            // Fullscreen transparent window on main screen, start hidden.
            // CSS handles all layout — bar lives at the bottom of screen.h
            // (visibleFrame = excludes Dock + menu bar automatically).
            // This matches the stable backup version that worked correctly.
            if let Some(window) = app.get_webview_window("main") {
                let screen = get_main_screen();
                let _ = window.set_size(tauri::LogicalSize::new(screen.frame_w, screen.frame_h));
                let _ = window.set_position(tauri::LogicalPosition::new(screen.frame_x, screen.frame_y));
                panel::configure_bar_mode(&window);
                let _ = window.hide();
            }

            // Tray icon — click toggles bar
            let _tray = tauri::tray::TrayIconBuilder::new()
                .tooltip("MindScope")
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click { .. } = event {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            if w.is_visible().unwrap_or(false) {
                                panel::set_clickthrough(&w, true);
                                let _ = w.hide();
                            } else {
                                let _ = w.show();
                                let _ = w.set_focus();
                                // Default to catching clicks so bar receives interaction;
                                // the bar's onMouseLeave enables click-through when the
                                // cursor leaves the bar area.
                                panel::set_clickthrough(&w, false);
                            }
                        }
                    }
                })
                .build(app)?;

            // Cmd+Shift+Space — toggle full overlay
            use tauri_plugin_global_shortcut::GlobalShortcutExt;
            let ah = app.handle().clone();
            app.global_shortcut().on_shortcut("CmdOrCtrl+Shift+Space", move |_app, _sc, event| {
                if event.state == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                    if let Some(w) = ah.get_webview_window("main") {
                        if w.is_visible().unwrap_or(false) {
                            // Ensure clickthrough before hiding so desktop isn't blocked
                            panel::set_clickthrough(&w, true);
                            let _ = w.hide();
                        } else {
                            let _ = w.show();
                            let _ = w.set_focus();
                            panel::set_clickthrough(&w, false);
                            panel::configure_bar_mode(&w);
                        }
                    }
                }
            })?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            check_permission,
            open_permission_settings,
            resize_to_bar,
            resize_to_fullscreen,
            resize_to_search,
            start_recording,
            stop_recording,
            is_recording,
            check_mic_permission,
            request_mic_permission,
            start_audio,
            stop_audio,
            is_audio_recording,
            toggle_audio,
            get_meeting_state,
            get_audio_segments,
            get_timeline,
            get_all_thumbnails,
            get_screenshot,
            get_ocr_regions,
            ask_ai,
            search,
            get_storage_info,
            cleanup_old_data,
            get_settings,
            update_settings,
            list_people,
            list_meetings,
            list_projects,
            hide_window,
            quit_app,
            synapse_update,
            synapse_is_syncing,
            expand_bar,
            collapse_bar,
            set_clickthrough,
            set_bar_mode,
            set_fullscreen_mode,
            is_whisper_available,
            download_whisper_model,
            list_pipes,
            create_pipe,
            set_pipe_enabled,
            run_pipe,
            get_daily_brief,
            generate_journal,
            get_all_apps,
            get_meeting_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running MindScope");
}
