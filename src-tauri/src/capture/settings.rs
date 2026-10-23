use serde::{Deserialize, Serialize};
use std::fs;

use super::paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub retention_days: u32,         // 7, 30, 90, 365, 0=forever
    pub capture_interval_secs: u64,
    pub jpeg_quality: f32,           // 0.0-1.0
    pub idle_threshold_secs: u64,
    pub excluded_apps: Vec<String>,
    pub capture_audio: bool,         // auto-record audio when a meeting is detected
    #[serde(default)]
    pub transcription_engine: Option<String>,  // "whisper" (default) or "system" (SFSpeech)
    #[serde(default = "default_true")]
    pub private_browsing: bool,  // skip Incognito/Private windows
    /// Explicit path to ffmpeg (file or directory). Empty/None = search PATH + ~/.local/bin etc.
    #[serde(default)]
    pub ffmpeg_path: Option<String>,
    /// Explicit path to the `claude` CLI. Empty/None = search PATH + ~/.local/bin etc.
    #[serde(default)]
    pub claude_path: Option<String>,
    /// Hard cap for everything under data/ (frames, segments, audio, cache, db). 0 = no cap.
    #[serde(default = "default_storage_budget_mb")]
    pub storage_budget_mb: u64,
    /// Max `claude` CLI invocations per local day across all background loops. 0 = unlimited.
    #[serde(default = "default_claude_daily_budget")]
    pub claude_daily_budget: u32,
    /// Capture height in pixels (the active display is downscaled to this before OCR/encode).
    #[serde(default = "default_capture_height")]
    pub capture_height: u32,
}

fn default_true() -> bool { true }
fn default_storage_budget_mb() -> u64 { 10 * 1024 }
fn default_claude_daily_budget() -> u32 { 120 }
fn default_capture_height() -> u32 { 1080 }

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
            ffmpeg_path: None,
            claude_path: None,
            storage_budget_mb: default_storage_budget_mb(),
            claude_daily_budget: default_claude_daily_budget(),
            capture_height: default_capture_height(),
        }
    }
}

pub fn load_settings() -> AppSettings {
    let path = paths::settings_path();
    if let Ok(content) = fs::read_to_string(&path) {
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        let settings = AppSettings::default();
        save_settings(&settings);
        settings
    }
}

pub fn save_settings(settings: &AppSettings) {
    let path = paths::settings_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(content) = serde_json::to_string_pretty(settings) {
        let _ = fs::write(&path, content);
    }
}
