//! Small shared utilities.

use std::sync::{Mutex, MutexGuard};

/// Poison-tolerant locking for process-wide statics: a panic on another thread
/// must not take the recorder, DB or encoder down with it.
pub trait LockExt<T> {
    fn lock_or_recover(&self) -> MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    fn lock_or_recover(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Append one line to a log file, creating it if needed. Errors are ignored.
pub fn append_line(path: &std::path::Path, line: &str) {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
        if !line.ends_with('\n') { let _ = f.write_all(b"\n"); }
    }
}

/// Rotate `path` to `path.1` once it exceeds `max_bytes` (one generation kept).
pub fn rotate_log(path: &std::path::Path, max_bytes: u64) {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > max_bytes {
            let mut rotated = path.as_os_str().to_owned();
            rotated.push(".1");
            let _ = std::fs::rename(path, std::path::PathBuf::from(rotated));
        }
    }
}
