//! Real macOS privacy-permission state, replacing the old "AtomicBool that is
//! set true after the first successful capture" / "mic check always returns
//! true" placeholders.
//!
//! * Screen Recording: `CGPreflightScreenCaptureAccess` / `CGRequestScreenCaptureAccess`
//! * Microphone: `AVCaptureDevice.authorizationStatus(for: .audio)` /
//!   `requestAccess(for:completionHandler:)`

use std::time::Duration;

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

// Force-link AVFoundation so `class!(AVCaptureDevice)` resolves at runtime.
#[cfg(target_os = "macos")]
#[link(name = "AVFoundation", kind = "framework")]
extern "C" {}

/// Is Screen Recording granted to this process (no dialog)?
pub fn screen_capture_granted() -> bool {
    #[cfg(target_os = "macos")]
    unsafe { CGPreflightScreenCaptureAccess() }
    #[cfg(not(target_os = "macos"))]
    true
}

/// Ask macOS for Screen Recording. Shows the system dialog the first time and
/// registers the app in System Settings → Privacy & Security → Screen Recording.
pub fn request_screen_capture() -> bool {
    #[cfg(target_os = "macos")]
    unsafe { CGRequestScreenCaptureAccess() }
    #[cfg(not(target_os = "macos"))]
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MicStatus { NotDetermined, Restricted, Denied, Authorized }

#[cfg(target_os = "macos")]
fn av_media_type_audio() -> cocoa::base::id {
    use cocoa::foundation::NSString;
    unsafe { NSString::alloc(cocoa::base::nil).init_str("soun") }
}

/// Microphone authorization status (no dialog).
pub fn mic_status() -> MicStatus {
    #[cfg(target_os = "macos")]
    {
        use objc::{class, msg_send, sel, sel_impl};
        let status: i64 = unsafe {
            let media = av_media_type_audio();
            let s: i64 = msg_send![class!(AVCaptureDevice), authorizationStatusForMediaType: media];
            let _: () = msg_send![media, release];
            s
        };
        match status {
            3 => MicStatus::Authorized,
            2 => MicStatus::Denied,
            1 => MicStatus::Restricted,
            _ => MicStatus::NotDetermined,
        }
    }
    #[cfg(not(target_os = "macos"))]
    MicStatus::Authorized
}

pub fn mic_granted() -> bool { mic_status() == MicStatus::Authorized }

/// Trigger the microphone permission dialog (only from an explicit user action)
/// and wait up to `timeout` for the answer. Returns the resulting grant state.
pub fn request_mic(timeout: Duration) -> bool {
    match mic_status() {
        MicStatus::Authorized => return true,
        MicStatus::Denied | MicStatus::Restricted => return false,
        MicStatus::NotDetermined => {}
    }
    #[cfg(target_os = "macos")]
    {
        use objc::runtime::{BOOL, NO};
        use objc::{class, msg_send, sel, sel_impl};
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        let block = block::ConcreteBlock::new(move |granted: BOOL| {
            let _ = tx.send(granted != NO);
        });
        let block = block.copy();
        unsafe {
            let media = av_media_type_audio();
            let _: () = msg_send![class!(AVCaptureDevice), requestAccessForMediaType: media completionHandler: &*block];
            let _: () = msg_send![media, release];
        }
        rx.recv_timeout(timeout).unwrap_or_else(|_| mic_granted())
    }
    #[cfg(not(target_os = "macos"))]
    { let _ = timeout; true }
}

pub fn open_screen_recording_settings() {
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        .spawn();
}

pub fn open_microphone_settings() {
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
        .spawn();
}
