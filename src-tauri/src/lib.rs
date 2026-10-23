mod capture;
mod vault;

use std::path::PathBuf;
use std::sync::Mutex;

use tauri::{Manager, State};

use capture::audio::{self, AudioSegment};
use capture::db;
use capture::panel;
use capture::permissions;
use capture::recorder::{self, Recorder};
use capture::util::LockExt;
use capture::video;
use capture::whisper;
use capture::{claude_cli, frame_server, paths, settings, text, timeutil};

// App state
struct AppState {
    vault_path: PathBuf,
    recorder: Recorder,
}

type ManagedState = Mutex<AppState>;

/// Run a blocking job off the async runtime's worker threads.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("task join error: {}", e))
}

// --- Permissions (real TCC state, no heuristics) ---

#[tauri::command]
fn check_permission() -> bool {
    permissions::screen_capture_granted()
}

/// Ask macOS for Screen Recording (shows the system dialog once).
#[tauri::command]
fn request_screen_permission() -> bool {
    permissions::request_screen_capture()
}

#[tauri::command]
fn open_permission_settings() {
    permissions::open_screen_recording_settings();
}

/// Token + port for the local HTTP API (the webview appends `?token=`).
#[tauri::command]
fn get_api_info() -> serde_json::Value {
    serde_json::json!({ "port": frame_server::PORT, "token": frame_server::api_token() })
}

// --- Recording ---

#[tauri::command]
fn start_recording(state: State<'_, ManagedState>) -> bool {
    state.lock_or_recover().recorder.start()
}

#[tauri::command]
fn stop_recording(state: State<'_, ManagedState>) {
    state.lock_or_recover().recorder.stop();
}

#[tauri::command]
fn is_recording(state: State<'_, ManagedState>) -> bool {
    state.lock_or_recover().recorder.is_running()
}

// --- Window resize (bar <-> fullscreen) ---

#[tauri::command]
fn resize_to_bar(app: tauri::AppHandle) {
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

/// All screens (bundled `screen_info` helper), with a single-screen fallback.
fn get_all_screens() -> Vec<ScreenInfo> {
    if let Some(screens) = capture::helpers::run_json::<Vec<ScreenInfo>>("screen_info", &[]) {
        if !screens.is_empty() { return screens; }
    }
    vec![ScreenInfo { x: 0.0, y: 34.0, w: 1440.0, h: 866.0, scale: 2.0, is_main: true, mouse: true, frame_x: 0.0, frame_y: 0.0, frame_w: 1440.0, frame_h: 900.0 }]
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

/// Kept for backward compatibility with the frontend bindings (window is a fixed size).
#[tauri::command]
fn expand_bar(_app: tauri::AppHandle) {}

#[tauri::command]
fn collapse_bar(_app: tauri::AppHandle) {}

/// Hide the window reliably (clickthrough + hide)
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
    blocking(capture::synapse::run_synapse_update).await?
}

#[tauri::command]
fn synapse_is_syncing() -> bool {
    capture::synapse::is_syncing()
}

/// Run the deep "dreaming" consolidation pass on demand (merge/dedup/decay the
/// fact store, re-render cold files, enforce the byte ceiling).
#[tauri::command]
async fn synapse_deep_pass() -> Result<(usize, usize, u64), String> {
    blocking(|| {
        capture::memory::deep_pass();
        capture::memory::stats()
    })
    .await
}

/// Forget every fact derived from one source (privacy / correctness).
/// `source` is a provenance token such as `day:2026-10-23`, `app:Safari`,
/// `frame:123`, or a prefix like `day:` to forget all days.
#[tauri::command]
async fn memory_forget_source(source: String) -> Result<usize, String> {
    blocking(move || capture::memory::forget_source(&source)).await
}

/// Hide all UI — background recording/transcription keeps running.
#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        panel::set_clickthrough(&window, true);
        let _ = window.hide();
    }
}

#[tauri::command]
fn set_clickthrough(app: tauri::AppHandle, enabled: bool) {
    if let Some(window) = app.get_webview_window("main") {
        panel::set_clickthrough(&window, enabled);
    }
}

#[tauri::command]
fn set_bar_mode(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        panel::configure_bar_mode(&window);
    }
}

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

/// Request mic permission — only call from an explicit user button click.
#[tauri::command]
async fn request_mic_permission() -> Result<bool, String> {
    blocking(audio::request_mic_permission).await
}

fn start_manual_audio() -> Result<bool, String> {
    recorder::log_meeting_event("start_audio ENTRY", recorder::get_meeting_state().0);
    // The auto meeting recorder already has the mic: one ffmpeg per mic is enough.
    if recorder::auto_audio().is_running() {
        return Ok(true);
    }
    // The user asked for recording — undo a previous "stop" so nothing fights it.
    recorder::clear_audio_suppression();
    // Start a manual session (or piggyback on the auto session if one is active)
    let (existing_id, existing_type) = recorder::get_current_session();
    let session_type = if existing_id.is_empty() {
        let t = recorder::detect_meeting_app_name().unwrap_or_else(|| "manual".to_string());
        recorder::start_audio_session(&t);
        t
    } else {
        existing_type
    };
    // Publish the manual session as an active "meeting" so the Transcript tab
    // polls and renders the live transcript + red dot.
    recorder::mark_manual_meeting_start(&session_type);
    match recorder::manual_audio().start() {
        Ok(()) => Ok(true),
        Err(audio::StartError::AlreadyRunning) => Ok(true),
        Err(e) => {
            recorder::mark_manual_meeting_end();
            recorder::end_audio_session();
            Err(match e {
                audio::StartError::NoPermission => "Microphone permission not granted. Click 'Enable' to request permission.".to_string(),
                audio::StartError::FfmpegMissing => "ffmpeg not found. Install it (brew install ffmpeg) or set ffmpeg_path in settings.json.".to_string(),
                audio::StartError::AlreadyRunning => unreachable!(),
            })
        }
    }
}

fn stop_manual_audio() {
    recorder::log_meeting_event("stop_audio ENTRY", recorder::get_meeting_state().0);
    recorder::manual_audio().stop();
    recorder::auto_audio().stop();
    // Prevent auto-restart during the current meeting
    recorder::suppress_auto_audio();
    recorder::end_audio_session();
    recorder::mark_manual_meeting_end();
}

#[tauri::command]
async fn start_audio() -> Result<bool, String> {
    blocking(start_manual_audio).await?
}

#[tauri::command]
async fn stop_audio() -> Result<(), String> {
    blocking(stop_manual_audio).await
}

/// Toggle audio recording on/off (manual mic button)
#[tauri::command]
async fn toggle_audio() -> Result<bool, String> {
    blocking(|| {
        if recorder::is_any_audio_running() {
            recorder::log_meeting_event("toggle_audio branch=STOP", true);
            stop_manual_audio();
            Ok(false)
        } else {
            recorder::log_meeting_event("toggle_audio branch=START", false);
            start_manual_audio()
        }
    }).await?
}

/// Is audio currently recording (manual recorder OR auto meeting recorder)?
/// Tracked child handles, no `pgrep`.
#[tauri::command]
async fn is_audio_recording() -> bool {
    recorder::is_any_audio_running()
}

/// Get meeting state: { active, app_name, recording }
#[tauri::command]
fn get_meeting_state() -> serde_json::Value {
    let (active, app, _start) = recorder::get_meeting_state();
    serde_json::json!({
        "active": active,
        "app": app,
        "recording": recorder::is_any_audio_running(),
    })
}

#[tauri::command]
async fn get_audio_segments(date: String) -> Result<Vec<AudioSegment>, String> {
    blocking(move || audio::load_audio_segments(&date)).await
}

// --- Timeline (from SQLite) ---

/// Light rows for a local date: id, timestamp, app, window. Images are fetched by id.
#[tauri::command]
async fn get_timeline(date: String) -> Result<Vec<db::FrameLite>, String> {
    blocking(move || db::get_frames_lite_for_date(&date, None).unwrap_or_default()).await
}

/// Incremental poll: only rows with id > `after_id` for that date.
#[tauri::command]
async fn get_timeline_since(date: String, after_id: i64) -> Result<Vec<db::FrameLite>, String> {
    blocking(move || db::get_frames_lite_for_date(&date, Some(after_id)).unwrap_or_default()).await
}

/// Batch-load thumbnails (last 30 JPEG frames of a date) as base64 — off the main thread.
#[tauri::command]
async fn get_all_thumbnails(date: String) -> Result<std::collections::HashMap<String, String>, String> {
    blocking(move || {
        let mut result = std::collections::HashMap::new();
        let frames = db::get_recent_frames_for_date(&date, 30).unwrap_or_default();
        for frame in &frames {
            let path = &frame.image_path;
            if path.starts_with("video://") { continue; }
            if let Ok(meta) = std::fs::metadata(path) {
                if meta.len() > 500_000 { continue; }
            }
            if let Ok(img) = image::open(path) {
                let thumb = img.thumbnail(960, 600);
                if let Some(bytes) = capture::screenshot::encode_jpeg(&thumb, 50) {
                    result.insert(path.clone(), db::base64_encode_bytes(&bytes));
                }
            }
        }
        result
    }).await
}

#[tauri::command]
async fn get_screenshot(image_path: String) -> Result<Option<String>, String> {
    blocking(move || {
        if let Some(rest) = image_path.strip_prefix("video://") {
            if let Some((seg_path, frame_str)) = rest.rsplit_once('#') {
                let frame_idx: u32 = frame_str.parse().unwrap_or(0);
                return video::extract_frame_base64(seg_path, frame_idx);
            }
            return None;
        }
        db::read_image_base64(&image_path)
    }).await
}

/// OCR regions for highlighting — stored regions from the DB; OCR only as a fallback.
#[tauri::command]
async fn get_ocr_regions(image_path: String) -> Result<Vec<capture::ocr::OcrRegion>, String> {
    blocking(move || {
        if let Ok(Some(frame)) = db::get_frame_by_path(&image_path) {
            let regions = frame.regions();
            if !regions.is_empty() { return regions; }
        }
        let path = std::path::Path::new(&image_path);
        if path.starts_with(paths::frames_root()) && path.exists() {
            capture::ocr::extract_with_regions(path).regions
        } else {
            vec![]
        }
    }).await
}

// --- AI (Claude CLI) ---

/// Ask Claude with the prompt the UI built (search + timeline context inlined).
#[tauri::command]
async fn ask_ai(prompt: String) -> Result<String, String> {
    blocking(move || {
        let ms_vault = paths::vault_dir();
        let enriched_prompt = if ms_vault.exists() {
            let mut vault_context = String::new();
            if let Ok(entries) = std::fs::read_dir(&ms_vault) {
                let mut files: Vec<_> = entries.flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with("meet."))
                    .collect();
                files.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
                for entry in files.iter().take(3) {
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        vault_context.push_str(text::truncate_chars(&content, 500));
                        vault_context.push_str("\n---\n");
                    }
                }
            }
            if vault_context.is_empty() { prompt.clone() } else {
                format!("Vault context (recent meetings):\n{}\n\n{}", vault_context, prompt)
            }
        } else {
            prompt.clone()
        };
        claude_cli::run("ask-ai", &enriched_prompt, claude_cli::CallOpts::interactive())
    }).await?
}

// --- Search (FTS5) ---

#[tauri::command]
async fn search(query: String) -> Result<Vec<db::FrameRow>, String> {
    blocking(move || db::search_frames(&query, 50).unwrap_or_default()).await
}

// --- Storage ---

#[tauri::command]
async fn get_storage_info() -> Result<Option<db::StorageStats>, String> {
    blocking(|| db::get_storage_stats().ok()).await
}

#[tauri::command]
async fn cleanup_old_data(retention_days: u32) -> Result<u32, String> {
    blocking(move || capture::retention::run_now(retention_days)).await
}

// --- Settings ---

#[tauri::command]
fn get_settings() -> settings::AppSettings {
    settings::load_settings()
}

#[tauri::command]
fn update_settings(settings: serde_json::Value) -> settings::AppSettings {
    let mut current = capture::settings::load_settings();
    if let Some(v) = settings.get("retention_days").and_then(|v| v.as_u64()) { current.retention_days = v as u32; }
    if let Some(v) = settings.get("capture_interval_secs").and_then(|v| v.as_u64()) { current.capture_interval_secs = v.clamp(1, 120); }
    if let Some(v) = settings.get("jpeg_quality").and_then(|v| v.as_f64()) { current.jpeg_quality = (v as f32).clamp(0.1, 1.0); }
    if let Some(v) = settings.get("idle_threshold_secs").and_then(|v| v.as_u64()) { current.idle_threshold_secs = v; }
    if let Some(v) = settings.get("capture_audio").and_then(|v| v.as_bool()) { current.capture_audio = v; }
    if let Some(v) = settings.get("transcription_engine").and_then(|v| v.as_str()) { current.transcription_engine = Some(v.to_string()); }
    if let Some(v) = settings.get("excluded_apps").and_then(|v| v.as_array()) {
        current.excluded_apps = v.iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect();
    }
    if let Some(v) = settings.get("private_browsing").and_then(|v| v.as_bool()) { current.private_browsing = v; }
    if let Some(v) = settings.get("ffmpeg_path") { current.ffmpeg_path = v.as_str().map(|s| s.to_string()).filter(|s| !s.is_empty()); }
    if let Some(v) = settings.get("claude_path") { current.claude_path = v.as_str().map(|s| s.to_string()).filter(|s| !s.is_empty()); }
    if let Some(v) = settings.get("storage_budget_mb").and_then(|v| v.as_u64()) { current.storage_budget_mb = v; }
    if let Some(v) = settings.get("claude_daily_budget").and_then(|v| v.as_u64()) { current.claude_daily_budget = v as u32; }
    if let Some(v) = settings.get("capture_height").and_then(|v| v.as_u64()) { current.capture_height = (v as u32).clamp(360, 4320); }
    capture::settings::save_settings(&current);
    current
}

// --- Meeting status (real-time polling) ---

#[tauri::command]
async fn get_meeting_status() -> Result<serde_json::Value, String> {
    blocking(|| {
        let (active, app_name, start_time) = recorder::get_meeting_state();

        let mut recent_transcripts = Vec::new();
        if active && start_time > 0 {
            let segments = audio::load_audio_segments(&timeutil::today());
            let mut matched: Vec<_> = segments.into_iter()
                .filter(|seg| !seg.transcript.is_empty())
                .filter(|seg| timeutil::parse_local_datetime_micros(&seg.timestamp)
                    .map(|ts| ts >= start_time - 5_000_000).unwrap_or(false))
                .collect();
            if matched.len() > 10 {
                matched = matched.split_off(matched.len() - 10);
            }
            for seg in matched {
                let time_str = seg.timestamp.get(11..16).unwrap_or(&seg.timestamp).to_string();
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
            "recording": recorder::is_any_audio_running(),
            "recent_transcripts": recent_transcripts,
        })
    }).await
}

// --- Apps list ---

#[tauri::command]
async fn get_all_apps() -> Result<Vec<String>, String> {
    blocking(|| db::get_all_apps().unwrap_or_default()).await
}

// --- Whisper ---

#[tauri::command]
fn is_whisper_available() -> bool {
    whisper::is_model_available()
}

#[tauri::command]
async fn download_whisper_model() -> Result<(), String> {
    blocking(whisper::download_model).await?
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
    blocking(move || capture::pipes::run_pipe(&id)).await?
}

// --- Vault Sync (auto-sync screen data to knowledge vault) ---

#[tauri::command]
async fn get_daily_brief() -> Result<String, String> {
    blocking(capture::vault_sync::generate_daily_brief).await
}

#[tauri::command]
async fn generate_journal(date: String) -> Result<(), String> {
    blocking(move || capture::vault_sync::generate_daily_journal(&date)).await
}

// --- Vault (people/meetings/projects) ---

use vault::parser;
use vault::types::*;

fn vault_path(state: &State<'_, ManagedState>) -> PathBuf {
    state.lock_or_recover().vault_path.clone()
}

#[tauri::command]
async fn list_people(state: State<'_, ManagedState>) -> Result<Vec<Person>, String> {
    let p = vault_path(&state);
    blocking(move || parser::parse_people(&p)).await
}

#[tauri::command]
async fn list_meetings(state: State<'_, ManagedState>) -> Result<Vec<Meeting>, String> {
    let p = vault_path(&state);
    blocking(move || parser::parse_meetings(&p)).await
}

#[tauri::command]
async fn list_projects(state: State<'_, ManagedState>) -> Result<Vec<Project>, String> {
    let p = vault_path(&state);
    blocking(move || parser::parse_projects(&p)).await
}

// ===== App Entry =====

/// Parse `forget --source <token>` (in either order) from the process args.
/// Returns the source token when the `forget` subcommand is present.
fn parse_forget_source_arg() -> Option<String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.iter().any(|a| a == "forget") {
        return None;
    }
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--source" || a == "-s" {
            return it.next().filter(|v| !v.is_empty()).cloned();
        }
        if let Some(v) = a.strip_prefix("--source=") {
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Resolve $MINDSCOPE_HOME once (also exported for the Swift helpers).
    let home = paths::mindscope_home();
    let vault_path = paths::vault_dir();

    // CLI: `mindscope forget --source <token>` removes every fact derived from
    // one source (frame/day/app), then exits without launching the UI.
    if let Some(src) = parse_forget_source_arg() {
        let n = capture::memory::forget_source(&src);
        println!("forgot {} fact(s) derived from source {:?}", n, src);
        return;
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_log::Builder::default().level(log::LevelFilter::Info).build())
        .manage(Mutex::new(AppState {
            vault_path,
            recorder: Recorder::new(),
        }))
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            log::info!("MindScope: data root {:?}", home);
            let missing = capture::helpers::missing();
            if missing.is_empty() {
                log::info!("MindScope: all {} Swift helpers found", capture::helpers::NAMES.len());
            } else {
                log::error!("MindScope: missing Swift helpers {:?} — rebuild (build.rs compiles swift/*.swift)", missing);
            }
            match capture::tools::find_ffmpeg() {
                Some(p) => log::info!("MindScope: ffmpeg at {:?}", p),
                None => log::warn!("MindScope: ffmpeg not found; audio capture disabled until installed or ffmpeg_path is set"),
            }
            match capture::tools::find_claude() {
                Some(p) => log::info!("MindScope: claude CLI at {:?}", p),
                None => log::warn!("MindScope: claude CLI not found; AI features disabled"),
            }

            // Init DB
            if let Err(e) = db::init_db() {
                log::error!("MindScope: DB init failed: {}", e);
            }

            // Local HTTP API (token-protected, 127.0.0.1 only)
            std::thread::spawn(|| {
                let rt = match tokio::runtime::Runtime::new() {
                    Ok(rt) => rt,
                    Err(e) => { log::error!("MindScope: API runtime failed: {}", e); return; }
                };
                rt.block_on(async {
                    if let Err(e) = frame_server::start(frame_server::PORT).await {
                        log::error!("MindScope: API server failed: {}", e);
                    }
                });
            });

            capture::pipes::ensure_default_pipes();
            capture::pipes::start_scheduler();
            capture::vault_sync::start_vault_sync_loop();
            capture::synapse::bootstrap();
            capture::synapse::start_synapse_loop();
            capture::retention::start_retention_loop();

            // Auto-start screen recording only. Audio starts on meeting detection
            // (if mic permission was granted) or from the mic button.
            {
                let state: State<'_, ManagedState> = app.state();
                let started = state.lock_or_recover().recorder.start();
                log::info!("MindScope: recorder start result: {}", started);
            }

            // Fullscreen transparent window on main screen, start hidden.
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
                                panel::set_clickthrough(&w, false);
                            }
                        }
                    }
                })
                .build(app)?;

            // Cmd+Shift+Space — toggle full overlay
            use tauri_plugin_global_shortcut::GlobalShortcutExt;
            let ah = app.handle().clone();
            let shortcut = app.global_shortcut().on_shortcut("CmdOrCtrl+Shift+Space", move |_app, _sc, event| {
                if event.state == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                    if let Some(w) = ah.get_webview_window("main") {
                        if w.is_visible().unwrap_or(false) {
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
            });
            if let Err(e) = shortcut {
                // Another MindScope instance (or another app) owns the shortcut; keep running.
                log::warn!("MindScope: could not register Cmd+Shift+Space ({}); use the tray icon", e);
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            check_permission,
            request_screen_permission,
            open_permission_settings,
            get_api_info,
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
            get_timeline_since,
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
            synapse_deep_pass,
            memory_forget_source,
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
