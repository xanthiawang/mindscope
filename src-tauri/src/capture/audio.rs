//! Microphone capture → Whisper streaming pipeline.
//!
//! ffmpeg (found via settings/PATH, never hard-coded) streams 16 kHz mono PCM to
//! stdout; we transcribe 2 s batches in memory and append each transcript as one
//! JSON line to `data/audio/<date>.jsonl`. The ffmpeg child is tracked so it is
//! killed directly (no `pkill -f ffmpeg`), and the Whisper model is unloaded
//! when recording stops.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use std::fs;

use super::paths;
use super::permissions;
use super::tools;
use super::util::LockExt;

/// Microphone permission (no dialog). Real `AVCaptureDevice` status.
pub fn check_mic_permission() -> bool {
    permissions::mic_granted()
}

/// Request microphone permission — triggers the system dialog once.
/// Only call this from an explicit user action (button click).
pub fn request_mic_permission() -> bool {
    permissions::request_mic(Duration::from_secs(120))
}

/// Why `AudioRecorder::start` could not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    AlreadyRunning,
    NoPermission,
    FfmpegMissing,
}

/// Streaming audio recorder (mic → ffmpeg → Whisper).
pub struct AudioRecorder {
    running: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Default for AudioRecorder {
    fn default() -> Self { Self::new() }
}

impl AudioRecorder {
    pub fn new() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            child: Arc::new(Mutex::new(None)),
        }
    }

    /// Start recording. Returns `Ok(())` when the streaming thread is up.
    pub fn start(&self) -> Result<(), StartError> {
        if self.running.load(Ordering::SeqCst) {
            return Err(StartError::AlreadyRunning);
        }
        if !check_mic_permission() {
            return Err(StartError::NoPermission);
        }
        let Some(ffmpeg) = tools::find_ffmpeg() else {
            log::error!("MindScope: ffmpeg not found (settings.ffmpeg_path, PATH, ~/.local/bin, /opt/homebrew/bin). Audio capture disabled.");
            return Err(StartError::FfmpegMissing);
        };

        self.running.store(true, Ordering::SeqCst);
        let running = self.running.clone();
        let child_slot = self.child.clone();

        thread::spawn(move || {
            log::info!("MindScope streaming audio recorder started ({})", ffmpeg.display());
            run_stream_loop(&ffmpeg, &running, &child_slot);
            running.store(false, Ordering::SeqCst);
            super::whisper::unload();
            log::info!("MindScope streaming audio recorder stopped");
        });

        Ok(())
    }

    /// Stop recording: flag the loop, kill the tracked ffmpeg child.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(mut child) = self.child.lock_or_recover().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// PID of the live ffmpeg child, if any (used to exclude it from mic-in-use checks).
    pub fn child_pid(&self) -> Option<u32> {
        self.child.lock_or_recover().as_ref().map(|c| c.id())
    }
}

fn run_stream_loop(ffmpeg: &PathBuf, running: &AtomicBool, child_slot: &Mutex<Option<Child>>) {
    // 2 s batches for Whisper context, 0.3 s overlap so words split at boundaries are caught.
    const SAMPLE_RATE: u32 = 16_000;
    const BATCH_SECONDS: f32 = 2.0;
    const BATCH_SAMPLES: usize = (SAMPLE_RATE as f32 * BATCH_SECONDS) as usize;
    const OVERLAP_SAMPLES: usize = (SAMPLE_RATE as f32 * 0.3) as usize;

    let mut consecutive_failures = 0u32;

    while running.load(Ordering::SeqCst) {
        let mut child = match Command::new(ffmpeg)
            .args([
                "-hide_banner", "-loglevel", "error",
                "-f", "avfoundation",
                "-i", ":default",
                "-ac", "1",
                "-ar", "16000",
                "-f", "s16le",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                // Do NOT retry every 3 s forever: a missing/broken ffmpeg is permanent.
                log::error!("MindScope: ffmpeg spawn failed ({}), stopping audio capture", e);
                return;
            }
        };

        let Some(mut stdout) = child.stdout.take() else {
            let _ = child.kill();
            return;
        };
        *child_slot.lock_or_recover() = Some(child);

        use std::io::Read;
        let mut pcm_buf: Vec<i16> = Vec::with_capacity(BATCH_SAMPLES * 2);
        let mut byte_buf = vec![0u8; 4096];
        let mut last_batch_start = std::time::Instant::now();
        let mut last_transcript = String::new();
        let mut got_data = false;

        while running.load(Ordering::SeqCst) {
            let n = match stdout.read(&mut byte_buf) {
                Ok(0) => break, // EOF (device gone, permission denied, killed)
                Ok(n) => n,
                Err(_) => break,
            };
            got_data = true;

            for chunk in byte_buf[..n].chunks_exact(2) {
                pcm_buf.push(i16::from_le_bytes([chunk[0], chunk[1]]));
            }

            if pcm_buf.len() >= BATCH_SAMPLES {
                let batch: Vec<f32> = pcm_buf[..BATCH_SAMPLES].iter().map(|&s| s as f32 / 32768.0).collect();
                let drain_end = BATCH_SAMPLES.saturating_sub(OVERLAP_SAMPLES);
                pcm_buf.drain(..drain_end);

                let batch_ts = super::timeutil::local_timestamp();
                let date = super::timeutil::today();

                match super::whisper::transcribe_samples(&batch) {
                    Ok(transcript) if !transcript.is_empty() => {
                        let transcript = super::pii::sanitize(&transcript);
                        let norm = transcript.trim().to_lowercase();
                        let prev_norm = last_transcript.trim().to_lowercase();
                        let is_dup = norm == prev_norm
                            || (norm.chars().count() > 10 && prev_norm.contains(&norm))
                            || (prev_norm.chars().count() > 10 && norm.contains(&prev_norm)
                                && norm.chars().count() < prev_norm.chars().count() + 10);

                        if !is_dup {
                            let (session_id, session_type) = super::recorder::get_current_session();
                            save_audio_segment(&date, AudioSegment {
                                timestamp: batch_ts,
                                audio_path: String::new(),
                                transcript: transcript.clone(),
                                duration_secs: BATCH_SECONDS as u32,
                                session_id,
                                session_type,
                            });
                            last_transcript = transcript;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        // Model missing etc. — keep capturing but don't spam the log.
                        if last_batch_start.elapsed() > Duration::from_secs(60) {
                            log::warn!("MindScope: transcription unavailable: {}", e);
                        }
                    }
                }
                last_batch_start = std::time::Instant::now();
            }

            if super::recorder::is_audio_suppressed() { break; }

            // Stalled read: restart ffmpeg
            if last_batch_start.elapsed() > Duration::from_secs(15) { break; }
        }

        if let Some(mut c) = child_slot.lock_or_recover().take() {
            let _ = c.kill();
            let _ = c.wait();
        }

        if !running.load(Ordering::SeqCst) { break; }
        if got_data {
            consecutive_failures = 0;
            thread::sleep(Duration::from_millis(200));
        } else {
            consecutive_failures += 1;
            if consecutive_failures >= 3 {
                log::error!("MindScope: ffmpeg produced no audio 3 times in a row (no input device or mic denied); stopping audio capture");
                return;
            }
            thread::sleep(Duration::from_secs(2));
        }
    }
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
    /// Session type: "Zoom", "Tencent Meeting", "manual", etc.
    #[serde(default)]
    pub session_type: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct AudioIndex {
    segments: Vec<AudioSegment>,
}

/// Legacy whole-day index (rewritten per batch before the JSONL switch). Read-only now.
fn legacy_index_path(date: &str) -> PathBuf {
    paths::audio_root().join(format!("{}.json", date))
}

/// Append-only per-day transcript log: one JSON object per line.
fn jsonl_path(date: &str) -> PathBuf {
    paths::audio_root().join(format!("{}.jsonl", date))
}

fn save_audio_segment(date: &str, segment: AudioSegment) {
    if let Ok(line) = serde_json::to_string(&segment) {
        super::util::append_line(&jsonl_path(date), &line);
    }
}

pub fn load_audio_segments(date: &str) -> Vec<AudioSegment> {
    let mut out: Vec<AudioSegment> = fs::read_to_string(legacy_index_path(date))
        .ok()
        .and_then(|c| serde_json::from_str::<AudioIndex>(&c).ok())
        .map(|i| i.segments)
        .unwrap_or_default();
    if let Ok(content) = fs::read_to_string(jsonl_path(date)) {
        out.extend(content.lines().filter_map(|l| serde_json::from_str::<AudioSegment>(l).ok()));
    }
    out
}

/// Number of transcript segments stored for a date (cheap activity signal).
pub fn count_audio_segments(date: &str) -> usize {
    let legacy = fs::read_to_string(legacy_index_path(date))
        .ok()
        .and_then(|c| serde_json::from_str::<AudioIndex>(&c).ok())
        .map(|i| i.segments.len())
        .unwrap_or(0);
    let jsonl = fs::read_to_string(jsonl_path(date)).map(|c| c.lines().count()).unwrap_or(0);
    legacy + jsonl
}
