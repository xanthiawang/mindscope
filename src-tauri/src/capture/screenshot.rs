//! Screen capture + "what is the user looking at" queries.
//!
//! * `topmost_window()` — bundled `topmost_window` helper (CGWindowList z-order),
//!   which also tells us the display the window is on.
//! * `capture_display()` — xcap capture of ONE display, downscaled to the
//!   configured height. The old code captured every monitor at full
//!   resolution each cycle and picked the "busiest" one.

use std::path::PathBuf;

use image::DynamicImage;

use super::helpers;
use super::paths;

/// Frontmost real application window, as reported by the `topmost_window` helper.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct WindowInfo {
    pub app: String,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub bundle: String,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub wid: u32,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default)]
    pub w: i32,
    #[serde(default)]
    pub h: i32,
    /// CGDirectDisplayID of the display containing the window (0 = unknown).
    #[serde(default)]
    pub display: u32,
}

pub fn topmost_window() -> Option<WindowInfo> {
    let w: WindowInfo = helpers::run_json("topmost_window", &[])?;
    if w.app.is_empty() || w.app.eq_ignore_ascii_case("mindscope") { return None; }
    Some(w)
}

/// Returns (app_name, window_title, bundle_id). Falls back to the `active_app`
/// helper (NSWorkspace frontmost + AX title) when z-order lookup fails.
pub fn get_active_window_info() -> (String, String, String) {
    if let Some(w) = topmost_window() {
        return (w.app, w.title, w.bundle);
    }
    if let Some(out) = helpers::run_text("active_app", &[]) {
        let parts: Vec<&str> = out.splitn(4, '|').collect();
        if parts.len() >= 2 && !parts[0].is_empty() && parts[0] != "Unknown" {
            return (
                parts[0].to_string(),
                parts[1].to_string(),
                parts.get(2).copied().unwrap_or("").to_string(),
            );
        }
    }
    ("Unknown".to_string(), String::new(), String::new())
}

/// Does this window title look like a private / incognito browser window?
pub fn is_private_window(title: &str) -> bool {
    let t = title.to_lowercase();
    ["incognito", "private browsing", "inprivate", "private window", "— private", "- private",
     "无痕", "隐私浏览", "私密浏览", "プライベート"]
        .iter()
        .any(|m| t.contains(m))
}

/// Capture one display (by CGDirectDisplayID; falls back to the primary display),
/// downscaled so its height is at most `max_height` pixels. Returns an RGB image.
pub fn capture_display(display_id: Option<u32>, max_height: u32) -> Option<DynamicImage> {
    let monitors = xcap::Monitor::all().ok()?;
    if monitors.is_empty() { return None; }
    let monitor = display_id
        .and_then(|id| monitors.iter().find(|m| m.id().ok() == Some(id)))
        .or_else(|| monitors.iter().find(|m| m.is_primary().unwrap_or(false)))
        .or_else(|| monitors.first())?;
    let rgba = monitor.capture_image().ok()?;
    let (w, h) = (rgba.width(), rgba.height());
    if w == 0 || h == 0 { return None; }
    let img = DynamicImage::ImageRgba8(rgba);
    let img = if max_height > 0 && h > max_height {
        let new_w = ((w as u64 * max_height as u64) / h as u64).max(1) as u32;
        // Box filter: a few ms for a 5K frame, vs. ~100 ms for a Triangle resize.
        img.thumbnail_exact(new_w, max_height)
    } else {
        img
    };
    // Drop alpha: smaller in memory, and JPEG has no alpha anyway.
    Some(DynamicImage::ImageRgb8(img.to_rgb8()))
}

/// Encode as JPEG. `quality` is 1-100.
pub fn encode_jpeg(img: &DynamicImage, quality: u8) -> Option<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality.clamp(1, 100));
    enc.encode_image(img).ok()?;
    Some(buf.into_inner())
}

/// Get cached app icon path
pub fn get_app_icon_path(app_name: &str) -> Option<PathBuf> {
    let safe = app_name.replace('/', "_");
    let icon_path = paths::icons_dir().join(format!("{}.png", safe));
    if icon_path.exists() { Some(icon_path) } else { None }
}
