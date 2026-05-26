use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::fs;

use super::db;
use super::speaker::SharedSpeakerManager;
use super::platform;

// Track microphone permission state
static MIC_PERMISSION_GRANTED: AtomicBool = AtomicBool::new(false);
static MIC_PERMISSION_CHECKED: AtomicBool = AtomicBool::new(false);

/// Check microphone permission silently — NEVER triggers dialog
pub fn check_mic_permission() -> bool {
    if MIC_PERMISSION_GRANTED.load(Ordering::Relaxed) {
        return true;
    }

    // Always return true — let ffmpeg handle the actual permission check.
    // On Windows, no explicit permission is needed.
    // On macOS, ffmpeg via AVFoundation will show the permission dialog if needed.
    MIC_PERMISSION_GRANTED.store(true, Ordering::Relaxed);
    MIC_PERMISSION_CHECKED.store(true, Ordering::Relaxed);
    true
}

/// Request microphone permission — triggers dialog ONCE
/// Only call this from an explicit user action (button click)
pub fn request_mic_permission() -> bool {
    let tmp = std::env::temp_dir().join("mindscope_mic_test.m4a");
    let tmp_str = tmp.to_str().unwrap_or_default();

    let ffmpeg = match platform::find_ffmpeg() {
        Some(f) => f,
        None => {
            MIC_PERMISSION_CHECKED.store(true, Ordering::Relaxed);
            return false;
        }
    };

    let audio_format = platform::get_audio_input_format();
    let audio_device = get_audio_device_arg();

    let status = Command::new(&ffmpeg)
        .args(["-f", audio_format, "-i", &audio_device, "-t", "0.5", "-y", tmp_str])
        .stderr(std::process::Stdio::null())
        .status();

    let granted = status.map(|s| s.success()).unwrap_or(false);
    let _ = fs::remove_file(&tmp);

    MIC_PERMISSION_GRANTED.store(granted, Ordering::Relaxed);
    MIC_PERMISSION_CHECKED.store(true, Ordering::Relaxed);
    granted
}

/// Get the audio device argument for ffmpeg based on platform
#[cfg(target_os = "macos")]
fn get_audio_device_arg() -> String {
    ":default".to_string()
}

#[cfg(target_os = "windows")]
fn get_audio_device_arg() -> String {
    format!("audio={}", platform::get_default_audio_device())
}

/// Audio recorder — only starts on explicit user action
pub struct AudioRecorder {
    running: Arc<AtomicBool>,
    chunk_duration_secs: u64,
    speaker_manager: Arc<SharedSpeakerManager>,
}

impl AudioRecorder {
    pub fn new(chunk_duration_secs: u64) -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            chunk_duration_secs,
            speaker_manager: Arc::new(SharedSpeakerManager::new()),
        }
    }

    /// Start recording — Retrace-style continuous streaming pipeline:
    /// ffmpeg outputs raw 16kHz mono PCM to stdout → Rust reads in 250ms batches
    /// → Whisper transcribes directly from in-memory samples (no file I/O)
    pub fn start(&self) -> bool {
        if self.running.load(Ordering::Relaxed) {
            return false;
        }

        self.running.store(true, Ordering::Relaxed);
        let running = self.running.clone();
        let _speaker_mgr = self.speaker_manager.clone();

        thread::spawn(move || {
            log::info!("MindScope streaming audio recorder started");

            // Find ffmpeg using platform-specific lookup
            let ffmpeg = match platform::find_ffmpeg() {
                Some(f) => f,
                None => {
                    log::error!("MindScope: ffmpeg not found");
                    return;
                }
            };

            let audio_format = platform::get_audio_input_format();
            let audio_device = get_audio_device_arg();

            // Batch settings: 2s chunks for Whisper context, fed every 2s
            // (Whisper base needs >=1s for decent accuracy; 2s is the sweet spot)
            const SAMPLE_RATE: u32 = 16_000;
            const BATCH_SECONDS: f32 = 2.0;
            const BATCH_SAMPLES: usize = (SAMPLE_RATE as f32 * BATCH_SECONDS) as usize;
            // Overlap 0.3s between batches so words split at boundaries are caught
            const OVERLAP_SAMPLES: usize = (SAMPLE_RATE as f32 * 0.3) as usize;

            loop {
                if !running.load(Ordering::Relaxed) { break; }

                // Spawn ffmpeg with raw PCM output to stdout
                let mut child = match std::process::Command::new(&ffmpeg)
                    .args([
                        "-f", audio_format,
                        "-i", &audio_device,
                        "-ac", "1",
                        "-ar", "16000",
                        "-f", "s16le",       // raw signed 16-bit PCM
                        "-",                  // stdout
                    ])
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    Ok(c) => c,
                    Err(e) => {
                        log::error!("MindScope: ffmpeg spawn failed: {}", e);
                        thread::sleep(Duration::from_secs(3));
                        continue;
                    }
                };

                let mut stdout = match child.stdout.take() {
                    Some(s) => s,
                    None => { let _ = child.kill(); continue; }
                };

                use std::io::Read;
                let mut pcm_buf: Vec<i16> = Vec::with_capacity(BATCH_SAMPLES * 2);
                let mut byte_buf = vec![0u8; 4096];
                let mut last_batch_start = std::time::Instant::now();
                let mut last_transcript = String::new();

                while running.load(Ordering::Relaxed) {
                    // Read raw PCM bytes from ffmpeg stdout
                    let n = match stdout.read(&mut byte_buf) {
                        Ok(0) => break, // EOF
                        Ok(n) => n,
                        Err(_) => break,
                    };

                    // Convert bytes to i16 samples (little-endian)
                    for chunk in byte_buf[..n].chunks_exact(2) {
                        let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
                        pcm_buf.push(sample);
                    }

                    // When we have enough samples for a batch, transcribe
                    if pcm_buf.len() >= BATCH_SAMPLES {
                        let batch: Vec<f32> = pcm_buf[..BATCH_SAMPLES]
                            .iter()
                            .map(|&s| s as f32 / 32768.0)
                            .collect();

                        // Keep overlap for next batch; drop the rest
                        let drain_end = BATCH_SAMPLES.saturating_sub(OVERLAP_SAMPLES);
                        pcm_buf.drain(..drain_end);

                        let batch_ts = super::recorder::timestamp_now();
                        let date = batch_ts[..10].to_string();

                        // Transcribe directly from memory (no file I/O)
                        if let Ok(transcript) = super::whisper::transcribe_samples(&batch) {
                            if !transcript.is_empty() {
                                // Cross-segment dedup: skip if identical to previous or contained in it
                                let norm = transcript.trim().to_lowercase();
                                let prev_norm = last_transcript.trim().to_lowercase();
                                let is_dup = norm == prev_norm
                                    || (norm.len() > 10 && prev_norm.contains(&norm))
                                    || (prev_norm.len() > 10 && norm.contains(&prev_norm) && norm.len() < prev_norm.len() + 10);

                                if !is_dup {
                                    let (session_id, session_type) = super::recorder::get_current_session();
                                    let segment = AudioSegment {
                                        timestamp: batch_ts,
                                        audio_path: String::new(),
                                        transcript: transcript.clone(),
                                        duration_secs: BATCH_SECONDS as u32,
                                        session_id,
                                        session_type,
                                    };
                                    save_audio_segment(&date, segment);
                                    last_transcript = transcript;
                                }
                            }
                        }

                        last_batch_start = std::time::Instant::now();
                    }

                    // Check suppression every iteration for fast stop
                    if super::recorder::is_audio_suppressed() {
                        break;
                    }

                    // Safety: if reading is stalled, restart ffmpeg
                    if last_batch_start.elapsed() > Duration::from_secs(15) {
                        break;
                    }
                }

                let _ = child.kill();
                let _ = child.wait();

                if !running.load(Ordering::Relaxed) { break; }
                thread::sleep(Duration::from_millis(200));
            }
            log::info!("MindScope streaming audio recorder stopped");
        });

        true
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
        platform::kill_audio_processes();
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Clear speaker identification state (use between meetings).
    pub fn clear_speakers(&self) {
        self.speaker_manager.clear();
    }
}

fn record_chunk(output_path: &Path, duration_secs: u64) -> bool {
    let path_str = output_path.to_str().unwrap_or("");

    let ffmpeg = match platform::find_ffmpeg() {
        Some(f) => f,
        None => return false,
    };

    let audio_format = platform::get_audio_input_format();
    let audio_device = get_audio_device_arg();

    let status = Command::new(&ffmpeg)
        .args([
            "-f", audio_format,
            "-i", &audio_device,
            "-t", &duration_secs.to_string(),
            "-ac", "1",                // mono
            "-ar", "16000",            // 16kHz for Whisper
            "-c:a", "aac",
            "-y",                      // overwrite
            path_str,
        ])
        .stderr(std::process::Stdio::null())
        .status();

    status.map(|s| s.success()).unwrap_or(false)
}

/// Transcribe audio — uses Whisper if model available, falls back to platform speech API
pub fn transcribe_audio(audio_path: &Path) -> String {
    let settings = super::settings::load_settings();
    let use_whisper = settings.transcription_engine.as_deref() != Some("system");

    // Try Whisper first (best accuracy, cross-platform)
    if use_whisper && super::whisper::is_model_available() {
        match super::whisper::transcribe(audio_path) {
            Ok(text) if !text.is_empty() => return text,
            Ok(_) => {} // empty = silence, fall through
            Err(e) => log::warn!("MindScope: Whisper failed: {}", e),
        }
    }

    // Fallback: platform-specific speech recognition
    transcribe_with_platform_api(audio_path)
}

/// Transcribe using platform-specific speech recognition API
#[cfg(target_os = "macos")]
fn transcribe_with_platform_api(audio_path: &Path) -> String {
    let helper = ensure_transcribe_helper();
    if !helper.exists() { return String::new(); }

    let output = Command::new(helper.to_str().unwrap_or(""))
        .arg(audio_path.to_str().unwrap_or(""))
        .output()
        .ok();

    match output {
        Some(out) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        _ => String::new(),
    }
}

#[cfg(target_os = "windows")]
fn transcribe_with_platform_api(_audio_path: &Path) -> String {
    // Windows speech recognition via SAPI is complex to implement
    // For now, rely on Whisper for transcription on Windows
    // Users should download the Whisper model for best results
    String::new()
}

/// Compile transcription helper once (macOS only)
#[cfg(target_os = "macos")]
fn ensure_transcribe_helper() -> PathBuf {
    let dir = dirs_next::home_dir().unwrap_or_default().join(".mindscope");
    let helper = dir.join("transcribe_helper");

    if helper.exists() { return helper; }

    let source = dir.join("transcribe_helper.swift");
    let swift_code = r#"
import Speech
import Foundation

guard CommandLine.arguments.count > 1 else { exit(1) }
let audioPath = CommandLine.arguments[1]
let semaphore = DispatchSemaphore(value: 0)
var resultText = ""

SFSpeechRecognizer.requestAuthorization { status in
    guard status == .authorized else { semaphore.signal(); return }
    let recognizer = SFSpeechRecognizer(locale: Locale(identifier: "en-US"))
    let url = URL(fileURLWithPath: audioPath)
    let request = SFSpeechURLRecognitionRequest(url: url)
    request.shouldReportPartialResults = false
    recognizer?.recognitionTask(with: request) { result, error in
        if let result = result, result.isFinal {
            resultText = result.bestTranscription.formattedString
        }
        if error != nil || (result?.isFinal ?? false) { semaphore.signal() }
    }
}
_ = semaphore.wait(timeout: .now() + 60)
print(resultText)
"#;

    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(&source, swift_code);

    let output = Command::new("swiftc")
        .args([source.to_str().unwrap(), "-o", helper.to_str().unwrap(), "-O", "-framework", "Speech"])
        .output();

    if let Ok(o) = output {
        if o.status.success() {
            let _ = fs::remove_file(&source);
            log::info!("MindScope: Transcription helper compiled");
        }
    }

    helper
}

/// Load an audio file as 16kHz mono f32 samples for speaker identification.
/// Returns None if decoding fails.
fn load_audio_samples_f32(path: &Path) -> Option<Vec<f32>> {
    // Decode to WAV using platform-specific converter, then read with hound
    let tmp = std::env::temp_dir().join("mindscope_speaker_tmp.wav");

    if platform::convert_audio_to_wav(path, &tmp).is_err() {
        return None;
    }

    let reader = hound::WavReader::open(&tmp).ok()?;
    let spec = reader.spec();
    let samples: Vec<f32> = if spec.sample_format == hound::SampleFormat::Float {
        reader.into_samples::<f32>().filter_map(|s| s.ok()).collect()
    } else {
        reader
            .into_samples::<i16>()
            .filter_map(|s| s.ok())
            .map(|s| s as f32 / 32768.0)
            .collect()
    };

    let _ = fs::remove_file(&tmp);
    Some(samples)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AudioSegment {
    pub timestamp: String,
    pub audio_path: String,
    pub transcript: String,
    pub duration_secs: u32,
    /// Session ID: unique per activity/meeting (e.g. timestamp of session start)
    /// Empty string for legacy segments recorded before session tracking.
    #[serde(default)]
    pub session_id: String,
    /// Session type: "zoom", "tencent", "teams", "manual", etc.
    #[serde(default)]
    pub session_type: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct AudioIndex {
    segments: Vec<AudioSegment>,
}

fn audio_dir(date: &str) -> PathBuf {
    let dir = db::data_dir().join("audio").join(date);
    fs::create_dir_all(&dir).ok();
    dir
}

fn audio_index_path(date: &str) -> PathBuf {
    db::data_dir().join("audio").join(format!("{}.json", date))
}

fn save_audio_segment(date: &str, segment: AudioSegment) {
    let path = audio_index_path(date);
    let mut index: AudioIndex = fs::read_to_string(&path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default();
    index.segments.push(segment);
    if let Ok(content) = serde_json::to_string_pretty(&index) {
        let _ = fs::write(&path, content);
    }
}

// --- System audio recording ---

/// Record system audio (not microphone) for the given duration.
/// Uses platform-specific implementation.
/// macOS: ScreenCaptureKit via Swift helper
/// Windows: WASAPI loopback via ffmpeg
pub fn record_system_audio(output_path: &Path, duration_secs: u64) -> bool {
    #[cfg(target_os = "macos")]
    {
        record_system_audio_macos(output_path, duration_secs)
    }
    #[cfg(target_os = "windows")]
    {
        record_system_audio_windows(output_path, duration_secs)
    }
}

#[cfg(target_os = "macos")]
fn record_system_audio_macos(output_path: &Path, duration_secs: u64) -> bool {
    let helper = ensure_system_audio_helper();
    if !helper.exists() {
        log::warn!("MindScope: system_audio helper not available");
        return false;
    }

    let path_str = output_path.to_str().unwrap_or("");
    let status = Command::new(helper.to_str().unwrap_or(""))
        .args([path_str, &duration_secs.to_string()])
        .status();

    match status {
        Ok(s) if s.success() => {
            output_path.exists() && fs::metadata(output_path).map(|m| m.len() > 0).unwrap_or(false)
        }
        Ok(s) => {
            log::warn!("MindScope: system_audio helper exited with: {}", s);
            false
        }
        Err(e) => {
            log::warn!("MindScope: failed to run system_audio helper: {}", e);
            false
        }
    }
}

#[cfg(target_os = "macos")]
fn ensure_system_audio_helper() -> PathBuf {
    let bin_dir = dirs_next::home_dir().unwrap_or_default()
        .join(".mindscope").join("bin");
    let helper = bin_dir.join("system_audio");

    if helper.exists() { return helper; }

    let source_candidates = [
        std::env::current_exe().unwrap_or_default()
            .parent().unwrap_or(Path::new("."))
            .join("swift").join("system_audio.swift"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("swift").join("system_audio.swift"),
    ];

    let source = source_candidates.iter().find(|p| p.exists());
    let Some(source) = source else {
        log::warn!("MindScope: system_audio.swift not found");
        return helper;
    };

    let _ = fs::create_dir_all(&bin_dir);

    let output = Command::new("swiftc")
        .args([
            source.to_str().unwrap(),
            "-o", helper.to_str().unwrap(),
            "-O",
            "-framework", "ScreenCaptureKit",
            "-framework", "AVFoundation",
            "-framework", "CoreMedia",
        ])
        .output();

    if let Ok(o) = output {
        if o.status.success() {
            log::info!("MindScope: system_audio helper compiled");
        }
    }

    helper
}

#[cfg(target_os = "windows")]
fn record_system_audio_windows(output_path: &Path, duration_secs: u64) -> bool {
    // On Windows, use ffmpeg with dshow virtual audio device or WASAPI
    // This requires a virtual audio device like "Stereo Mix" to be enabled
    let ffmpeg = match platform::find_ffmpeg() {
        Some(f) => f,
        None => {
            log::warn!("MindScope: ffmpeg not found for system audio");
            return false;
        }
    };

    let path_str = output_path.to_str().unwrap_or("");

    // Try to find a loopback device (Stereo Mix, What U Hear, etc.)
    let loopback_devices = ["Stereo Mix", "What U Hear", "Wave Out Mix", "Loopback"];

    for device in &loopback_devices {
        let status = Command::new(&ffmpeg)
            .args([
                "-f", "dshow",
                "-i", &format!("audio={}", device),
                "-t", &duration_secs.to_string(),
                "-ac", "1",
                "-ar", "16000",
                "-y",
                path_str,
            ])
            .stderr(std::process::Stdio::null())
            .status();

        if let Ok(s) = status {
            if s.success() && output_path.exists() {
                return fs::metadata(output_path).map(|m| m.len() > 0).unwrap_or(false);
            }
        }
    }

    log::warn!("MindScope: No system audio loopback device found. Enable 'Stereo Mix' in Windows Sound settings.");
    false
}

pub fn load_audio_segments(date: &str) -> Vec<AudioSegment> {
    let path = audio_index_path(date);
    fs::read_to_string(&path)
        .ok()
        .and_then(|c| serde_json::from_str::<AudioIndex>(&c).ok())
        .map(|i| i.segments)
        .unwrap_or_default()
}
