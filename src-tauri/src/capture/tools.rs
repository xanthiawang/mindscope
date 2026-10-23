//! Resolve external CLIs (`ffmpeg`, `claude`) from settings + PATH, including
//! user-local install dirs (`~/.local/bin`, Homebrew, bun, npm-global…).
//! Nothing is hard-coded to `/opt/homebrew/bin` any more.

use std::path::{Path, PathBuf};

fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs_next::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(p)
}

/// Extra directories searched after `$PATH`.
pub fn extra_dirs() -> Vec<PathBuf> {
    let home = dirs_next::home_dir().unwrap_or_default();
    vec![
        home.join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/local/bin"),
        home.join(".bun/bin"),
        home.join(".npm-global/bin"),
        home.join(".cargo/bin"),
        home.join(".volta/bin"),
        home.join(".claude/local"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ]
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.is_file() && std::fs::metadata(p).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// Find `name`: explicit `configured` path first, then `$PATH`, then `extra_dirs()`.
pub fn find_tool(name: &str, configured: Option<&str>) -> Option<PathBuf> {
    if let Some(c) = configured.map(str::trim).filter(|s| !s.is_empty()) {
        let p = expand_tilde(c);
        if is_executable(&p) { return Some(p); }
        if p.is_dir() {
            let q = p.join(name);
            if is_executable(&q) { return Some(q); }
        }
        log::warn!("MindScope: configured {} path {:?} is not executable, falling back to PATH", name, p);
    }
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend(extra_dirs());
    dirs.into_iter().map(|d| d.join(name)).find(|p| is_executable(p))
}

/// `PATH` value to hand to child processes (the app's PATH plus the extras).
pub fn child_path_env() -> std::ffi::OsString {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for d in extra_dirs() {
        if !dirs.contains(&d) { dirs.push(d); }
    }
    std::env::join_paths(dirs).unwrap_or_default()
}

pub fn find_ffmpeg() -> Option<PathBuf> {
    let s = super::settings::load_settings();
    find_tool("ffmpeg", s.ffmpeg_path.as_deref())
}

pub fn find_claude() -> Option<PathBuf> {
    let s = super::settings::load_settings();
    find_tool("claude", s.claude_path.as_deref())
}
