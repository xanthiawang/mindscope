//! Screen recorder loop + meeting detection state machine.
//!
//! Capture cycle (every `capture_interval_secs`):
//!   topmost window → (exclusions) → capture ONE display, downscaled →
//!   perceptual-hash dedup against the last kept frame (in memory, so it works
//!   even though the HEVC encoder deletes the JPEG right away) → JPEG → OCR →
//!   buffered DB insert → encoder; encoder replies carry the frame timestamp so
//!   exactly that row is re-pointed at `video://segment#frame`.
//!
//! Meeting detection polls the `is_meeting` helper every few seconds and needs
//! BOTH a meeting app and real microphone use (by a process other than us).
//! Hysteresis: two positive probes start a session, 30 s without a positive
//! probe ends it, which runs the end path (stop audio, unload Whisper, write
//! vault notes) that was previously unreachable.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use image::DynamicImage;

use super::audio::{self, AudioRecorder};
use super::db;
use super::ocr;
use super::paths;
use super::permissions;
use super::pii;
use super::screenshot;
use super::settings::{self, AppSettings};
use super::text;
use super::timeutil;
use super::util::{self, LockExt};
use super::video;

// ---------------------------------------------------------------------------
// Global meeting / session state — polled by the UI via Tauri commands + HTTP
// ---------------------------------------------------------------------------

static MEETING_ACTIVE: AtomicBool = AtomicBool::new(false);
static MEETING_APP: Mutex<Option<String>> = Mutex::new(None);
static MEETING_START: AtomicI64 = AtomicI64::new(0);
/// User manually stopped auto-recording this call — don't restart until the call ends.
static MEETING_AUDIO_SUPPRESSED: AtomicBool = AtomicBool::new(false);

static CURRENT_SESSION_ID: Mutex<Option<String>> = Mutex::new(None);
static CURRENT_SESSION_TYPE: Mutex<Option<String>> = Mutex::new(None);

/// Auto (meeting) recorder and manual (mic button) recorder. Both global so the
/// UI can answer "is audio recording?" without a process scan.
static AUTO_AUDIO: OnceLock<AudioRecorder> = OnceLock::new();
static MANUAL_AUDIO: OnceLock<AudioRecorder> = OnceLock::new();

pub fn auto_audio() -> &'static AudioRecorder { AUTO_AUDIO.get_or_init(|| AudioRecorder::new(audio::RecorderKind::Auto)) }
pub fn manual_audio() -> &'static AudioRecorder { MANUAL_AUDIO.get_or_init(|| AudioRecorder::new(audio::RecorderKind::Manual)) }

/// The user explicitly asked for recording: lift any "stop" suppression.
pub fn clear_audio_suppression() {
    MEETING_AUDIO_SUPPRESSED.store(false, Ordering::SeqCst);
}

/// Is any audio capture (auto or manual) running?
pub fn is_any_audio_running() -> bool {
    auto_audio().is_running() || manual_audio().is_running()
}

/// PIDs of our own ffmpeg children (excluded from mic-in-use detection).
pub fn audio_child_pids() -> Vec<u32> {
    [auto_audio().child_pid(), manual_audio().child_pid()].into_iter().flatten().collect()
}

/// Append one line to data/meeting_debug.log on meeting state transitions.
pub fn log_meeting_event(reason: &str, new_value: bool) {
    let path = paths::meeting_log_path();
    util::rotate_log(&path, 2 * 1024 * 1024);
    util::append_line(&path, &format!(
        "{} MEETING_ACTIVE={} reason={}",
        timeutil::local_timestamp(), new_value, reason
    ));
}

/// Start a new audio session. `session_type` is the app label ("Zoom", ...) or "manual".
pub fn start_audio_session(session_type: &str) {
    *CURRENT_SESSION_ID.lock_or_recover() = Some(timeutil::local_timestamp());
    *CURRENT_SESSION_TYPE.lock_or_recover() = Some(session_type.to_string());
    log::info!("MindScope: Audio session started ({})", session_type);
}

pub fn end_audio_session() {
    *CURRENT_SESSION_ID.lock_or_recover() = None;
    *CURRENT_SESSION_TYPE.lock_or_recover() = None;
    log::info!("MindScope: Audio session ended");
}

/// Current session (id, type); empty strings when none.
pub fn get_current_session() -> (String, String) {
    let id = CURRENT_SESSION_ID.lock_or_recover().clone().unwrap_or_default();
    let ty = CURRENT_SESSION_TYPE.lock_or_recover().clone().unwrap_or_default();
    (id, ty)
}

/// User manually stopped audio during a meeting — prevents auto-restart for this call.
pub fn suppress_auto_audio() {
    MEETING_AUDIO_SUPPRESSED.store(true, Ordering::SeqCst);
}

pub fn is_audio_suppressed() -> bool {
    MEETING_AUDIO_SUPPRESSED.load(Ordering::SeqCst)
}

/// (active, app_name, start_time_epoch_micros)
pub fn get_meeting_state() -> (bool, String, i64) {
    let active = MEETING_ACTIVE.load(Ordering::SeqCst);
    let app = MEETING_APP.lock_or_recover().clone().unwrap_or_default();
    let start = MEETING_START.load(Ordering::SeqCst);
    (active, app, start)
}

fn set_meeting_active(app: &str, start_micros: i64, reason: &str) {
    MEETING_ACTIVE.store(true, Ordering::SeqCst);
    MEETING_START.store(start_micros, Ordering::SeqCst);
    *MEETING_APP.lock_or_recover() = Some(app.to_string());
    log_meeting_event(reason, true);
}

fn clear_meeting_active(reason: &str) {
    MEETING_ACTIVE.store(false, Ordering::SeqCst);
    MEETING_START.store(0, Ordering::SeqCst);
    *MEETING_APP.lock_or_recover() = None;
    log_meeting_event(reason, false);
}

/// Mark the start of a manual recording session (mic button pressed).
pub fn mark_manual_meeting_start(label: &str) {
    set_meeting_active(label, timeutil::now_micros(), &format!("mark_manual_meeting_start({})", label));
}

/// Mark the end of a manual recording session (mic button released).
pub fn mark_manual_meeting_end() {
    clear_meeting_active("mark_manual_meeting_end");
}

/// Dump all meeting-related state for the /debug/meeting endpoint.
pub fn dump_debug_state() -> serde_json::Value {
    let (active, app, start) = get_meeting_state();
    let (session_id, session_type) = get_current_session();
    serde_json::json!({
        "MEETING_ACTIVE": active,
        "MEETING_APP": app,
        "MEETING_START": start,
        "MEETING_AUDIO_SUPPRESSED": is_audio_suppressed(),
        "CURRENT_SESSION_ID": session_id,
        "CURRENT_SESSION_TYPE": session_type,
        "auto_audio_running": auto_audio().is_running(),
        "manual_audio_running": manual_audio().is_running(),
        "whisper_loaded": super::whisper::is_loaded(),
        "audio_child_pids": audio_child_pids(),
    })
}

pub fn timestamp_now() -> String { timeutil::local_timestamp() }

// ---------------------------------------------------------------------------
// Meeting probe (bundled `is_meeting` helper)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ProbeApp {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub bundle: String,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct CallWindow {
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub title: String,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct MeetingProbe {
    #[serde(default)]
    pub mic_active: bool,
    /// true when per-process mic attribution is available (macOS 14+)
    #[serde(default)]
    pub mic_precise: bool,
    #[serde(default)]
    pub in_call: bool,
    #[serde(default)]
    pub in_call_app: String,
    #[serde(default)]
    pub frontmost: ProbeApp,
    #[serde(default)]
    pub meeting_apps: Vec<ProbeApp>,
    #[serde(default)]
    pub call_window: Option<CallWindow>,
}

/// Run the `is_meeting` helper once, excluding our own audio children.
pub fn probe_meeting() -> Option<MeetingProbe> {
    let pids: Vec<String> = audio_child_pids().iter().map(|p| p.to_string()).collect();
    let mut args: Vec<&str> = Vec::new();
    for p in &pids {
        args.push("--ignore-pid");
        args.push(p);
    }
    super::helpers::run_json("is_meeting", &args)
}

/// Label for the meeting app that is in a call or frontmost (for tagging manual sessions).
pub fn detect_meeting_app_name() -> Option<String> {
    let p = probe_meeting()?;
    if p.in_call && !p.in_call_app.is_empty() { return Some(p.in_call_app); }
    if let Some(cw) = p.call_window { if !cw.app.is_empty() { return Some(cw.app); } }
    p.meeting_apps
        .iter()
        .find(|a| a.bundle == p.frontmost.bundle && !a.bundle.is_empty())
        .map(|a| a.name.clone())
}

/// Hysteresis state for auto meeting recording.
struct MeetingDetector {
    probe_every: Duration,
    last_probe: Option<Instant>,
    positives: u32,
    last_positive: Option<Instant>,
    active: bool,
    session_app: String,
    session_window: String,
    session_start_micros: i64,
    warned_missing_helper: bool,
    warned_no_mic: bool,
    warned_imprecise: bool,
}

/// Consecutive positive probes required to start a session.
const START_CONFIRM_PROBES: u32 = 2;
/// Session ends after this long without a positive probe.
const END_AFTER: Duration = Duration::from_secs(30);

/// What the detector decided for one probe (side effects are applied by `tick`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Decision {
    Nothing,
    /// Suppression (user pressed stop) can be lifted: the call it belonged to is over.
    ClearSuppression,
    /// Start an auto session for this app label.
    Start(String),
    /// End the current auto session (reason string for the log).
    Stop(&'static str),
}

/// Inputs to one detector step that come from outside the probe.
#[derive(Debug, Clone, Copy)]
struct Environment {
    suppressed: bool,
    manual_running: bool,
    meeting_flag_active: bool,
}

impl MeetingDetector {
    fn new() -> Self {
        Self {
            probe_every: Duration::from_secs(6),
            last_probe: None,
            positives: 0,
            last_positive: None,
            active: false,
            session_app: String::new(),
            session_window: String::new(),
            session_start_micros: 0,
            warned_missing_helper: false,
            warned_no_mic: false,
            warned_imprecise: false,
        }
    }

    /// Pure decision step: feed one probe result at time `now`.
    fn decide(&mut self, probe: &MeetingProbe, now: Instant, env: Environment, fallback_app: &str) -> Decision {
        let in_call = probe.in_call;
        if in_call {
            self.positives += 1;
            self.last_positive = Some(now);
        } else {
            self.positives = 0;
        }
        let call_over = !in_call
            && self.last_positive.map(|t| now.duration_since(t) >= END_AFTER).unwrap_or(true);

        if self.active {
            if env.suppressed { return Decision::Stop("auto_stop_user_suppressed"); }
            if call_over { return Decision::Stop("auto_stop_call_ended"); }
            return Decision::Nothing;
        }
        // Suppression belongs to one call; drop it once that call is clearly over.
        if env.suppressed && call_over && !env.meeting_flag_active {
            return Decision::ClearSuppression;
        }
        if in_call && self.positives >= START_CONFIRM_PROBES && !env.suppressed && !env.manual_running {
            let label = if probe.in_call_app.is_empty() { fallback_app.to_string() } else { probe.in_call_app.clone() };
            return Decision::Start(label);
        }
        Decision::Nothing
    }

    fn tick(&mut self, settings: &AppSettings, app_name: &str, window_name: &str) {
        let now = Instant::now();
        if let Some(last) = self.last_probe {
            if now.duration_since(last) < self.probe_every { return; }
        }
        self.last_probe = Some(now);

        let probe = match probe_meeting() {
            Some(p) => p,
            None => {
                if !self.warned_missing_helper {
                    self.warned_missing_helper = true;
                    log::warn!("MindScope: is_meeting helper unavailable; auto meeting detection disabled");
                }
                MeetingProbe::default()
            }
        };
        if !probe.mic_precise && !self.warned_imprecise && probe.mic_active {
            self.warned_imprecise = true;
            log::warn!("MindScope: per-process microphone attribution needs macOS 14+; using device-level mic state (less precise)");
        }

        let env = Environment {
            suppressed: is_audio_suppressed(),
            manual_running: manual_audio().is_running(),
            meeting_flag_active: MEETING_ACTIVE.load(Ordering::SeqCst),
        };
        match self.decide(&probe, now, env, app_name) {
            Decision::Nothing => {}
            Decision::ClearSuppression => MEETING_AUDIO_SUPPRESSED.store(false, Ordering::SeqCst),
            Decision::Stop(reason) => self.finish(reason),
            Decision::Start(label) => {
                if !settings.capture_audio { return; }
                if !permissions::mic_granted() {
                    if !self.warned_no_mic {
                        self.warned_no_mic = true;
                        log::warn!("MindScope: meeting detected but microphone permission not granted; not recording");
                    }
                    return;
                }
                start_audio_session(&label);
                match auto_audio().start() {
                    Ok(()) => {
                        self.active = true;
                        self.session_app = label.clone();
                        self.session_window = window_name.to_string();
                        self.session_start_micros = timeutil::now_micros();
                        set_meeting_active(&label, self.session_start_micros, &format!("auto_start({})", label));
                        log::info!("MindScope: Meeting detected ({}), auto-started audio", label);
                    }
                    Err(e) => {
                        end_audio_session();
                        log::warn!("MindScope: meeting detected but audio could not start: {:?}", e);
                    }
                }
            }
        }
    }

    /// End the current auto session: stop audio, clear flags, write vault notes.
    fn finish(&mut self, reason: &str) {
        auto_audio().stop();
        self.active = false;
        self.positives = 0;
        let end_micros = timeutil::now_micros();

        let (_, cur_type) = get_current_session();
        let manual_owned = cur_type == "manual" && manual_audio().is_running();
        if !manual_owned {
            end_audio_session();
            clear_meeting_active(reason);
        } else {
            log_meeting_event(&format!("{}_manual_kept", reason), true);
        }

        log::info!("MindScope: Meeting ended ({}), generating vault notes...", reason);
        let app = std::mem::take(&mut self.session_app);
        let win = std::mem::take(&mut self.session_window);
        let start = self.session_start_micros;
        thread::spawn(move || generate_meeting_vault(start, end_micros, &app, &win));
    }
}

// ---------------------------------------------------------------------------
// Frame signature for in-memory dedup
// ---------------------------------------------------------------------------

/// 32×32 perceptual hash + 256-bin luma histogram of a frame.
pub struct FrameSignature {
    hash: [u64; 16],
    hist: [u32; 256],
}

impl FrameSignature {
    pub fn of(img: &DynamicImage) -> Self {
        let small = img.resize_exact(32, 32, image::imageops::FilterType::Triangle).to_luma8();
        let px: Vec<u8> = small.pixels().map(|p| p.0[0]).collect();
        let mean = (px.iter().map(|&p| p as u32).sum::<u32>() / px.len().max(1) as u32) as u8;
        let mut hash = [0u64; 16];
        for (i, &p) in px.iter().enumerate() {
            if p > mean { hash[i / 64] |= 1u64 << (i % 64); }
        }
        let hist_src = img.resize_exact(64, 64, image::imageops::FilterType::Triangle).to_luma8();
        let mut hist = [0u32; 256];
        for p in hist_src.pixels() { hist[p.0[0] as usize] += 1; }
        Self { hash, hist }
    }

    fn hamming(&self, other: &Self) -> u32 {
        self.hash.iter().zip(other.hash.iter()).map(|(a, b)| (a ^ b).count_ones()).sum()
    }

    fn histogram_correlation(&self, other: &Self) -> f64 {
        let n = 256.0;
        let mean1: f64 = self.hist.iter().map(|&v| v as f64).sum::<f64>() / n;
        let mean2: f64 = other.hist.iter().map(|&v| v as f64).sum::<f64>() / n;
        let (mut num, mut den1, mut den2) = (0.0, 0.0, 0.0);
        for i in 0..256 {
            let d1 = self.hist[i] as f64 - mean1;
            let d2 = other.hist[i] as f64 - mean2;
            num += d1 * d2;
            den1 += d1 * d1;
            den2 += d2 * d2;
        }
        let den = (den1 * den2).sqrt();
        if den < 1e-10 { 1.0 } else { num / den }
    }

    /// Near-identical frame: ≤ 8 of 1024 hash bits differ AND histograms correlate > 0.98.
    pub fn similar_to(&self, other: &Self) -> bool {
        self.hamming(other) <= 8 && self.histogram_correlation(other) > 0.98
    }
}

// ---------------------------------------------------------------------------
// Recorder
// ---------------------------------------------------------------------------

pub struct Recorder {
    running: Arc<AtomicBool>,
}

impl Default for Recorder {
    fn default() -> Self { Self::new() }
}

impl Recorder {
    pub fn new() -> Self {
        Self { running: Arc::new(AtomicBool::new(false)) }
    }

    pub fn start(&self) -> bool {
        if self.running.swap(true, Ordering::SeqCst) { return false; }
        let running = self.running.clone();
        thread::Builder::new()
            .name("mindscope-recorder".into())
            .spawn(move || {
                run_capture_loop(&running);
                running.store(false, Ordering::SeqCst);
            })
            .is_ok()
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

fn sleep_interval(running: &AtomicBool, interval: Duration) {
    // Sleep in short steps so stop() is honoured quickly.
    let deadline = Instant::now() + interval;
    while running.load(Ordering::SeqCst) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100).min(deadline.saturating_duration_since(Instant::now())));
    }
}

fn run_capture_loop(running: &AtomicBool) {
    if let Err(e) = db::init_db() {
        log::error!("MindScope: DB init failed: {}", e);
        return;
    }

    let frame_buffer = db::FrameBuffer::new(5, 10);
    let mut detector = MeetingDetector::new();
    let mut app_settings = settings::load_settings();
    let mut settings_loaded = Instant::now();

    let mut encoder_started = false;
    let mut encoder_warned = false;
    let mut permission_requested = false;
    let mut permission_logged = false;

    let mut last_app = String::new();
    let mut last_ocr_text = String::new();
    let mut last_signature: Option<FrameSignature> = None;
    let mut frames_kept: u64 = 0;
    let mut skip_count: u64 = 0;

    log::info!("MindScope recorder started (interval={}s, height={}px)",
        app_settings.capture_interval_secs, app_settings.capture_height);

    // Runs EVERY iteration (also when the frame is deduplicated away): write
    // buffered rows once they are old enough and re-point encoded frames at
    // video://. Doing this only on the kept-frame path left rows unflushed and
    // pointing at JPEGs the encoder had already deleted whenever the screen
    // stayed static.
    let housekeeping = |frame_buffer: &db::FrameBuffer| {
        if let Err(e) = frame_buffer.maybe_flush() {
            log::error!("MindScope: frame buffer flush failed: {}", e);
        }
        for enc in video::drain_encoder_output() {
            let video_ref = format!("video://{}#{}", enc.segment, enc.frame);
            if enc.ts > 0 {
                if !frame_buffer.update_path_by_ts(enc.ts, &video_ref) {
                    let _ = db::update_frame_path_by_timestamp(enc.ts, &video_ref);
                }
            } else {
                let _ = db::update_latest_frame_path(&video_ref);
            }
        }
    };

    while running.load(Ordering::SeqCst) {
        if settings_loaded.elapsed() >= Duration::from_secs(10) {
            app_settings = settings::load_settings();
            settings_loaded = Instant::now();
        }
        let interval = Duration::from_secs(app_settings.capture_interval_secs.clamp(1, 120));
        housekeeping(&frame_buffer);

        // Real permission state, no heuristics.
        if !permissions::screen_capture_granted() {
            if !permission_requested {
                permission_requested = true;
                permissions::request_screen_capture();
            }
            if !permission_logged {
                permission_logged = true;
                log::warn!("MindScope: Screen Recording permission not granted; waiting (System Settings → Privacy & Security → Screen Recording)");
            }
            sleep_interval(running, Duration::from_secs(5));
            continue;
        }

        if !encoder_started {
            if video::encoder_available() {
                match video::start_encoder() {
                    Ok(_) => encoder_started = true,
                    Err(e) => if !encoder_warned { encoder_warned = true; log::warn!("MindScope: encoder start failed: {}", e); },
                }
            } else if !encoder_warned {
                encoder_warned = true;
                log::warn!("MindScope: hevc_encoder helper missing; keeping JPEG frames on disk");
            }
        }

        // What is the user looking at?
        let win = screenshot::topmost_window();
        let (app_name, window_name) = match &win {
            Some(w) => (w.app.clone(), w.title.clone()),
            None => {
                let (a, t, _) = screenshot::get_active_window_info();
                (a, t)
            }
        };
        let display_id = win.as_ref().map(|w| w.display).filter(|d| *d != 0);

        detector.tick(&app_settings, &app_name, &window_name);

        // Exclusions (settings honoured every cycle)
        if app_settings.excluded_apps.iter().any(|a| a.eq_ignore_ascii_case(&app_name)) {
            sleep_interval(running, interval);
            continue;
        }
        if app_settings.private_browsing && screenshot::is_private_window(&window_name) {
            sleep_interval(running, interval);
            continue;
        }

        // Capture the active display only, downscaled.
        let Some(img) = screenshot::capture_display(display_id, app_settings.capture_height) else {
            sleep_interval(running, interval);
            continue;
        };

        // Dedup against the previous kept frame (in memory).
        let signature = FrameSignature::of(&img);
        let same_context = app_name == last_app && frames_kept > 0;
        if same_context {
            if let Some(prev) = &last_signature {
                if signature.similar_to(prev) {
                    skip_count += 1;
                    if skip_count % 50 == 0 {
                        log::info!("MindScope: skipped {} duplicate frames", skip_count);
                    }
                    drop(img);
                    sleep_interval(running, interval);
                    continue;
                }
            }
        }

        let ts_micros = timeutil::now_micros();
        let date = timeutil::date_string(ts_micros / 1_000_000);
        let quality = (app_settings.jpeg_quality.clamp(0.1, 1.0) * 100.0).round() as u8;
        let Some(jpeg) = screenshot::encode_jpeg(&img, quality) else {
            sleep_interval(running, interval);
            continue;
        };
        drop(img);
        let image_path = db::frames_dir(&date).join(format!("frame_{}.jpg", ts_micros / 1000));
        if let Err(e) = std::fs::write(&image_path, &jpeg) {
            log::error!("MindScope: failed to write frame: {}", e);
            sleep_interval(running, interval);
            continue;
        }
        drop(jpeg);
        last_signature = Some(signature);

        // OCR with bounding boxes every 3rd kept frame or on app change; PII-scrubbed.
        let ocr_result = if frames_kept % 3 == 0 || !same_context {
            let mut result = ocr::extract_with_regions(&image_path);
            result.text = pii::sanitize(&result.text);
            for r in &mut result.regions { r.text = pii::sanitize(&r.text); }
            if !result.text.is_empty() { last_ocr_text = result.text.clone(); }
            result
        } else {
            ocr::OcrResult { text: last_ocr_text.clone(), regions: vec![] }
        };
        let window_clean = pii::sanitize(&window_name);
        last_app = app_name.clone();

        frame_buffer.buffer_frame(ts_micros, &app_name, &window_clean, &ocr_result, &image_path.to_string_lossy());

        if encoder_started {
            video::encode_frame(&image_path, ts_micros);
        }
        housekeeping(&frame_buffer);

        frames_kept += 1;
        if frames_kept % 50 == 0 {
            log::info!("MindScope: kept {} frames (skipped {} duplicates)", frames_kept, skip_count);
        }

        sleep_interval(running, interval);
    }

    if detector.active {
        detector.finish("recorder_stopped");
    }
    video::stop_encoder();
    // Encoder has finalised: apply its last confirmations, then write everything.
    housekeeping(&frame_buffer);
    if let Err(e) = frame_buffer.flush_frames() {
        log::error!("MindScope: final frame buffer flush failed: {}", e);
    }
    log::info!("MindScope recorder stopped");
}

// ---------------------------------------------------------------------------
// Meeting notes
// ---------------------------------------------------------------------------

/// Generate meeting notes and write them to the vault after a meeting ends.
fn generate_meeting_vault(start_ts: i64, end_ts: i64, app_name: &str, window_name: &str) {
    let frames = db::get_frames_in_range(start_ts, end_ts, 0).unwrap_or_default();

    let mut screen_context = String::new();
    let mut last_text = String::new();
    for f in &frames {
        let t = f.ocr_text.as_str();
        if text::char_len(t) > 20 && t != last_text {
            screen_context.push_str(text::truncate_chars(t, 200));
            screen_context.push('\n');
            last_text = t.to_string();
        }
        if screen_context.len() > 4000 { break; }
    }

    let date = timeutil::date_string(end_ts / 1_000_000);
    let audio_segments = audio::load_audio_segments(&date);
    let mut transcript = String::new();
    for seg in &audio_segments {
        if seg.transcript.is_empty() { continue; }
        let Some(seg_ts) = timeutil::parse_local_datetime_micros(&seg.timestamp) else { continue; };
        if seg_ts < start_ts - 5_000_000 || seg_ts > end_ts + 5_000_000 { continue; }
        transcript.push_str(&seg.transcript);
        transcript.push(' ');
    }

    if frames.is_empty() && transcript.trim().is_empty() {
        log::info!("MindScope: meeting had no frames or transcript; skipping vault notes");
        return;
    }

    let prompt = format!(
        "Generate concise meeting notes in markdown. No meta-commentary.\n\
         App: {}\nWindow: {}\n\
         Screen text during meeting:\n{}\n\
         Audio transcript:\n{}\n\n\
         Format:\n## Summary\n(2-3 sentences)\n\n## Key Points\n- point1\n- point2\n\n## Action Items\n- [ ] item1\n- [ ] item2\n\n\
         If data is sparse, write what you can. Keep it short.",
        app_name, window_name,
        text::truncate_chars(&screen_context, 2000),
        text::truncate_chars(&transcript, 2000)
    );

    let vault_dir = paths::vault_dir();
    let _ = std::fs::create_dir_all(&vault_dir);

    let notes = match super::claude_cli::run_background("meeting-notes", &prompt) {
        Ok(response) => response,
        Err(e) => {
            log::warn!("MindScope: meeting notes via Claude unavailable ({}); writing raw notes", e);
            format!("## Summary\nMeeting on {} ({})\n\n## Transcript\n{}\n\n## Screen Notes\n{}",
                app_name, window_name,
                text::truncate_chars(&transcript, 1000),
                text::truncate_chars(&screen_context, 500))
        }
    };

    let date_dots = date.replace('-', ".");
    let filepath = vault_dir.join(format!("meet.{}.md", date_dots));
    let filepath = if filepath.exists() {
        let ts = timeutil::now_secs();
        vault_dir.join(format!("meet.{}.{}.md", date_dots, ts % 10000))
    } else {
        filepath
    };

    let content = format!(
        "---\ntitle: {}\ndate: {}\napp: {}\nupdated: {}\n---\n\n{}",
        text::truncate_chars(window_name, 60),
        date,
        app_name,
        timeutil::now_secs(),
        notes
    );

    match std::fs::write(&filepath, &content) {
        Ok(_) => log::info!("MindScope: Vault meeting notes saved to {:?}", filepath),
        Err(e) => log::error!("MindScope: Failed to write vault notes: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(in_call: bool) -> MeetingProbe {
        MeetingProbe { in_call, in_call_app: if in_call { "Zoom".into() } else { String::new() }, mic_precise: true, mic_active: in_call, ..Default::default() }
    }
    const ENV: Environment = Environment { suppressed: false, manual_running: false, meeting_flag_active: false };

    #[test]
    fn needs_two_positive_probes_to_start_and_30s_silence_to_stop() {
        let mut d = MeetingDetector::new();
        let t0 = Instant::now();
        assert_eq!(d.decide(&probe(true), t0, ENV, "Safari"), Decision::Nothing);
        assert_eq!(d.decide(&probe(true), t0 + Duration::from_secs(6), ENV, "Safari"), Decision::Start("Zoom".into()));
        d.active = true;
        // A single negative probe does not end the session...
        assert_eq!(d.decide(&probe(false), t0 + Duration::from_secs(12), ENV, "Safari"), Decision::Nothing);
        // ...nor does one at 29 s after the last positive...
        assert_eq!(d.decide(&probe(false), t0 + Duration::from_secs(6 + 29), ENV, "Safari"), Decision::Nothing);
        // ...but 30 s without evidence ends it (previously unreachable).
        assert_eq!(d.decide(&probe(false), t0 + Duration::from_secs(6 + 30), ENV, "Safari"), Decision::Stop("auto_stop_call_ended"));
    }

    #[test]
    fn idle_meeting_app_without_mic_never_starts() {
        let mut d = MeetingDetector::new();
        let t0 = Instant::now();
        // Helper reports meeting app open but not in a call (no mic use) → in_call=false
        for i in 0..20 {
            assert_eq!(d.decide(&probe(false), t0 + Duration::from_secs(6 * i), ENV, "Zoom"), Decision::Nothing);
        }
    }

    #[test]
    fn user_stop_ends_session_and_blocks_restart_until_call_over() {
        let mut d = MeetingDetector::new();
        let t0 = Instant::now();
        d.decide(&probe(true), t0, ENV, "x");
        assert!(matches!(d.decide(&probe(true), t0 + Duration::from_secs(6), ENV, "x"), Decision::Start(_)));
        d.active = true;
        let suppressed = Environment { suppressed: true, ..ENV };
        assert_eq!(d.decide(&probe(true), t0 + Duration::from_secs(12), suppressed, "x"), Decision::Stop("auto_stop_user_suppressed"));
        d.active = false;
        // Still in the same call: suppression stays, no restart.
        assert_eq!(d.decide(&probe(true), t0 + Duration::from_secs(18), suppressed, "x"), Decision::Nothing);
        assert_eq!(d.decide(&probe(true), t0 + Duration::from_secs(24), suppressed, "x"), Decision::Nothing);
        // Call ends (30 s without positives) → suppression is lifted for the next call.
        assert_eq!(d.decide(&probe(false), t0 + Duration::from_secs(30), suppressed, "x"), Decision::Nothing);
        assert_eq!(d.decide(&probe(false), t0 + Duration::from_secs(24 + 30), suppressed, "x"), Decision::ClearSuppression);
    }

    #[test]
    fn manual_recording_blocks_auto_start() {
        let mut d = MeetingDetector::new();
        let t0 = Instant::now();
        let env = Environment { manual_running: true, meeting_flag_active: true, ..ENV };
        d.decide(&probe(true), t0, env, "x");
        assert_eq!(d.decide(&probe(true), t0 + Duration::from_secs(6), env, "x"), Decision::Nothing);
    }

    #[test]
    fn frame_signature_dedups_identical_and_keeps_different_frames() {
        let mut a = image::RgbImage::new(320, 200);
        for (x, y, p) in a.enumerate_pixels_mut() {
            let v = ((x / 20 + y / 20) % 2 * 200) as u8; // checkerboard
            *p = image::Rgb([v, v, 255 - v]);
        }
        let img_a = DynamicImage::ImageRgb8(a.clone());
        let sig_a = FrameSignature::of(&img_a);
        // JPEG-like noise: same content, tiny per-pixel perturbation
        let mut b = a.clone();
        for (i, p) in b.pixels_mut().enumerate() { if i % 7 == 0 { p.0[0] = p.0[0].saturating_add(2); } }
        assert!(sig_a.similar_to(&FrameSignature::of(&DynamicImage::ImageRgb8(b))));
        // Different content: invert the bottom half (new window / scrolled page)
        let mut c = a.clone();
        for (_, y, p) in c.enumerate_pixels_mut() { if y > 100 { *p = image::Rgb([255 - p.0[0], 255 - p.0[1], 255 - p.0[2]]); } }
        assert!(!sig_a.similar_to(&FrameSignature::of(&DynamicImage::ImageRgb8(c))));
    }
}
