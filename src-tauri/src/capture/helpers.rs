//! Locate and run the Swift helper CLIs that `build.rs` compiles into Tauri
//! external binaries. They live next to the app executable
//! (`MindScope.app/Contents/MacOS/` in a bundle, `target/<profile>/` in dev),
//! never in `~/.mindscope/bin`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;

use super::util::LockExt;

pub const NAMES: &[&str] = &[
    "ocr_helper", "hevc_encoder", "frame_reader", "screen_info",
    "active_app", "system_audio", "is_meeting", "topmost_window",
];

static FOUND: Mutex<Option<HashMap<&'static str, PathBuf>>> = Mutex::new(None);

fn candidate_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = std::env::var_os("MINDSCOPE_HELPERS_DIR") {
        v.push(PathBuf::from(d));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.to_path_buf());
        }
    }
    // Dev fallback (cargo test, running the raw binary from elsewhere).
    v.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries"));
    v
}

/// Absolute path of a helper, or None if it was not compiled/bundled.
pub fn helper_path(name: &str) -> Option<PathBuf> {
    let key = NAMES.iter().copied().find(|n| *n == name)?;
    {
        let guard = FOUND.lock_or_recover();
        if let Some(p) = guard.as_ref().and_then(|m| m.get(key)) {
            if p.is_file() { return Some(p.clone()); }
        }
    }
    let triple = env!("MINDSCOPE_TARGET_TRIPLE");
    for dir in candidate_dirs() {
        for cand in [dir.join(name), dir.join(format!("{name}-{triple}"))] {
            if cand.is_file() {
                FOUND.lock_or_recover().get_or_insert_with(HashMap::new).insert(key, cand.clone());
                return Some(cand);
            }
        }
    }
    None
}

/// A `Command` for the helper with `MINDSCOPE_HOME` exported, or None if missing.
pub fn command(name: &str) -> Option<Command> {
    let path = helper_path(name)?;
    let mut cmd = Command::new(path);
    cmd.env("MINDSCOPE_HOME", super::paths::mindscope_home());
    cmd.stdin(Stdio::null());
    Some(cmd)
}

/// Run a helper that prints one JSON document on stdout.
pub fn run_json<T: serde::de::DeserializeOwned>(name: &str, args: &[&str]) -> Option<T> {
    let out = command(name)?.args(args).output().ok()?;
    if !out.status.success() { return None; }
    let s = String::from_utf8_lossy(&out.stdout);
    let s = s.trim();
    if s.is_empty() || s == "NONE" { return None; }
    serde_json::from_str(s).ok()
}

/// Run a helper and return trimmed stdout.
pub fn run_text(name: &str, args: &[&str]) -> Option<String> {
    let out = command(name)?.args(args).output().ok()?;
    if !out.status.success() { return None; }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Helpers that could not be found — logged once at startup.
pub fn missing() -> Vec<&'static str> {
    NAMES.iter().copied().filter(|n| helper_path(n).is_none()).collect()
}
