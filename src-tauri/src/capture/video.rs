//! HEVC video encoding/decoding via compiled Swift helpers
//! Based on Retrace's architecture: capture → encode → segment → extract
//! Encoder outputs JSON per frame: {"segment":"path","frame":N,"timestamp":"ISO8601"}
//! Frame extraction uses frame_reader helper with disk cache for fast seek

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

static ENCODER: Mutex<Option<EncoderProcess>> = Mutex::new(None);
/// Queue of encoder output lines (JSON per frame) — read by recorder
static ENCODER_OUTPUT: Mutex<VecDeque<EncoderFrame>> = Mutex::new(VecDeque::new());

struct EncoderProcess {
    child: Child,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct EncoderFrame {
    pub segment: String,
    pub frame: u32,
    pub timestamp: String,
}

fn helpers_dir() -> PathBuf {
    dirs_next::home_dir().unwrap_or_default().join(".mindscope").join("bin")
}

fn segments_dir() -> PathBuf {
    let dir = dirs_next::home_dir().unwrap_or_default()
        .join(".mindscope").join("data").join("segments");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn cache_dir() -> PathBuf {
    let dir = dirs_next::home_dir().unwrap_or_default()
        .join(".mindscope").join("data").join("cache");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Start the HEVC encoder background process + stdout reader thread
pub fn start_encoder() -> Result<(), String> {
    let encoder_bin = helpers_dir().join("hevc_encoder");
    if !encoder_bin.exists() {
        return Err("HEVC encoder not found. Run compilation first.".into());
    }

    let seg_dir = segments_dir();
    let mut child = Command::new(encoder_bin.to_str().unwrap())
        .arg(seg_dir.to_str().unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start encoder: {}", e))?;

    // Spawn thread to read encoder stdout JSON lines
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                if let Ok(line) = line {
                    if let Ok(frame) = serde_json::from_str::<EncoderFrame>(&line) {
                        let mut q = ENCODER_OUTPUT.lock().unwrap();
                        q.push_back(frame);
                        // Keep max 1000 entries to prevent memory growth
                        while q.len() > 1000 { q.pop_front(); }
                    }
                }
            }
        });
    }

    let mut guard = ENCODER.lock().unwrap();
    *guard = Some(EncoderProcess { child });

    log::info!("MindScope: HEVC encoder started");
    Ok(())
}

/// Send a frame (image file path) to the encoder
pub fn encode_frame(image_path: &Path) -> bool {
    let mut guard = ENCODER.lock().unwrap();
    if let Some(ref mut enc) = *guard {
        if let Some(ref mut stdin) = enc.child.stdin {
            let path_str = image_path.to_string_lossy();
            if writeln!(stdin, "{}", path_str).is_ok() {
                return true;
            }
        }
        log::warn!("MindScope: HEVC encoder process died, restarting...");
        *guard = None;
    }
    drop(guard);

    // Try to restart encoder
    if start_encoder().is_ok() {
        let mut guard = ENCODER.lock().unwrap();
        if let Some(ref mut enc) = *guard {
            if let Some(ref mut stdin) = enc.child.stdin {
                let path_str = image_path.to_string_lossy();
                return writeln!(stdin, "{}", path_str).is_ok();
            }
        }
    }
    false
}

/// Drain encoder output queue — called by recorder to get segment/frame info
pub fn drain_encoder_output() -> Vec<EncoderFrame> {
    let mut q = ENCODER_OUTPUT.lock().unwrap();
    q.drain(..).collect()
}

/// Stop the encoder gracefully
pub fn stop_encoder() {
    let mut guard = ENCODER.lock().unwrap();
    if let Some(ref mut enc) = *guard {
        if let Some(ref mut stdin) = enc.child.stdin {
            let _ = writeln!(stdin, "QUIT");
        }
        let _ = enc.child.wait();
    }
    *guard = None;
    log::info!("MindScope: HEVC encoder stopped");
}

/// Extract a single frame from an HEVC video segment as JPEG bytes
/// Uses disk cache: ~/.mindscope/data/cache/<hash>.jpg
pub fn extract_frame(video_path: &str, frame_index: u32) -> Option<Vec<u8>> {
    // Check disk cache first
    let cache_key = format!("{}_{}", video_path.replace('/', "_"), frame_index);
    let cache_path = cache_dir().join(format!("{}.jpg", cache_key));
    if cache_path.exists() {
        if let Ok(bytes) = std::fs::read(&cache_path) {
            return Some(bytes);
        }
    }

    // Extract via frame_reader helper
    let reader_bin = helpers_dir().join("frame_reader");
    if !reader_bin.exists() { return None; }

    let output = Command::new(reader_bin.to_str().unwrap())
        .args([video_path, &frame_index.to_string()])
        .output()
        .ok()?;

    if output.status.success() && !output.stdout.is_empty() {
        // Cache to disk
        let _ = std::fs::write(&cache_path, &output.stdout);
        // Evict old cache entries (keep max 500 files)
        evict_cache(500);
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

/// Evict oldest cache files if over limit
fn evict_cache(max_files: usize) {
    let dir = cache_dir();
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "jpg"))
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((modified, e.path()))
        })
        .collect();

    if entries.len() <= max_files { return; }

    entries.sort_by_key(|(t, _)| *t);
    let to_remove = entries.len() - max_files;
    for (_, path) in entries.iter().take(to_remove) {
        let _ = std::fs::remove_file(path);
    }
}

/// List all video segments
pub fn list_segments() -> Vec<SegmentInfo> {
    let seg_dir = segments_dir();
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
