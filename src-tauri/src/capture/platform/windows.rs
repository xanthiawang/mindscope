//! Windows-specific platform implementations
//! Uses Win32 API and Windows Runtime APIs for OCR, window detection, etc.

use std::path::Path;
use std::process::Command;

use windows::{
    core::*,
    Win32::Foundation::*,
    Win32::UI::WindowsAndMessaging::*,
    Win32::System::Threading::*,
    Win32::Graphics::Gdi::*,
};

/// OCR using Windows.Media.Ocr API
pub fn ocr_extract_text(image_path: &Path) -> String {
    match ocr_extract_with_regions(image_path) {
        result => result.text,
    }
}

/// OCR with regions using Windows.Media.Ocr API
pub fn ocr_extract_with_regions(image_path: &Path) -> super::super::ocr::OcrResult {
use windows::{
    Media::Ocr::OcrEngine,
    Graphics::Imaging::BitmapDecoder,
    Storage::StorageFile,
};

    let result = (|| -> Result<super::super::ocr::OcrResult> {
        let path_str = image_path.to_str().ok_or(Error::from(E_INVALIDARG))?;
        let path_hstring = HSTRING::from(path_str);

        // Open the image file
        let file = StorageFile::GetFileFromPathAsync(&path_hstring)?.get()?;
        let stream = file.OpenAsync(windows::Storage::FileAccessMode::Read)?.get()?;

        // Decode the image
        let decoder = BitmapDecoder::CreateAsync(&stream)?.get()?;
        let bitmap = decoder.GetSoftwareBitmapAsync()?.get()?;

        // Create OCR engine with default language
        let engine = OcrEngine::TryCreateFromUserProfileLanguages()?;

        // Perform OCR
        let ocr_result = engine.RecognizeAsync(&bitmap)?.get()?;

        let mut full_text = String::new();
        let mut regions = Vec::new();

        // Get image dimensions for normalization
        let img_width = bitmap.PixelWidth()? as f64;
        let img_height = bitmap.PixelHeight()? as f64;

        // Extract text and regions from OCR result
        for line in ocr_result.Lines()? {
            let line_text = line.Text()?.to_string();
            full_text.push_str(&line_text);
            full_text.push('\n');

            // Get bounding box for the line
            for word in line.Words()? {
                let bounds = word.BoundingRect()?;
                let word_text = word.Text()?.to_string();

                regions.push(super::super::ocr::OcrRegion {
                    text: word_text,
                    x: bounds.X as f64 / img_width,
                    y: bounds.Y as f64 / img_height,
                    w: bounds.Width as f64 / img_width,
                    h: bounds.Height as f64 / img_height,
                });
            }
        }

        Ok(super::super::ocr::OcrResult {
            text: full_text.trim().to_string(),
            regions,
        })
    })();

    result.unwrap_or_default()
}

/// Get active window info using Win32 API
pub fn get_active_window_info() -> (String, String, String, String) {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return ("Unknown".to_string(), String::new(), String::new(), String::new());
        }

        // Get window title
        let mut title_buf = [0u16; 512];
        let title_len = GetWindowTextW(hwnd, &mut title_buf);
        let window_title = if title_len > 0 {
            String::from_utf16_lossy(&title_buf[..title_len as usize])
        } else {
            String::new()
        };

        // Get process ID
        let mut process_id: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));

        // Get process name
        let app_name = get_process_name(process_id).unwrap_or_else(|| "Unknown".to_string());

        // Get executable path (as bundle_id equivalent)
        let exe_path = get_process_path(process_id).unwrap_or_default();

        (app_name, window_title, exe_path, String::new())
    }
}

/// Get process name from process ID
fn get_process_name(process_id: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id).ok()?;

        let mut name_buf = [0u16; 260];
        let mut size = name_buf.len() as u32;

        if QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(name_buf.as_mut_ptr()), &mut size).is_ok() {
            let _ = CloseHandle(handle);
            let full_path = String::from_utf16_lossy(&name_buf[..size as usize]);
            // Extract just the filename
            full_path.rsplit('\\').next().map(|s| s.trim_end_matches(".exe").to_string())
        } else {
            let _ = CloseHandle(handle);
            None
        }
    }
}

/// Get full process path from process ID
fn get_process_path(process_id: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id).ok()?;

        let mut path_buf = [0u16; 260];
        let mut size = path_buf.len() as u32;

        if QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(path_buf.as_mut_ptr()), &mut size).is_ok() {
            let _ = CloseHandle(handle);
            Some(String::from_utf16_lossy(&path_buf[..size as usize]))
        } else {
            let _ = CloseHandle(handle);
            None
        }
    }
}

/// Screen information structure
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
            x: 0.0, y: 0.0, w: 1920.0, h: 1080.0, scale: 1.0,
            is_main: true, mouse: true,
            frame_x: 0.0, frame_y: 0.0, frame_w: 1920.0, frame_h: 1080.0,
        }
    }
}

/// Get all screens info using Win32 API
pub fn get_all_screens() -> Vec<ScreenInfo> {
    let mut screens = Vec::new();
    let mut mouse_pos = POINT::default();

    unsafe {
        let _ = GetCursorPos(&mut mouse_pos);
    }

    unsafe extern "system" fn enum_monitors_callback(
        hmonitor: HMONITOR,
        _hdc: HDC,
        _lprect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let screens = &mut *(lparam.0 as *mut Vec<(HMONITOR, MONITORINFOEXW)>);

        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;

        if GetMonitorInfoW(hmonitor, &mut info.monitorInfo as *mut _ as *mut MONITORINFO).as_bool() {
            screens.push((hmonitor, info));
        }

        BOOL(1)
    }

    let mut monitor_data: Vec<(HMONITOR, MONITORINFOEXW)> = Vec::new();

    unsafe {
        let _ = EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(enum_monitors_callback),
            LPARAM(&mut monitor_data as *mut _ as isize),
        );
    }

    for (_, info) in monitor_data {
        let rect = info.monitorInfo.rcMonitor;
        let work_rect = info.monitorInfo.rcWork;
        let is_primary = (info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0;

        // Check if mouse is on this monitor
        let mouse_on_screen = mouse_pos.x >= rect.left
            && mouse_pos.x < rect.right
            && mouse_pos.y >= rect.top
            && mouse_pos.y < rect.bottom;

        // Get DPI scale (Windows 10+)
        let scale = get_monitor_dpi_scale(&info);

        screens.push(ScreenInfo {
            x: work_rect.left as f64,
            y: work_rect.top as f64,
            w: (work_rect.right - work_rect.left) as f64,
            h: (work_rect.bottom - work_rect.top) as f64,
            scale,
            is_main: is_primary,
            mouse: mouse_on_screen,
            frame_x: rect.left as f64,
            frame_y: rect.top as f64,
            frame_w: (rect.right - rect.left) as f64,
            frame_h: (rect.bottom - rect.top) as f64,
        });
    }

    if screens.is_empty() {
        screens.push(ScreenInfo::default());
    }

    screens
}

/// Get monitor DPI scale factor
fn get_monitor_dpi_scale(_info: &MONITORINFOEXW) -> f64 {
    // Default to 1.0 - proper DPI detection requires shcore.dll
    // which adds complexity. For now, use system DPI as approximation.
    unsafe {
        let hdc = GetDC(HWND::default());
        if !hdc.0.is_null() {
            let dpi = GetDeviceCaps(hdc, LOGPIXELSX);
            ReleaseDC(HWND::default(), hdc);
            return dpi as f64 / 96.0;
        }
    }
    1.0
}

/// Open Windows Settings (no equivalent permission dialog needed)
pub fn open_permission_settings() {
    // Windows doesn't require explicit screen recording permission
    // Open Windows Settings as a fallback
    let _ = Command::new("explorer")
        .arg("ms-settings:privacy")
        .spawn();
}

/// Get ffmpeg audio input device specifier for Windows
pub fn get_audio_input_device() -> &'static str {
    "audio=@device_cm_{33D9A762-90C8-11D0-BD43-00A0C911CE86}\\wave_{00000000-0000-0000-0000-000000000000}"
}

/// Get ffmpeg audio input format for Windows
pub fn get_audio_input_format() -> &'static str {
    "dshow"
}

/// Find ffmpeg executable on Windows
pub fn find_ffmpeg() -> Option<String> {
    // Check common installation paths
    let paths = [
        r"C:\ffmpeg\bin\ffmpeg.exe",
        r"C:\Program Files\ffmpeg\bin\ffmpeg.exe",
        r"C:\Program Files (x86)\ffmpeg\bin\ffmpeg.exe",
        "ffmpeg", // PATH lookup
    ];

    for path in &paths {
        if std::path::Path::new(path).exists() || *path == "ffmpeg" {
            if Command::new(path).arg("-version").output().is_ok() {
                return Some(path.to_string());
            }
        }
    }

    // Try to find ffmpeg in PATH
    if let Ok(output) = Command::new("where").arg("ffmpeg").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(path.lines().next().unwrap_or("ffmpeg").to_string());
            }
        }
    }

    None
}

/// Convert audio to WAV using ffmpeg (Windows doesn't have afconvert)
pub fn convert_audio_to_wav(input_path: &Path, output_path: &Path) -> std::result::Result<(), String> {
    let ffmpeg = find_ffmpeg().ok_or_else(|| "ffmpeg not found".to_string())?;

    let status = Command::new(&ffmpeg)
        .args([
            "-i", input_path.to_str().unwrap_or(""),
            "-acodec", "pcm_s16le",
            "-ac", "1",
            "-ar", "16000",
            "-y",
            output_path.to_str().unwrap_or(""),
        ])
        .status()
        .map_err(|e| format!("ffmpeg failed: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        Err("ffmpeg conversion failed".to_string())
    }
}

/// Kill audio recording processes on Windows
pub fn kill_audio_processes() {
    // Kill ffmpeg processes
    let _ = Command::new("taskkill")
        .args(["/F", "/IM", "ffmpeg.exe"])
        .output();
}

/// Check if ffmpeg is currently recording audio on Windows
pub fn is_ffmpeg_recording() -> bool {
    Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq ffmpeg.exe"])
        .output()
        .map(|o| {
            let output = String::from_utf8_lossy(&o.stdout);
            output.contains("ffmpeg.exe")
        })
        .unwrap_or(false)
}

/// Find Claude CLI executable on Windows
pub fn find_claude_cli() -> Option<String> {
    // Check common paths
    let home = dirs_next::home_dir().unwrap_or_default();
    let paths = [
        home.join(".claude").join("claude.exe"),
        home.join("AppData").join("Local").join("Programs").join("claude").join("claude.exe"),
        std::path::PathBuf::from(r"C:\Program Files\Claude\claude.exe"),
    ];

    for path in &paths {
        if path.exists() {
            return Some(path.to_string_lossy().to_string());
        }
    }

    // Try to find in PATH
    if let Ok(output) = Command::new("where").arg("claude").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(path.lines().next().unwrap_or("claude").to_string());
            }
        }
    }

    None
}

/// List available audio input devices for Windows
pub fn list_audio_devices() -> Vec<String> {
    let ffmpeg = match find_ffmpeg() {
        Some(f) => f,
        None => return Vec::new(),
    };

    let output = Command::new(&ffmpeg)
        .args(["-list_devices", "true", "-f", "dshow", "-i", "dummy"])
        .output();

    let mut devices = Vec::new();
    if let Ok(out) = output {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let mut in_audio_section = false;

        for line in stderr.lines() {
            if line.contains("DirectShow audio devices") {
                in_audio_section = true;
                continue;
            }
            if line.contains("DirectShow video devices") {
                in_audio_section = false;
            }
            if in_audio_section && line.contains("]  \"") {
                if let Some(start) = line.find('"') {
                    if let Some(end) = line.rfind('"') {
                        if end > start {
                            devices.push(line[start + 1..end].to_string());
                        }
                    }
                }
            }
        }
    }

    devices
}

/// Get the default audio input device name
pub fn get_default_audio_device() -> String {
    let devices = list_audio_devices();
    devices
        .into_iter()
        .find(|d| d.to_lowercase().contains("microphone"))
        .unwrap_or_else(|| "Microphone".to_string())
}

/// Returns the canonical display name for a known meeting process, or None.
/// Process name comparison is case-insensitive.
fn known_meeting_process_name(process: &str) -> Option<&'static str> {
    let p = process.to_lowercase();
    if p.contains("zoom") { return Some("Zoom"); }
    if p.contains("teams") || p.contains("ms-teams") { return Some("Microsoft Teams"); }
    if p.contains("lark") { return Some("Lark"); }
    if p.contains("dingtalk") { return Some("DingTalk"); }
    if p.contains("wemeet") { return Some("WeMeet"); }
    if p.contains("tencentmeeting") { return Some("Tencent Meeting"); }
    if p.contains("webex") { return Some("Webex"); }
    if p.contains("discord") { return Some("Discord"); }
    None
}

/// Returns the name of the active meeting app (e.g. "Zoom"), or None.
/// Checks running processes via tasklist; also checks Google Meet via Chrome window title.
pub fn get_meeting_app_name() -> Option<String> {
    let output = Command::new("tasklist")
        .args(["/FO", "CSV", "/NH"])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut chrome_running = false;

    for line in stdout.lines() {
        let process = line.split(',').next()?.trim_matches('"');
        if let Some(name) = known_meeting_process_name(process) {
            return Some(name.to_string());
        }
        if process.to_lowercase().contains("chrome") {
            chrome_running = true;
        }
    }

    // Google Meet runs inside Chrome — detect via foreground window title
    if chrome_running {
        let (_, title, _, _) = get_active_window_info();
        let t = title.to_lowercase();
        if t.contains("meet") && (t.contains("google") || t.contains("meet.google")) {
            return Some("Google Meet".to_string());
        }
    }

    None
}

/// Returns true if any known meeting app is currently running.
pub fn is_meeting_app_running() -> bool {
    get_meeting_app_name().is_some()
}

/// Returns the taskbar height in physical pixels plus an 8px gap.
/// Uses SHAppBarMessage(ABM_GETTASKBARPOS). Returns 48 (safe default) on failure.
pub fn get_taskbar_height() -> u32 {
    use windows::Win32::UI::Shell::{SHAppBarMessage, APPBARDATA, ABM_GETTASKBARPOS};
    use windows::Win32::Foundation::RECT;

    unsafe {
        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: windows::Win32::Foundation::HWND::default(),
            uCallbackMessage: 0,
            uEdge: 0,
            rc: RECT::default(),
            lParam: windows::Win32::Foundation::LPARAM(0),
        };
        SHAppBarMessage(ABM_GETTASKBARPOS, &mut abd);
        let rect = abd.rc;
        let height = (rect.bottom - rect.top).max(0) as u32;
        if height == 0 { 48 } else { height + 8 }
    }
}

#[cfg(test)]
mod tests {
    use super::known_meeting_process_name;

    #[test]
    fn test_known_meeting_process_zoom() {
        assert_eq!(known_meeting_process_name("zoom.exe"), Some("Zoom"));
        assert_eq!(known_meeting_process_name("Zoom.exe"), Some("Zoom"));
    }

    #[test]
    fn test_known_meeting_process_teams() {
        assert_eq!(known_meeting_process_name("Teams.exe"), Some("Microsoft Teams"));
        assert_eq!(known_meeting_process_name("ms-teams.exe"), Some("Microsoft Teams"));
    }

    #[test]
    fn test_known_meeting_process_others() {
        assert_eq!(known_meeting_process_name("lark.exe"), Some("Lark"));
        assert_eq!(known_meeting_process_name("DingTalk.exe"), Some("DingTalk"));
        assert_eq!(known_meeting_process_name("wemeet.exe"), Some("WeMeet"));
        assert_eq!(known_meeting_process_name("webex.exe"), Some("Webex"));
        assert_eq!(known_meeting_process_name("discord.exe"), Some("Discord"));
    }

    #[test]
    fn test_known_meeting_process_unknown() {
        assert_eq!(known_meeting_process_name("notepad.exe"), None);
        assert_eq!(known_meeting_process_name("chrome.exe"), None);
        assert_eq!(known_meeting_process_name(""), None);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_taskbar_height_reasonable() {
        let h = super::get_taskbar_height();
        assert!(h >= 32, "taskbar height too small: {}", h);
        assert!(h <= 200, "taskbar height unreasonably large: {}", h);
    }
}
