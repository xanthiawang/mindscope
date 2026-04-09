//! macOS NSWindow manipulation via Objective-C FFI
//! Enables click-through, window level control, and collection behavior

#[cfg(target_os = "macos")]
mod macos {
    use cocoa::appkit::NSWindowCollectionBehavior;
    use cocoa::base::{id, YES, NO};
    use objc::msg_send;
    use objc::sel;
    use objc::sel_impl;

    /// Window level constants
    pub const NS_FLOATING_WINDOW_LEVEL: i64 = 3;
    pub const NS_SCREEN_SAVER_WINDOW_LEVEL: i64 = 1000;

    pub fn set_ignore_mouse_events(ns_window: id, ignore: bool) {
        unsafe {
            let _: () = msg_send![ns_window, setIgnoresMouseEvents: if ignore { YES } else { NO }];
        }
    }

    /// Enable mouse forwarding — window ignores mouse but forwards events to webview
    /// WebView uses CSS `pointer-events: none` on transparent areas to pass through
    pub fn set_ignore_mouse_with_forward(ns_window: id, ignore: bool) {
        unsafe {
            // ignoresMouseEvents with forwarding: the window ignores but
            // the content view still gets events for CSS hit-testing
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
}

/// Configure window for rewind/fullscreen mode: screenSaver level, interactable
pub fn configure_fullscreen_mode(window: &tauri::WebviewWindow) {
    #[cfg(target_os = "macos")]
    {
        if let Some(ns_window) = macos::get_ns_window(window) {
            macos::set_window_level(ns_window, macos::NS_SCREEN_SAVER_WINDOW_LEVEL);
            macos::set_ignore_mouse_events(ns_window, false);
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
}
