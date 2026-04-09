use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub retention_days: u32,         // 7, 30, 90, 365, 0=forever
    pub capture_interval_secs: u64,
    pub jpeg_quality: f32,
    pub idle_threshold_secs: u64,
    pub excluded_apps: Vec<String>,
    pub capture_audio: bool,
    #[serde(default)]
    pub transcription_engine: Option<String>,  // "whisper" (default) or "system" (SFSpeech)
    #[serde(default)]
    pub private_browsing: bool,  // skip Incognito/Private windows
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            retention_days: 90,          // 3 months default
            capture_interval_secs: 5,
            jpeg_quality: 0.7,
            idle_threshold_secs: 60,
            excluded_apps: vec![],
            capture_audio: true,
            transcription_engine: None,
            private_browsing: true, // skip private windows by default
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageInfo {
    pub total_size_mb: f64,
    pub total_size_display: String,
    pub frame_count: u64,
    pub oldest_date: String,
    pub newest_date: String,
    pub days_stored: u32,
    pub avg_per_day_mb: f64,
}

fn settings_path() -> PathBuf {
    let home = dirs_next::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".mindscope").join("settings.json")
}

pub fn load_settings() -> AppSettings {
    let path = settings_path();
    if let Ok(content) = fs::read_to_string(&path) {
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        let settings = AppSettings::default();
        save_settings(&settings);
        settings
    }
}

pub fn save_settings(settings: &AppSettings) {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(content) = serde_json::to_string_pretty(settings) {
        let _ = fs::write(&path, content);
    }
}

pub fn get_storage_info() -> StorageInfo {
    let data_dir = super::storage::data_dir();
    let frames_dir = data_dir.join("frames");

    let mut total_size: u64 = 0;
    let mut frame_count: u64 = 0;
    let mut dates: Vec<String> = Vec::new();

    if let Ok(entries) = fs::read_dir(&frames_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let meta = entry.metadata();

            if let Ok(m) = &meta {
                if m.is_dir() {
                    // Count frames in this date directory
                    if let Ok(frames) = fs::read_dir(entry.path()) {
                        for f in frames.flatten() {
                            if let Ok(fm) = f.metadata() {
                                total_size += fm.len();
                                frame_count += 1;
                            }
                        }
                    }
                    dates.push(name);
                } else if m.is_file() {
                    total_size += m.len(); // JSON index files
                }
            }
        }
    }

    // Also count audio directory
    let audio_dir = data_dir.join("audio");
    if let Ok(entries) = fs::read_dir(&audio_dir) {
        for entry in entries.flatten() {
            if let Ok(m) = entry.metadata() {
                if m.is_dir() {
                    if let Ok(files) = fs::read_dir(entry.path()) {
                        for f in files.flatten() {
                            if let Ok(fm) = f.metadata() {
                                total_size += fm.len();
                            }
                        }
                    }
                }
            }
        }
    }

    dates.sort();
    let oldest = dates.first().cloned().unwrap_or_default();
    let newest = dates.last().cloned().unwrap_or_default();
    let days_stored = dates.len() as u32;

    let total_mb = total_size as f64 / (1024.0 * 1024.0);
    let avg_per_day = if days_stored > 0 { total_mb / days_stored as f64 } else { 0.0 };

    let display = if total_mb >= 1024.0 {
        format!("{:.1} GB", total_mb / 1024.0)
    } else {
        format!("{:.0} MB", total_mb)
    };

    StorageInfo {
        total_size_mb: total_mb,
        total_size_display: display,
        frame_count,
        oldest_date: oldest,
        newest_date: newest,
        days_stored,
        avg_per_day_mb: avg_per_day,
    }
}

/// Delete data older than retention_days. Returns number of days deleted.
pub fn cleanup_old_data(retention_days: u32) -> u32 {
    if retention_days == 0 {
        return 0; // Forever — don't delete
    }

    let data_dir = super::storage::data_dir();
    let frames_dir = data_dir.join("frames");
    let audio_dir = data_dir.join("audio");

    // Calculate cutoff date
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let cutoff_secs = now_secs - (retention_days as u64 * 86400);

    let mut deleted = 0u32;

    // Check each date directory
    for dir in [&frames_dir, &audio_dir] {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();

                // Parse date from directory name (YYYY-MM-DD) or file name (YYYY-MM-DD.json)
                let date_str = name.split('.').next().unwrap_or("");
                if date_str.len() != 10 { continue; }

                // Simple date comparison: parse to approximate seconds
                if let Some(date_secs) = date_to_approx_secs(date_str) {
                    if date_secs < cutoff_secs {
                        let path = entry.path();
                        if path.is_dir() {
                            let _ = fs::remove_dir_all(&path);
                            log::info!("MindScope: Deleted old data: {}", name);
                            deleted += 1;
                        } else if path.is_file() {
                            let _ = fs::remove_file(&path);
                        }
                    }
                }
            }
        }
    }

    deleted
}

fn date_to_approx_secs(date: &str) -> Option<u64> {
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 { return None; }
    let y: u64 = parts[0].parse().ok()?;
    let m: u64 = parts[1].parse().ok()?;
    let d: u64 = parts[2].parse().ok()?;

    // Approximate: days since epoch
    let days = (y - 1970) * 365 + (y - 1969) / 4 + (m - 1) * 30 + d;
    Some(days * 86400)
}
