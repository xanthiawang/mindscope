//! macOS-specific platform implementations
//! These wrap the existing Swift helpers and macOS APIs

use std::path::Path;
use std::process::Command;

/// OCR using Apple Vision framework via Swift helper
pub fn ocr_extract_text(image_path: &Path) -> String {
    let helper = dirs_next::home_dir()
        .unwrap_or_default()
        .join(".mindscope")
        .join("bin")
        .join("ocr_helper");

    if !helper.exists() {
        return String::new();
    }

    let output = Command::new(helper.to_str().unwrap_or(""))
        .arg(image_path.to_str().unwrap_or(""))
        .output()
        .ok();

    match output {
        Some(out) if out.status.success() => {
            let json_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if let Ok(result) = serde_json::from_str::<serde_json::Value>(&json_str) {
                result.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string()
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

/// OCR with regions using Apple Vision framework
pub fn ocr_extract_with_regions(image_path: &Path) -> super::super::ocr::OcrResult {
    let helper = dirs_next::home_dir()
        .unwrap_or_default()
        .join(".mindscope")
        .join("bin")
        .join("ocr_helper");

    if !helper.exists() {
        return super::super::ocr::OcrResult::default();
    }

    let output = Command::new(helper.to_str().unwrap_or(""))
        .arg(image_path.to_str().unwrap_or(""))
        .output()
        .ok();

    match output {
        Some(out) if out.status.success() => {
            let json_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
            serde_json::from_str(&json_str).unwrap_or_default()
        }
        _ => super::super::ocr::OcrResult::default(),
    }
}

/// Get active window info using Swift helper (NSWorkspace + Accessibility API)
pub fn get_active_window_info() -> (String, String, String, String) {
    let helper = dirs_next::home_dir()
        .unwrap_or_default()
        .join(".mindscope")
        .join("bin")
        .join("active_app");

    if helper.exists() {
        if let Ok(out) = Command::new(helper.to_str().unwrap_or("")).output() {
            let result = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let parts: Vec<&str> = result.splitn(4, '|').collect();
            if parts.len() >= 2 && !parts[0].is_empty() && parts[0] != "Unknown" {
                let bundle_id = parts.get(2).unwrap_or(&"").to_string();
                let browser_url = parts.get(3).unwrap_or(&"").to_string();
                return (parts[0].to_string(), parts[1].to_string(), bundle_id, browser_url);
            }
        }
    }

    // Fallback: AppleScript
    let script = r#"
tell application "System Events"
    set frontApp to name of first application process whose frontmost is true
    try
        set frontWindow to name of front window of (first application process whose frontmost is true)
    on error
        set frontWindow to ""
    end try
    return frontApp & "|" & frontWindow
end tell
"#;
    let output = Command::new("osascript").args(["-e", script]).output().ok();
    if let Some(out) = output {
        let result = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Some((app, window)) = result.split_once('|') {
            return (app.to_string(), window.to_string(), String::new(), String::new());
        }
    }
    ("Unknown".to_string(), String::new(), String::new(), String::new())
}

/// Get screen info using Swift helper
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ScreenInfo {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub scale: f64,
    pub is_main: bool,
    pub mouse: bool,
    pub frame_x: f64,
    pub frame_y: f64,
    pub frame_w: f64,
    pub frame_h: f64,
}

impl Default for ScreenInfo {
    fn default() -> Self {
        Self {
            x: 0.0, y: 34.0, w: 1440.0, h: 866.0, scale: 2.0,
            is_main: true, mouse: true,
            frame_x: 0.0, frame_y: 0.0, frame_w: 1440.0, frame_h: 900.0,
        }
    }
}

pub fn get_all_screens() -> Vec<ScreenInfo> {
    let helper = dirs_next::home_dir()
        .unwrap_or_default()
        .join(".mindscope")
        .join("bin")
        .join("screen_info");

    if helper.exists() {
        if let Ok(out) = Command::new(helper.to_str().unwrap_or("")).output() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if let Ok(screens) = serde_json::from_str::<Vec<ScreenInfo>>(&s) {
                if !screens.is_empty() {
                    return screens;
                }
            }
        }
    }
    vec![ScreenInfo::default()]
}

/// Open screen recording permission settings
pub fn open_permission_settings() {
    let _ = Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        .spawn();
}

/// Get ffmpeg audio input device specifier
pub fn get_audio_input_device() -> &'static str {
    ":default"
}

/// Get ffmpeg audio input format
pub fn get_audio_input_format() -> &'static str {
    "avfoundation"
}

/// Find ffmpeg executable
pub fn find_ffmpeg() -> Option<String> {
    let paths = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "ffmpeg"];
    for path in &paths {
        if std::path::Path::new(path).exists() || *path == "ffmpeg" {
            if Command::new(path).arg("-version").output().is_ok() {
                return Some(path.to_string());
            }
        }
    }
    None
}

/// Convert audio to WAV using afconvert (macOS built-in)
pub fn convert_audio_to_wav(input_path: &Path, output_path: &Path) -> Result<(), String> {
    let status = Command::new("afconvert")
        .args([
            "-d", "LEI16",
            "-c", "1",
            "-r", "16000",
            input_path.to_str().unwrap_or(""),
            output_path.to_str().unwrap_or(""),
        ])
        .status()
        .map_err(|e| format!("afconvert failed: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        Err("afconvert failed".into())
    }
}

/// Kill audio recording processes
pub fn kill_audio_processes() {
    let _ = Command::new("pkill").args(["-f", "afrecord"]).status();
    let _ = Command::new("pkill").args(["-f", "ffmpeg.*avfoundation"]).status();
}

/// Check if ffmpeg is currently recording audio
pub fn is_ffmpeg_recording() -> bool {
    Command::new("pgrep")
        .args(["-f", "ffmpeg.*avfoundation"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Find Claude CLI executable
pub fn find_claude_cli() -> Option<String> {
    let paths = ["/opt/homebrew/bin/claude", "/usr/local/bin/claude"];
    for path in &paths {
        if std::path::Path::new(path).exists() {
            return Some(path.to_string());
        }
    }
    None
}
