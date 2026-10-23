//! Every on-disk location MindScope uses, rooted at `$MINDSCOPE_HOME`
//! (default `~/.mindscope`). Pointing `MINDSCOPE_HOME` at a throwaway directory
//! lets the app run end-to-end without touching real data.

use std::path::PathBuf;
use std::sync::OnceLock;

static HOME: OnceLock<PathBuf> = OnceLock::new();

/// `$MINDSCOPE_HOME` or `~/.mindscope`. Also exports the resolved value into the
/// process environment so the Swift helpers (which cache icons etc.) agree.
pub fn mindscope_home() -> PathBuf {
    HOME.get_or_init(|| {
        let p = match std::env::var_os("MINDSCOPE_HOME") {
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => dirs_next::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".mindscope"),
        };
        std::env::set_var("MINDSCOPE_HOME", &p);
        p
    })
    .clone()
}

fn ensure(p: PathBuf) -> PathBuf {
    let _ = std::fs::create_dir_all(&p);
    p
}

pub fn data_dir() -> PathBuf { ensure(mindscope_home().join("data")) }
pub fn db_path() -> PathBuf { data_dir().join("mindscope.db") }
pub fn frames_root() -> PathBuf { ensure(data_dir().join("frames")) }
pub fn frames_dir(date: &str) -> PathBuf { ensure(frames_root().join(date)) }
pub fn segments_dir() -> PathBuf { ensure(data_dir().join("segments")) }
pub fn cache_dir() -> PathBuf { ensure(data_dir().join("cache")) }
pub fn audio_root() -> PathBuf { ensure(data_dir().join("audio")) }
pub fn icons_dir() -> PathBuf { ensure(data_dir().join("icons")) }
pub fn models_dir() -> PathBuf { ensure(mindscope_home().join("models")) }
pub fn vault_dir() -> PathBuf { mindscope_home().join("vault") }
pub fn synapse_dir() -> PathBuf { mindscope_home().join("synapse") }
pub fn pipes_dir() -> PathBuf { ensure(mindscope_home().join("pipes")) }
pub fn settings_path() -> PathBuf { mindscope_home().join("settings.json") }
pub fn debug_log_path() -> PathBuf { data_dir().join("debug.log") }
pub fn meeting_log_path() -> PathBuf { data_dir().join("meeting_debug.log") }
