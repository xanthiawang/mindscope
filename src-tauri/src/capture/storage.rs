use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// A single captured frame with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedFrame {
    pub id: u64,
    pub timestamp: String,
    pub app_name: String,
    pub window_name: String,
    pub ocr_text: String,
    pub image_path: String,
}

/// Frame index stored as JSON for fast lookup
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FrameIndex {
    pub frames: Vec<CapturedFrame>,
}

/// Get the storage directory for MindScope data
pub fn data_dir() -> PathBuf {
    let home = dirs_next::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = home.join(".mindscope").join("data");
    fs::create_dir_all(&dir).ok();
    dir
}

/// Get the frames directory for a specific date
pub fn frames_dir(date: &str) -> PathBuf {
    let dir = data_dir().join("frames").join(date);
    fs::create_dir_all(&dir).ok();
    dir
}

/// Get the index file path for a specific date
fn index_path(date: &str) -> PathBuf {
    data_dir().join("frames").join(format!("{}.json", date))
}

/// Load frame index for a date
pub fn load_index(date: &str) -> FrameIndex {
    let path = index_path(date);
    if let Ok(content) = fs::read_to_string(&path) {
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        FrameIndex::default()
    }
}

/// Save frame index for a date
pub fn save_index(date: &str, index: &FrameIndex) {
    let path = index_path(date);
    if let Ok(content) = serde_json::to_string_pretty(index) {
        let _ = fs::write(&path, content);
    }
}

/// Add a frame to the index
pub fn add_frame(date: &str, frame: CapturedFrame) {
    let mut index = load_index(date);
    index.frames.push(frame);
    save_index(date, &index);
}

/// Get all frames for a date, optionally filtered by hour
pub fn get_frames(date: &str, hour: Option<u32>) -> Vec<CapturedFrame> {
    let index = load_index(date);
    match hour {
        Some(h) => {
            index.frames.into_iter().filter(|f| {
                // Parse hour from timestamp like "2026-04-09T14:30:00"
                f.timestamp.get(11..13)
                    .and_then(|s| s.parse::<u32>().ok())
                    .map(|fh| fh == h)
                    .unwrap_or(false)
            }).collect()
        }
        None => index.frames,
    }
}

/// Read a frame's image as base64
pub fn read_frame_image_base64(image_path: &str) -> Option<String> {
    let bytes = fs::read(image_path).ok()?;
    Some(base64_encode(&bytes))
}

/// Activity segment: a continuous period of the same app
/// Used by the frontend to build a compressed timeline that skips idle gaps
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivitySegment {
    pub app_name: String,
    pub window_name: String,
    pub start_ts: String,
    pub end_ts: String,
    pub start_idx: usize,
    pub end_idx: usize,
    pub frame_count: usize,
    pub is_idle: bool,
}

/// Build activity segments from frames, marking idle periods
/// A segment is "idle" if the same app+window is held for > idle_threshold consecutive frames
/// with no OCR text change (screen hasn't meaningfully changed)
pub fn build_activity_segments(frames: &[CapturedFrame], idle_threshold_secs: u64) -> Vec<ActivitySegment> {
    if frames.is_empty() {
        return vec![];
    }

    let mut segments: Vec<ActivitySegment> = Vec::new();
    let mut seg_start = 0;
    let mut seg_app = &frames[0].app_name;
    let mut seg_window = &frames[0].window_name;

    for i in 1..=frames.len() {
        let changed = if i < frames.len() {
            frames[i].app_name != *seg_app || frames[i].window_name != *seg_window
        } else {
            true // Force close last segment
        };

        if changed {
            // Calculate duration of this segment
            let start_ts = &frames[seg_start].timestamp;
            let end_ts = &frames[i - 1].timestamp;
            let duration_secs = timestamp_diff_secs(start_ts, end_ts);
            let frame_count = i - seg_start;

            // Check if this is idle: same context for a long time with few unique OCR texts
            let unique_ocr_count = count_unique_ocr(&frames[seg_start..i]);
            let is_idle = duration_secs > idle_threshold_secs && unique_ocr_count <= 1 && frame_count > 3;

            segments.push(ActivitySegment {
                app_name: seg_app.clone(),
                window_name: seg_window.clone(),
                start_ts: start_ts.clone(),
                end_ts: end_ts.clone(),
                start_idx: seg_start,
                end_idx: i - 1,
                frame_count,
                is_idle,
            });

            if i < frames.len() {
                seg_start = i;
                seg_app = &frames[i].app_name;
                seg_window = &frames[i].window_name;
            }
        }
    }

    segments
}

/// Get frames with idle frames removed (compressed timeline)
pub fn get_frames_compressed(date: &str, idle_threshold_secs: u64) -> (Vec<CapturedFrame>, Vec<ActivitySegment>) {
    let all_frames = get_frames(date, None);
    let segments = build_activity_segments(&all_frames, idle_threshold_secs);

    // Build compressed frame list: keep first+last frame of idle segments, all frames of active segments
    let mut compressed: Vec<CapturedFrame> = Vec::new();
    for seg in &segments {
        if seg.is_idle {
            // Keep only first and last frame of idle period
            if seg.start_idx < all_frames.len() {
                compressed.push(all_frames[seg.start_idx].clone());
            }
            if seg.end_idx != seg.start_idx && seg.end_idx < all_frames.len() {
                compressed.push(all_frames[seg.end_idx].clone());
            }
        } else {
            // Keep all frames from active segment
            for idx in seg.start_idx..=seg.end_idx.min(all_frames.len() - 1) {
                compressed.push(all_frames[idx].clone());
            }
        }
    }

    (compressed, segments)
}

/// Count unique non-empty OCR texts in a slice
fn count_unique_ocr(frames: &[CapturedFrame]) -> usize {
    let mut seen = std::collections::HashSet::new();
    for f in frames {
        if !f.ocr_text.is_empty() {
            seen.insert(&f.ocr_text);
        }
    }
    seen.len()
}

/// Parse timestamp "2026-04-09T14:30:00" and compute difference in seconds
fn timestamp_diff_secs(start: &str, end: &str) -> u64 {
    let parse = |ts: &str| -> u64 {
        // Extract HH:MM:SS from "YYYY-MM-DDThh:mm:ss"
        let h: u64 = ts.get(11..13).and_then(|s| s.parse().ok()).unwrap_or(0);
        let m: u64 = ts.get(14..16).and_then(|s| s.parse().ok()).unwrap_or(0);
        let s: u64 = ts.get(17..19).and_then(|s| s.parse().ok()).unwrap_or(0);
        h * 3600 + m * 60 + s
    };
    let s = parse(start);
    let e = parse(end);
    if e >= s { e - s } else { 0 }
}

/// Estimate storage usage
pub fn storage_usage_mb() -> f64 {
    let dir = data_dir();
    dir_size(&dir) as f64 / (1024.0 * 1024.0)
}

fn dir_size(path: &Path) -> u64 {
    let mut size = 0;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let meta = entry.metadata();
            if let Ok(m) = meta {
                if m.is_dir() {
                    size += dir_size(&entry.path());
                } else {
                    size += m.len();
                }
            }
        }
    }
    size
}

fn base64_encode(bytes: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(bytes.len() * 4 / 3 + 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 { result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char); } else { result.push('='); }
        if chunk.len() > 2 { result.push(CHARS[(triple & 0x3F) as usize] as char); } else { result.push('='); }
    }
    result
}
