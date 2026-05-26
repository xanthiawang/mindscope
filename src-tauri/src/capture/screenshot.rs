use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::platform;

static PERMISSION_CONFIRMED: AtomicBool = AtomicBool::new(false);

pub fn mark_permission_granted() {
    PERMISSION_CONFIRMED.store(true, Ordering::Relaxed);
}

pub fn check_screen_permission() -> bool {
    // On Windows, screen recording doesn't require explicit permission
    #[cfg(target_os = "windows")]
    {
        return true;
    }

    #[cfg(target_os = "macos")]
    {
        PERMISSION_CONFIRMED.load(Ordering::Relaxed)
    }
}

pub fn has_screen_permission() -> bool {
    check_screen_permission()
}

pub fn open_permission_settings() {
    platform::open_permission_settings();
}

/// Get the topmost non-MindScope app window.
/// Returns (app_name, window_title, pid, window_id) or None.
#[cfg(target_os = "macos")]
fn get_topmost_app_window() -> Option<(String, String, u32, u32)> {
    let helper = dirs_next::home_dir().unwrap_or_default()
        .join(".mindscope").join("bin").join("topmost_window");
    if !helper.exists() { return None; }

    let output = std::process::Command::new(helper.to_str().unwrap_or(""))
        .output().ok()?;
    let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if result == "NONE" || result.is_empty() { return None; }

    let parts: Vec<&str> = result.splitn(4, '|').collect();
    if parts.len() < 4 { return None; }
    let app = parts[0].to_string();
    let title = parts[1].to_string();
    let pid: u32 = parts[2].parse().ok()?;
    let wid: u32 = parts[3].parse().ok()?;
    Some((app, title, pid, wid))
}

#[cfg(target_os = "windows")]
fn get_topmost_app_window() -> Option<(String, String, u32, u32)> {
    let (app, title, _, _) = platform::get_active_window_info();
    if app == "Unknown" {
        None
    } else {
        Some((app, title, 0, 0))
    }
}

/// Capture screenshot — uses CGWindowList z-order (via Swift helper) to find
/// the topmost non-MindScope app, then captures that specific window via xcap.
/// This mirrors Screenpipe's approach of filtering own UI from screen captures.
pub fn capture_screen(output_dir: &Path) -> Option<PathBuf> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    let filename = format!("frame_{}.jpg", timestamp);
    let filepath = output_dir.join(&filename);

    // Multi-monitor support: find the monitor containing the frontmost app
    // via CGWindowList (via topmost_window helper). Otherwise iterate all monitors
    // and pick the one with the most "content" (highest std deviation).
    let target_app = get_topmost_app_window().map(|(a, _, _, _)| a);

    if let Ok(monitors) = xcap::Monitor::all() {
        // Try to capture each monitor and pick the one with most content
        let mut best_image: Option<image::DynamicImage> = None;
        let mut best_std: f64 = 0.0;

        for monitor in &monitors {
            if let Ok(image) = monitor.capture_image() {
                // Quick std calculation on a downsample
                let dyn_img = image::DynamicImage::ImageRgba8(image);
                let small = dyn_img.thumbnail(200, 200);
                let pixels: Vec<u8> = small.to_rgb8().into_raw();
                let mean: f64 = pixels.iter().map(|&p| p as f64).sum::<f64>() / pixels.len() as f64;
                let variance: f64 = pixels.iter()
                    .map(|&p| (p as f64 - mean).powi(2))
                    .sum::<f64>() / pixels.len() as f64;
                let std = variance.sqrt();

                if std > best_std {
                    best_std = std;
                    best_image = Some(dyn_img);
                }
            }
        }

        if let Some(img) = best_image {
            let mut buf = std::io::Cursor::new(Vec::new());
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 45);
            if encoder.encode_image(&img).is_ok() {
                if std::fs::write(&filepath, buf.into_inner()).is_ok() {
                    PERMISSION_CONFIRMED.store(true, Ordering::Relaxed);
                    let _ = target_app; // reserved
                    return Some(filepath);
                }
            }
        }
    }
    None
}

/// Get active app info — uses CGWindowList to find topmost non-MindScope window.
/// This is used by the recorder for accurate app tracking even when MindScope overlays the screen.
pub fn get_active_app_via_zorder() -> Option<(String, String)> {
    get_topmost_app_window().map(|(app, title, _, _)| (app, title))
}

/// Returns (app_name, window_title, bundle_id, browser_url)
pub fn get_active_window_info() -> (String, String, String, String) {
    // Priority 1: Use z-order detection (excludes MindScope itself)
    if let Some((app, title)) = get_active_app_via_zorder() {
        if !app.is_empty() && !app.eq_ignore_ascii_case("mindscope") {
            return (app, title, String::new(), String::new());
        }
    }

    // Priority 2: Use platform-specific implementation
    platform::get_active_window_info()
}

// --- Multi-window capture (ported from Screenpipe) ---

/// A single captured window with its metadata.
pub struct WindowCapture {
    pub app_name: String,
    pub window_name: String,
    pub image: image::DynamicImage,
}

/// System UI apps that should never be captured.
/// Adapted from Screenpipe's SKIP_APPS list for macOS.
const SKIP_APPS: &[&str] = &[
    "Window Server",
    "SystemUIServer",
    "ControlCenter",
    "Dock",
    "NotificationCenter",
    "loginwindow",
    "WindowManager",
    "Contexts",
    "Screenshot",
    "MindScope",
];

/// Window titles that indicate system UI elements to skip.
const SKIP_TITLES: &[&str] = &[
    "Item-0",
    "App Icon Window",
    "Dock",
    "Menu Bar",
    "Notification Center",
    "Control Center",
    "Spotlight",
    "StatusIndicator",
    "Menubar",
];

/// Capture all visible application windows (not just primary monitor).
/// Filters out system UI windows and MindScope itself.
/// Returns a vec of WindowCapture with app name, window title, and image data.
pub fn capture_windows() -> Vec<WindowCapture> {
    let skip_apps: HashSet<&str> = SKIP_APPS.iter().copied().collect();
    let skip_titles: HashSet<&str> = SKIP_TITLES.iter().copied().collect();

    let windows = match xcap::Window::all() {
        Ok(w) => w,
        Err(e) => {
            log::warn!("MindScope: failed to enumerate windows: {}", e);
            return Vec::new();
        }
    };

    let mut results = Vec::new();

    for window in windows {
        let app_name = match window.app_name() {
            Ok(n) => n,
            Err(_) => continue,
        };
        let title = window.title().unwrap_or_default();

        // Skip system UI apps
        if skip_apps.contains(app_name.as_str()) {
            continue;
        }

        // Skip system UI window titles
        if skip_titles.contains(title.as_str()) {
            continue;
        }

        // Skip minimized windows
        if window.is_minimized().unwrap_or(false) {
            continue;
        }

        let w = window.width().unwrap_or(0);
        let h = window.height().unwrap_or(0);

        // Skip windows with no title and tiny/zero dimensions (menu bar extras, etc.)
        if title.is_empty() && (w < 10 || h < 10) {
            continue;
        }

        // Skip zero-dimension windows
        if w == 0 || h == 0 {
            continue;
        }

        // Capture the window image
        match window.capture_image() {
            Ok(rgba_image) => {
                results.push(WindowCapture {
                    app_name,
                    window_name: title,
                    image: image::DynamicImage::ImageRgba8(rgba_image),
                });
            }
            Err(e) => {
                log::debug!("MindScope: failed to capture window '{}' ({}): {}", title, app_name, e);
            }
        }
    }

    results
}

/// Get cached app icon path
pub fn get_app_icon_path(app_name: &str) -> Option<std::path::PathBuf> {
    let icon_path = dirs_next::home_dir().unwrap_or_default()
        .join(".mindscope").join("data").join("icons").join(format!("{}.png", app_name));
    if icon_path.exists() { Some(icon_path) } else { None }
}
