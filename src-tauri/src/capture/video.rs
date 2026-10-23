//! HEVC video encoding/decoding via the bundled Swift helpers
//! (`hevc_encoder` persistent process, `frame_reader` one-shot).
//! Encoder stdin: `<jpeg path>\t<frame timestamp micros>` per line.
//! Encoder stdout: `{"segment":"path","frame":N,"timestamp":"ISO8601","ts":<micros>}` per frame —
//! `ts` is echoed back so the DB row of exactly that frame can be re-pointed at `video://`.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::Mutex;

use super::helpers;
use super::paths;
use super::util::LockExt;

static ENCODER: Mutex<Option<EncoderProcess>> = Mutex::new(None);
/// Queue of encoder output lines (JSON per frame) — drained by the recorder.
static ENCODER_OUTPUT: Mutex<VecDeque<EncoderFrame>> = Mutex::new(VecDeque::new());

struct EncoderProcess {
    child: Child,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct EncoderFrame {
    pub segment: String,
    pub frame: u32,
    #[serde(default)]
    pub timestamp: String,
    /// Frame timestamp (epoch micros) that MindScope passed in; 0 for legacy encoders.
    #[serde(default)]
    pub ts: i64,
}

pub fn encoder_available() -> bool {
    helpers::helper_path("hevc_encoder").is_some()
}

/// Start the HEVC encoder background process + stdout reader thread
pub fn start_encoder() -> Result<(), String> {
    let seg_dir = paths::segments_dir();
    let mut cmd = helpers::command("hevc_encoder")
        .ok_or_else(|| "hevc_encoder helper not bundled (build.rs compiles it from swift/)".to_string())?;
    let mut child = cmd
        .arg(seg_dir.to_string_lossy().as_ref())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to start encoder: {}", e))?;

    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if let Ok(frame) = serde_json::from_str::<EncoderFrame>(&line) {
                    let mut q = ENCODER_OUTPUT.lock_or_recover();
                    q.push_back(frame);
                    while q.len() > 1000 { q.pop_front(); }
                }
            }
        });
    }

    *ENCODER.lock_or_recover() = Some(EncoderProcess { child });
    log::info!("MindScope: HEVC encoder started");
    Ok(())
}

fn write_line(enc: &mut EncoderProcess, line: &str) -> bool {
    match enc.child.stdin.as_mut() {
        Some(stdin) => writeln!(stdin, "{}", line).is_ok(),
        None => false,
    }
}

/// Send a frame (image file path + its timestamp) to the encoder.
/// Restarts the encoder once if the pipe is broken.
pub fn encode_frame(image_path: &Path, ts_micros: i64) -> bool {
    let line = format!("{}\t{}", image_path.to_string_lossy(), ts_micros);
    {
        let mut guard = ENCODER.lock_or_recover();
        if let Some(enc) = guard.as_mut() {
            if write_line(enc, &line) { return true; }
            log::warn!("MindScope: HEVC encoder process died, restarting...");
            let _ = enc.child.kill();
            let _ = enc.child.wait();
            *guard = None;
        }
    }
    if start_encoder().is_ok() {
        if let Some(enc) = ENCODER.lock_or_recover().as_mut() {
            return write_line(enc, &line);
        }
    }
    false
}

/// Drain encoder output queue — called by recorder to get segment/frame info
pub fn drain_encoder_output() -> Vec<EncoderFrame> {
    ENCODER_OUTPUT.lock_or_recover().drain(..).collect()
}

/// Stop the encoder gracefully
pub fn stop_encoder() {
    let mut guard = ENCODER.lock_or_recover();
    if let Some(enc) = guard.as_mut() {
        let _ = write_line(enc, "QUIT");
        // Give it a moment to finalize the segment, then make sure it is gone.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match enc.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(50)),
                _ => { let _ = enc.child.kill(); let _ = enc.child.wait(); break; }
            }
        }
    }
    *guard = None;
    log::info!("MindScope: HEVC encoder stopped");
}

/// Extract a single frame from an HEVC video segment as JPEG bytes
/// Uses disk cache: data/cache/<hash>.jpg
pub fn extract_frame(video_path: &str, frame_index: u32) -> Option<Vec<u8>> {
    // Only ever read segments that live under our own data dir.
    let seg_root = paths::segments_dir();
    let vp = Path::new(video_path);
    if !vp.starts_with(&seg_root) || !vp.is_file() { return None; }

    let cache_key = format!("{}_{}", video_path.replace('/', "_"), frame_index);
    let cache_path = paths::cache_dir().join(format!("{}.jpg", cache_key));
    if let Ok(bytes) = std::fs::read(&cache_path) {
        return Some(bytes);
    }

    let idx = frame_index.to_string();
    let output = helpers::command("frame_reader")?
        .args([video_path, &idx])
        .stderr(Stdio::null())
        .output()
        .ok()?;

    if output.status.success() && !output.stdout.is_empty() {
        let _ = std::fs::write(&cache_path, &output.stdout);
        Some(output.stdout)
    } else {
        log::warn!("MindScope: frame extraction failed for {}:{}", video_path, frame_index);
        None
    }
}

/// Extract frame and return as base64 JPEG
pub fn extract_frame_base64(video_path: &str, frame_index: u32) -> Option<String> {
    let jpeg = extract_frame(video_path, frame_index)?;
    Some(super::db::base64_encode_bytes(&jpeg))
}

/// Evict oldest cache files if over `max_files` or `max_bytes`. Called by the retention loop.
pub fn evict_cache(max_files: usize, max_bytes: u64) -> (usize, u64) {
    let dir = paths::cache_dir();
    let mut entries: Vec<(std::time::SystemTime, PathBuf, u64)> = std::fs::read_dir(&dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "jpg"))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            Some((meta.modified().ok()?, e.path(), meta.len()))
        })
        .collect();
    entries.sort_by_key(|(t, _, _)| *t);
    let mut total: u64 = entries.iter().map(|e| e.2).sum();
    let mut count = entries.len();
    let mut removed = (0usize, 0u64);
    for (_, path, size) in entries {
        if count <= max_files && total <= max_bytes { break; }
        if std::fs::remove_file(&path).is_ok() {
            count -= 1;
            total -= size;
            removed.0 += 1;
            removed.1 += size;
        }
    }
    removed
}

/// List all video segments
pub fn list_segments() -> Vec<SegmentInfo> {
    let seg_dir = paths::segments_dir();
    let mut segments = Vec::new();

    if let Ok(dates) = std::fs::read_dir(&seg_dir) {
        for date_entry in dates.flatten() {
            if !date_entry.path().is_dir() { continue; }
            let date = date_entry.file_name().to_string_lossy().to_string();

            if let Ok(files) = std::fs::read_dir(date_entry.path()) {
                for file in files.flatten() {
                    let name = file.file_name().to_string_lossy().to_string();
                    if name.ends_with(".mp4") {
                        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
                        segments.push(SegmentInfo {
                            path: file.path().to_string_lossy().to_string(),
                            date: date.clone(),
                            filename: name,
                            size_bytes: size,
                        });
                    }
                }
            }
        }
    }

    segments.sort_by(|a, b| a.path.cmp(&b.path));
    segments
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SegmentInfo {
    pub path: String,
    pub date: String,
    pub filename: String,
    pub size_bytes: u64,
}
