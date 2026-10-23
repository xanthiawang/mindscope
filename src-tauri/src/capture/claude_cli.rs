//! The single place MindScope shells out to the `claude` CLI.
//!
//! Every caller (Synapse loop, Ask AI, meeting notes, daily journal/focus,
//! pipes) goes through `run()`, which gives them all:
//!   * binary resolution via settings + PATH (incl. ~/.local/bin), never a fixed path
//!   * a hard timeout (the child is killed, not left running)
//!   * `--max-turns` (when the installed CLI supports it) and `--tools ""`
//!     for tool-free background calls, so no file edits / shell happen behind
//!     the user's back and concurrent Claude Code sessions don't conflict
//!   * a daily call budget persisted in data/claude_budget.json
//!   * `--no-session-persistence` so background calls leave no session files

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::paths;
use super::settings;
use super::timeutil;
use super::tools;
use super::util::LockExt;

#[derive(Debug, Clone)]
pub struct CallOpts {
    /// Model id/alias passed with `--model`; None = CLI default.
    pub model: Option<String>,
    pub max_turns: u32,
    pub timeout: Duration,
    /// `--tools ""`: no built-in tools at all.
    pub tool_free: bool,
    /// Working directory; defaults to the vault (so CLAUDE.md there applies) or $HOME.
    pub cwd: Option<PathBuf>,
}

impl CallOpts {
    /// Cheap background summarisation: Haiku, one turn, tool-free, 2 min cap.
    pub fn background() -> Self {
        Self {
            model: Some("claude-haiku-4-5".to_string()),
            max_turns: 1,
            timeout: Duration::from_secs(120),
            tool_free: true,
            cwd: None,
        }
    }

    /// User-facing Ask AI: default model, still tool-free (context is inlined), 3 min cap.
    pub fn interactive() -> Self {
        Self {
            model: None,
            max_turns: 3,
            timeout: Duration::from_secs(180),
            tool_free: true,
            cwd: None,
        }
    }
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Budget {
    date: String,
    count: u32,
}

static BUDGET: Mutex<Option<Budget>> = Mutex::new(None);
static SUPPORTS_MAX_TURNS: OnceLock<bool> = OnceLock::new();

fn budget_path() -> PathBuf { paths::data_dir().join("claude_budget.json") }

fn load_budget() -> Budget {
    std::fs::read_to_string(budget_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Reserve one call against today's budget. `limit` 0 = unlimited.
fn consume_budget(limit: u32) -> Result<(), String> {
    let mut guard = BUDGET.lock_or_recover();
    let today = timeutil::today();
    let b = guard.get_or_insert_with(load_budget);
    if b.date != today {
        b.date = today;
        b.count = 0;
    }
    if limit > 0 && b.count >= limit {
        return Err(format!("daily Claude CLI budget of {} calls reached; try again tomorrow or raise claude_daily_budget", limit));
    }
    b.count += 1;
    if let Ok(s) = serde_json::to_string(&*b) {
        let _ = std::fs::write(budget_path(), s);
    }
    Ok(())
}

/// Calls made today (for diagnostics).
pub fn calls_today() -> u32 {
    let guard = BUDGET.lock_or_recover();
    match guard.as_ref() {
        Some(b) if b.date == timeutil::today() => b.count,
        _ => 0,
    }
}

fn supports_max_turns(claude: &PathBuf) -> bool {
    *SUPPORTS_MAX_TURNS.get_or_init(|| {
        Command::new(claude)
            .arg("--help")
            .env("PATH", tools::child_path_env())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("--max-turns"))
            .unwrap_or(false)
    })
}

/// Run one `claude -p` call. `purpose` is only used for logging.
pub fn run(purpose: &str, prompt: &str, opts: CallOpts) -> Result<String, String> {
    let s = settings::load_settings();
    let claude = tools::find_claude()
        .ok_or_else(|| "Claude CLI not found (set claude_path in settings.json or install `claude` on PATH)".to_string())?;
    consume_budget(s.claude_daily_budget)?;

    let vault = paths::vault_dir();
    let cwd = opts.cwd.clone().unwrap_or_else(|| {
        if vault.is_dir() { vault } else { dirs_next::home_dir().unwrap_or_else(|| PathBuf::from("/")) }
    });

    let mut cmd = Command::new(&claude);
    cmd.arg("-p").arg("--output-format").arg("text").arg("--no-session-persistence");
    if let Some(m) = &opts.model { cmd.arg("--model").arg(m); }
    if opts.tool_free { cmd.arg("--tools").arg(""); }
    if opts.max_turns > 0 && supports_max_turns(&claude) {
        cmd.arg("--max-turns").arg(opts.max_turns.to_string());
    }
    cmd.current_dir(&cwd)
        .env("PATH", tools::child_path_env())
        .env("MINDSCOPE_HOME", paths::mindscope_home())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let started = Instant::now();
    log::info!("MindScope claude[{}]: invoking {} (model={}, timeout={}s)",
        purpose, claude.display(), opts.model.as_deref().unwrap_or("default"), opts.timeout.as_secs());

    let mut child = cmd.spawn().map_err(|e| format!("failed to run claude: {}", e))?;

    // Prompt via stdin: no ARG_MAX limit and no prompt text in `ps` output.
    if let Some(mut stdin) = child.stdin.take() {
        let prompt = prompt.to_string();
        std::thread::spawn(move || { let _ = stdin.write_all(prompt.as_bytes()); });
    }
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut s) = stdout { let _ = s.read_to_end(&mut buf); }
        buf
    });
    let err_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut s) = stderr { let _ = s.read_to_end(&mut buf); }
        buf
    });

    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {
                if started.elapsed() >= opts.timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("claude wait failed: {}", e)),
        }
    };
    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();

    let Some(status) = status else {
        return Err(format!("claude timed out after {}s", opts.timeout.as_secs()));
    };
    let elapsed = started.elapsed().as_secs_f32();
    if !status.success() {
        let err = String::from_utf8_lossy(&stderr);
        let short: String = err.chars().take(300).collect();
        log::warn!("MindScope claude[{}]: exit {} after {:.1}s: {}", purpose, status, elapsed, short.trim());
        return Err(format!("Claude error: {}", short.trim()));
    }
    let text = String::from_utf8_lossy(&stdout).trim().to_string();
    log::info!("MindScope claude[{}]: ok in {:.1}s ({} chars)", purpose, elapsed, text.chars().count());
    if text.is_empty() { Err("Empty response from Claude".into()) } else { Ok(text) }
}

/// Background summarisation call (Haiku, tool-free, 1 turn).
pub fn run_background(purpose: &str, prompt: &str) -> Result<String, String> {
    run(purpose, prompt, CallOpts::background())
}

/// Is a `claude` binary available at all?
pub fn available() -> bool { tools::find_claude().is_some() }
