//! Window manipulation for MindScope
//! Enables click-through, window level control, and collection behavior
//! Platform-specific implementations for macOS and Windows

#[cfg(target_os = "macos")]
mod macos {
    use cocoa::appkit::NSWindowCollectionBehavior;
    use cocoa::base::{id, YES, NO};
    use objc::msg_send;
    use objc::sel;
    use objc::sel_impl;

    pub const NS_FLOATING_WINDOW_LEVEL: i64 = 3;
    pub const NS_SCREEN_SAVER_WINDOW_LEVEL: i64 = 1000;

    pub fn set_ignore_mouse_events(ns_window: id, ignore: bool) {
        unsafe {
            let _: () = msg_send![ns_window, setIgnoresMouseEvents: if ignore { YES } else { NO }];
        }
    }

    pub fn set_window_level(ns_window: id, level: i64) {
        unsafe {
            let _: () = msg_send![ns_window, setLevel: level];
        }
    }

    pub fn set_overlay_collection_behavior(ns_window: id) {
        unsafe {
            let behavior = NSWindowCollectionBehavior::NSWindowCollectionBehaviorCanJoinAllSpaces
                | NSWindowCollectionBehavior::NSWindowCollectionBehaviorFullScreenAuxiliary
                | NSWindowCollectionBehavior::NSWindowCollectionBehaviorStationary;
            let _: () = msg_send![ns_window, setCollectionBehavior: behavior];
        }
    }

    pub fn get_ns_window(window: &tauri::WebviewWindow) -> Option<id> {
        #[allow(deprecated)]
        window.ns_window().ok().map(|ptr| ptr as id)
    }
}

#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicI32, Ordering};

/// Physical pixels from screen bottom that should receive mouse events.
/// Bar-only default = 200px; bumped to 700 while any panel is open.
#[cfg(target_os = "windows")]
static INTERACTIVE_ZONE_PX: AtomicI32 = AtomicI32::new(200);

/// Called by the Tauri command `set_interactive_zone` when panels open/close.
#[cfg(target_os = "windows")]
pub fn set_interactive_zone(px: i32) {
    INTERACTIVE_ZONE_PX.store(px, Ordering::Relaxed);
}
#[cfg(not(target_os = "windows"))]
pub fn set_interactive_zone(_px: i32) {}

#[cfg(target_os = "windows")]
mod windows_impl {
    use windows::Win32::Foundation::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    pub const HWND_TOPMOST: isize = -1;
    pub const HWND_NOTOPMOST: isize = -2;

    pub fn set_window_topmost(hwnd: HWND, topmost: bool) {
        unsafe {
            let insert_after = if topmost {
                HWND(HWND_TOPMOST as *mut _)
            } else {
                HWND(HWND_NOTOPMOST as *mut _)
            };
            let _ = SetWindowPos(
                hwnd,
                insert_after,
                0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    pub fn set_click_through(hwnd: HWND, enabled: bool) {
        unsafe {
            let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            let new_style = if enabled {
                ex_style | WS_EX_TRANSPARENT.0 | WS_EX_LAYERED.0
            } else {
                ex_style & !(WS_EX_TRANSPARENT.0)
            };
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_style as isize);
        }
    }

    pub fn set_tool_window(hwnd: HWND) {
        unsafe {
            let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            let new_style = ex_style | WS_EX_TOOLWINDOW.0;
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_style as isize);
        }
    }

    pub fn get_hwnd(window: &tauri::WebviewWindow) -> Option<HWND> {
        #[allow(deprecated)]
        window.hwnd().ok().map(|h| HWND(h.0 as *mut _))
    }

    /// Poll cursor position every 50ms and toggle click-through accordingly.
    ///
    /// On Windows there is no per-region transparency like macOS — WS_EX_TRANSPARENT
    /// applies to the whole window. The JS onMouseEnter/onMouseLeave handlers that
    /// work on macOS never fire when the window is in click-through mode, so we need
    /// a background thread to manage the state based on raw cursor position.
    ///
    /// Strategy: the window covers the full screen. The bar sits in the bottom ~130px
    /// and panels extend up to ~560px above it. We use a fixed 700px interactive zone
    /// from the bottom, which covers bar + all panels without blocking the upper screen.
    pub fn start_poll(window: tauri::WebviewWindow) {
        let hwnd_raw = match get_hwnd(&window) {
            Some(h) => h.0 as usize,
            None => return,
        };

        std::thread::spawn(move || {
            let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);
            let mut last_ct = true;
            // Start transparent so the rest of the desktop is immediately usable.
            set_click_through(hwnd, true);

            loop {
                std::thread::sleep(std::time::Duration::from_millis(50));

                let visible = window.is_visible().unwrap_or(false);
                if !visible {
                    if !last_ct {
                        set_click_through(hwnd, true);
                        last_ct = true;
                    }
                    continue;
                }

                let in_interactive = unsafe {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    let screen_h = GetSystemMetrics(SM_CYSCREEN);
                    let zone = super::INTERACTIVE_ZONE_PX.load(std::sync::atomic::Ordering::Relaxed);
                    pt.y >= screen_h - zone
                };

                let should_ct = !in_interactive;
                if should_ct != last_ct {
                    set_click_through(hwnd, should_ct);
                    last_ct = should_ct;
                }
            }
        });
    }
}

/// Configure window for bar mode: floating level, collection behavior, interactable
pub fn configure_bar_mode(window: &tauri::WebviewWindow) {
    #[cfg(target_os = "macos")]
    {
        if let Some(ns_window) = macos::get_ns_window(window) {
            macos::set_window_level(ns_window, macos::NS_FLOATING_WINDOW_LEVEL);
            macos::set_ignore_mouse_events(ns_window, false);
            macos::set_overlay_collection_behavior(ns_window);
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(hwnd) = windows_impl::get_hwnd(window) {
            windows_impl::set_window_topmost(hwnd, true);
            // Start click-through ON — the polling thread (started in lib.rs setup)
            // disables it when the cursor enters the bottom half of the screen.
            windows_impl::set_click_through(hwnd, true);
            windows_impl::set_tool_window(hwnd);
        }
    }
}

/// Start the Windows click-through polling thread.
/// No-op on macOS (JS onMouseEnter/onMouseLeave handles it there).
#[cfg(target_os = "windows")]
pub fn start_clickthrough_poll(window: tauri::WebviewWindow) {
    windows_impl::start_poll(window);
}

#[cfg(not(target_os = "windows"))]
pub fn start_clickthrough_poll(_window: tauri::WebviewWindow) {}

/// Configure window for rewind/fullscreen mode: high level, interactable
pub fn configure_fullscreen_mode(window: &tauri::WebviewWindow) {
    #[cfg(target_os = "macos")]
    {
        if let Some(ns_window) = macos::get_ns_window(window) {
            macos::set_window_level(ns_window, macos::NS_SCREEN_SAVER_WINDOW_LEVEL);
            macos::set_ignore_mouse_events(ns_window, false);
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(hwnd) = windows_impl::get_hwnd(window) {
            windows_impl::set_window_topmost(hwnd, true);
            windows_impl::set_click_through(hwnd, false);
        }
    }
}

/// Set click-through state
pub fn set_clickthrough(window: &tauri::WebviewWindow, enabled: bool) {
    #[cfg(target_os = "macos")]
    {
        if let Some(ns_window) = macos::get_ns_window(window) {
            macos::set_ignore_mouse_events(ns_window, enabled);
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(hwnd) = windows_impl::get_hwnd(window) {
            windows_impl::set_click_through(hwnd, enabled);
        }
    }
}
