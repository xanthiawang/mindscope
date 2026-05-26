# Windows Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every MindScope feature (capture, OCR, audio, meeting detection, Synapse, bar UI) work on Windows without touching the macOS code path.

**Architecture:** Add `#[cfg(target_os = "windows")]` blocks to the three files where macOS-specific shell-outs remain (recorder.rs, vault_sync.rs), add two new public functions to platform/windows.rs for meeting detection, add taskbar height detection to panel.rs, and wire a `get_taskbar_height` Tauri command into the frontend. All macOS code is untouched.

**Tech Stack:** Rust (windows crate v0.58, whisper-rs already present), Tauri 2, React/TypeScript

---

## File Map

| File | What changes |
|---|---|
| `src-tauri/Cargo.toml` | Add `Win32_UI_Shell` to windows features |
| `src-tauri/src/capture/platform/windows.rs` | Add `is_meeting_app_running()`, `get_meeting_app_name()`, `get_taskbar_height()` |
| `src-tauri/src/capture/recorder.rs` | Windows cfg guards on Swift-helper calls; fix `pkill` → `kill_audio_processes()` |
| `src-tauri/src/capture/vault_sync.rs` | Fix hardcoded `/opt/homebrew/bin/claude`; normalize `\r\n` → `\n` |
| `src-tauri/src/capture/panel.rs` | Export `get_taskbar_height()` using `SHAppBarMessage` |
| `src-tauri/src/lib.rs` | Register `get_taskbar_height` Tauri command |
| `src/lib/commands.ts` | Add `getTaskbarHeight()` invoke wrapper |
| `src/App.tsx` | Replace hardcoded `bottom: 58` with state from `getTaskbarHeight` |

---

## Task 1: Add Win32_UI_Shell to Cargo.toml

**Files:**
- Modify: `src-tauri/Cargo.toml` (lines 49–63, the `[target.'cfg(target_os = "windows")'.dependencies.windows]` block)

- [ ] **Step 1: Add the missing feature**

Open `src-tauri/Cargo.toml`. The current windows block is:

```toml
[target.'cfg(target_os = "windows")'.dependencies]
tauri = { version = "2", features = ["macos-private-api", "tray-icon"] }
whisper-rs = { version = "0.16" }
windows = { version = "0.58", features = [
    "Win32_Foundation",
    "Win32_UI_WindowsAndMessaging",
    "Win32_Graphics_Gdi",
    "Win32_System_Threading",
    "Win32_System_ProcessStatus",
    "Win32_Security",
    "Win32_System_LibraryLoader",
    "Win32_Globalization",
    "Foundation_Collections",
    "Media_Ocr",
    "Graphics_Imaging",
    "Storage",
    "Storage_Streams",
] }
```

Replace it with (add `"Win32_UI_Shell"` to the features list):

```toml
[target.'cfg(target_os = "windows")'.dependencies]
tauri = { version = "2", features = ["macos-private-api", "tray-icon"] }
whisper-rs = { version = "0.16" }
windows = { version = "0.58", features = [
    "Win32_Foundation",
    "Win32_UI_WindowsAndMessaging",
    "Win32_Graphics_Gdi",
    "Win32_System_Threading",
    "Win32_System_ProcessStatus",
    "Win32_Security",
    "Win32_System_LibraryLoader",
    "Win32_Globalization",
    "Win32_UI_Shell",
    "Foundation_Collections",
    "Media_Ocr",
    "Graphics_Imaging",
    "Storage",
    "Storage_Streams",
] }
```

- [ ] **Step 2: Verify it compiles**

```powershell
cd src-tauri
cargo check --target x86_64-pc-windows-msvc 2>&1 | Select-String "error"
```

Expected: no `error` lines (warnings are fine).

- [ ] **Step 3: Commit**

```powershell
git add src-tauri/Cargo.toml
git commit -m "chore: add Win32_UI_Shell to windows crate features"
```

---

## Task 2: Add meeting detection and taskbar height to windows.rs

**Files:**
- Modify: `src-tauri/src/capture/platform/windows.rs`

The goal is three new public functions:
- `is_meeting_app_running() -> bool` — returns true if a known meeting app is running
- `get_meeting_app_name() -> Option<String>` — returns the name of the first detected meeting app
- `get_taskbar_height() -> u32` — returns taskbar height + 8px gap using `SHAppBarMessage`

Plus a private helper `known_meeting_process_name(process: &str) -> Option<&'static str>` that is unit-testable independently of the shell-out.

- [ ] **Step 1: Write the failing unit tests**

Add this block at the bottom of `src-tauri/src/capture/platform/windows.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::known_meeting_process_name;

    #[test]
    fn test_known_meeting_process_zoom() {
        assert_eq!(known_meeting_process_name("zoom.exe"), Some("Zoom"));
        assert_eq!(known_meeting_process_name("Zoom.exe"), Some("Zoom"));
    }

    #[test]
    fn test_known_meeting_process_teams() {
        assert_eq!(known_meeting_process_name("Teams.exe"), Some("Microsoft Teams"));
        assert_eq!(known_meeting_process_name("ms-teams.exe"), Some("Microsoft Teams"));
    }

    #[test]
    fn test_known_meeting_process_others() {
        assert_eq!(known_meeting_process_name("lark.exe"), Some("Lark"));
        assert_eq!(known_meeting_process_name("DingTalk.exe"), Some("DingTalk"));
        assert_eq!(known_meeting_process_name("wemeet.exe"), Some("WeMeet"));
        assert_eq!(known_meeting_process_name("webex.exe"), Some("Webex"));
        assert_eq!(known_meeting_process_name("discord.exe"), Some("Discord"));
    }

    #[test]
    fn test_known_meeting_process_unknown() {
        assert_eq!(known_meeting_process_name("notepad.exe"), None);
        assert_eq!(known_meeting_process_name("chrome.exe"), None);
        assert_eq!(known_meeting_process_name(""), None);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_taskbar_height_reasonable() {
        let h = super::get_taskbar_height();
        // Standard taskbar is 32–48px; with 8px gap that's 40–56
        assert!(h >= 32, "taskbar height too small: {}", h);
        assert!(h <= 200, "taskbar height unreasonably large: {}", h);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```powershell
cargo test -p mindscope known_meeting_process 2>&1 | tail -20
```

Expected: `error[E0425]: cannot find function 'known_meeting_process_name'`

- [ ] **Step 3: Add the three functions to windows.rs**

Append the following to `src-tauri/src/capture/platform/windows.rs`, before the `#[cfg(test)]` block:

```rust
/// Returns the canonical display name for a known meeting process, or None.
/// Process name comparison is case-insensitive.
fn known_meeting_process_name(process: &str) -> Option<&'static str> {
    let p = process.to_lowercase();
    if p.contains("zoom") { return Some("Zoom"); }
    if p.contains("teams") || p.contains("ms-teams") { return Some("Microsoft Teams"); }
    if p.contains("lark") { return Some("Lark"); }
    if p.contains("dingtalk") { return Some("DingTalk"); }
    if p.contains("wemeet") { return Some("WeMeet"); }
    if p.contains("tencentmeeting") { return Some("Tencent Meeting"); }
    if p.contains("webex") { return Some("Webex"); }
    if p.contains("discord") { return Some("Discord"); }
    None
}

/// Returns the name of the active meeting app (e.g. "Zoom"), or None.
/// Checks running processes via tasklist; also checks Google Meet via Chrome window title.
pub fn get_meeting_app_name() -> Option<String> {
    let output = Command::new("tasklist")
        .args(["/FO", "CSV", "/NH"])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut chrome_running = false;

    for line in stdout.lines() {
        // CSV row: "process.exe","pid","session","session#","mem"
        let process = line.split(',').next()?.trim_matches('"');
        if let Some(name) = known_meeting_process_name(process) {
            return Some(name.to_string());
        }
        if process.to_lowercase().contains("chrome") {
            chrome_running = true;
        }
    }

    // Google Meet runs inside Chrome — detect via foreground window title
    if chrome_running {
        let (_, title, _, _) = get_active_window_info();
        let t = title.to_lowercase();
        if t.contains("meet") && (t.contains("google") || t.contains("meet.google")) {
            return Some("Google Meet".to_string());
        }
    }

    None
}

/// Returns true if any known meeting app is currently running.
pub fn is_meeting_app_running() -> bool {
    get_meeting_app_name().is_some()
}

/// Returns the taskbar height in physical pixels plus an 8px gap.
/// Uses SHAppBarMessage(ABM_GETTASKBARPOS). Returns 48 (safe default) on failure.
pub fn get_taskbar_height() -> u32 {
    use windows::Win32::UI::Shell::{SHAppBarMessage, APPBARDATA, ABM_GETTASKBARPOS};
    use windows::Win32::Foundation::RECT;

    unsafe {
        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: windows::Win32::Foundation::HWND::default(),
            uCallbackMessage: 0,
            uEdge: 0,
            rc: RECT::default(),
            lParam: windows::Win32::Foundation::LPARAM(0),
        };
        SHAppBarMessage(ABM_GETTASKBARPOS, &mut abd);
        let rect = abd.rc;
        let height = (rect.bottom - rect.top).max(0) as u32;
        if height == 0 { 48 } else { height + 8 }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```powershell
cargo test -p mindscope known_meeting_process 2>&1 | tail -20
```

Expected: `test result: ok. 4 passed; 0 failed`

- [ ] **Step 5: Run full check to catch any compile errors**

```powershell
cargo check --target x86_64-pc-windows-msvc 2>&1 | Select-String "error"
```

Expected: no `error` lines.

- [ ] **Step 6: Commit**

```powershell
git add src-tauri/src/capture/platform/windows.rs
git commit -m "feat(windows): add meeting detection and taskbar height functions"
```

---

## Task 3: Wire Windows meeting detection in recorder.rs

**Files:**
- Modify: `src-tauri/src/capture/recorder.rs`

Two places need Windows-safe variants:
1. `is_meeting_active()` — currently calls a Swift binary that doesn't exist on Windows
2. `detect_meeting_app_name()` — same Swift binary
3. The `pkill` call that kills ffmpeg on macOS — must use `platform::kill_audio_processes()` on Windows
4. `is_meeting_running()` — dead code that uses `osascript`; guard it under `#[cfg(target_os = "macos")]` to suppress Windows warnings

- [ ] **Step 1: Add Windows cfg block to is_meeting_active()**

The current function starts at approximately line 442. Find this exact function body:

```rust
fn is_meeting_active() -> (bool, String) {
    let helper = dirs_next::home_dir().unwrap_or_default()
        .join(".mindscope").join("bin").join("is_meeting");
    if !helper.exists() { return (false, String::new()); }

    if let Ok(output) = std::process::Command::new(helper.to_str().unwrap_or(""))
        .output()
    {
        let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if result.starts_with("MEETING|") {
            let parts: Vec<&str> = result.splitn(3, '|').collect();
            let app_name = parts.get(1).unwrap_or(&"Meeting").to_string();
            return (true, app_name);
        }
    }
    (false, String::new())
}
```

Replace with:

```rust
fn is_meeting_active() -> (bool, String) {
    #[cfg(target_os = "windows")]
    {
        let name = super::platform::get_meeting_app_name();
        return (name.is_some(), name.unwrap_or_default());
    }

    #[cfg(not(target_os = "windows"))]
    {
        let helper = dirs_next::home_dir().unwrap_or_default()
            .join(".mindscope").join("bin").join("is_meeting");
        if !helper.exists() { return (false, String::new()); }

        if let Ok(output) = std::process::Command::new(helper.to_str().unwrap_or(""))
            .output()
        {
            let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if result.starts_with("MEETING|") {
                let parts: Vec<&str> = result.splitn(3, '|').collect();
                let app_name = parts.get(1).unwrap_or(&"Meeting").to_string();
                return (true, app_name);
            }
        }
        (false, String::new())
    }
}
```

- [ ] **Step 2: Add Windows cfg block to detect_meeting_app_name()**

Find:

```rust
pub fn detect_meeting_app_name() -> Option<String> {
    let helper = dirs_next::home_dir().unwrap_or_default()
        .join(".mindscope").join("bin").join("is_meeting");
    if !helper.exists() { return None; }
    let output = std::process::Command::new(helper.to_str().unwrap_or("")).output().ok()?;
    let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !result.starts_with("MEETING|") { return None; }
    let parts: Vec<&str> = result.splitn(3, '|').collect();
    parts.get(1).map(|s| s.to_string())
}
```

Replace with:

```rust
pub fn detect_meeting_app_name() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        return super::platform::get_meeting_app_name();
    }

    #[cfg(not(target_os = "windows"))]
    {
        let helper = dirs_next::home_dir().unwrap_or_default()
            .join(".mindscope").join("bin").join("is_meeting");
        if !helper.exists() { return None; }
        let output = std::process::Command::new(helper.to_str().unwrap_or("")).output().ok()?;
        let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !result.starts_with("MEETING|") { return None; }
        let parts: Vec<&str> = result.splitn(3, '|').collect();
        parts.get(1).map(|s| s.to_string())
    }
}
```

- [ ] **Step 3: Fix the pkill call**

Find this exact line (around line 245 in the suppression block):

```rust
let _ = std::process::Command::new("pkill").args(["-f", "ffmpeg.*avfoundation"]).status();
```

Replace with:

```rust
#[cfg(target_os = "windows")]
super::platform::kill_audio_processes();
#[cfg(not(target_os = "windows"))]
let _ = std::process::Command::new("pkill").args(["-f", "ffmpeg.*avfoundation"]).status();
```

- [ ] **Step 4: Guard the dead osascript function**

Find this function signature:

```rust
fn is_meeting_running(_meeting_apps: &[&str]) -> bool {
```

Add `#[cfg(target_os = "macos")]` directly above it:

```rust
#[cfg(target_os = "macos")]
fn is_meeting_running(_meeting_apps: &[&str]) -> bool {
```

- [ ] **Step 5: Compile check**

```powershell
cargo check --target x86_64-pc-windows-msvc 2>&1 | Select-String "error"
```

Expected: no `error` lines.

- [ ] **Step 6: Commit**

```powershell
git add src-tauri/src/capture/recorder.rs
git commit -m "feat(windows): wire Windows meeting detection; fix pkill on Windows"
```

---

## Task 4: Fix call_claude() and line endings in vault_sync.rs

**Files:**
- Modify: `src-tauri/src/capture/vault_sync.rs`

Two fixes:
1. `call_claude()` hardcodes `/opt/homebrew/bin/claude` — replace with `platform::find_claude_cli()`
2. Claude CLI on Windows writes `\r\n` line endings — normalize before returning

- [ ] **Step 1: Write the failing test for line ending normalization**

Add this block at the bottom of `src-tauri/src/capture/vault_sync.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::normalize_line_endings;

    #[test]
    fn test_normalize_crlf() {
        assert_eq!(normalize_line_endings("a\r\nb\r\nc"), "a\nb\nc");
    }

    #[test]
    fn test_normalize_lf_unchanged() {
        assert_eq!(normalize_line_endings("a\nb\nc"), "a\nb\nc");
    }

    #[test]
    fn test_normalize_mixed() {
        assert_eq!(normalize_line_endings("a\r\nb\nc\r\n"), "a\nb\nc\n");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```powershell
cargo test -p mindscope normalize_line_endings 2>&1 | tail -10
```

Expected: `error[E0425]: cannot find function 'normalize_line_endings'`

- [ ] **Step 3: Add the normalize helper above call_claude()**

Find this line in vault_sync.rs:

```rust
fn call_claude(prompt: &str) -> Option<String> {
```

Insert immediately before it:

```rust
/// Strip carriage returns so vault files always use LF line endings.
/// Claude CLI on Windows may output CRLF; this prevents corruption on re-read.
fn normalize_line_endings(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

```

- [ ] **Step 4: Replace the call_claude() implementation**

Find and replace the entire `call_claude` function body:

```rust
fn call_claude(prompt: &str) -> Option<String> {
    let vault = dirs_next::home_dir()?.join(".mindscope").join("vault");
    let cwd = if vault.exists() {
        vault
    } else {
        dirs_next::home_dir()?
    };
    let output = std::process::Command::new("/opt/homebrew/bin/claude")
        .args(["-p", prompt, "--model", "claude-haiku-4-5"])
        .current_dir(&cwd)
        .env("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin")
        .output()
        .ok()?;
    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !text.is_empty() {
            Some(text)
        } else {
            None
        }
    } else {
        None
    }
}
```

Replace with:

```rust
fn call_claude(prompt: &str) -> Option<String> {
    let vault = dirs_next::home_dir()?.join(".mindscope").join("vault");
    let cwd = if vault.exists() { vault } else { dirs_next::home_dir()? };

    let claude_path = super::platform::find_claude_cli()
        .unwrap_or_else(|| "claude".to_string());

    let output = std::process::Command::new(&claude_path)
        .args(["-p", prompt, "--model", "claude-haiku-4-5"])
        .current_dir(&cwd)
        .output()
        .ok()?;

    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if text.is_empty() { None } else { Some(normalize_line_endings(&text)) }
    } else {
        None
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

```powershell
cargo test -p mindscope normalize_line_endings 2>&1 | tail -10
```

Expected: `test result: ok. 3 passed; 0 failed`

- [ ] **Step 6: Compile check**

```powershell
cargo check --target x86_64-pc-windows-msvc 2>&1 | Select-String "error"
```

Expected: no `error` lines.

- [ ] **Step 7: Commit**

```powershell
git add src-tauri/src/capture/vault_sync.rs
git commit -m "fix(windows): use platform find_claude_cli; normalize CRLF in vault writes"
```

---

## Task 5: Export get_taskbar_height from panel.rs and register Tauri command

**Files:**
- Modify: `src-tauri/src/capture/panel.rs`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Add pub get_taskbar_height() to panel.rs**

Find the line in `src-tauri/src/capture/panel.rs`:

```rust
#[cfg(not(target_os = "windows"))]
pub fn set_interactive_zone(_px: i32) {}
```

Add the following immediately after it (after the blank line that follows):

```rust
/// Returns the Windows taskbar height plus an 8px gap, for bar positioning.
/// On non-Windows platforms returns 58 (the macOS Dock safe area).
#[cfg(target_os = "windows")]
pub fn get_taskbar_height() -> u32 {
    super::platform::get_taskbar_height()
}

#[cfg(not(target_os = "windows"))]
pub fn get_taskbar_height() -> u32 {
    58
}
```

- [ ] **Step 2: Add the Tauri command in lib.rs**

Find this block near the top of `src-tauri/src/lib.rs` (after the `set_clickthrough` command):

```rust
#[tauri::command]
fn set_clickthrough(app: tauri::AppHandle, enabled: bool) {
    if let Some(window) = app.get_webview_window("main") {
        panel::set_clickthrough(&window, enabled);
    }
}
```

Add immediately after it:

```rust
/// Returns the physical height (px) the bar should sit above screen bottom.
/// On Windows: taskbar height + 8px gap. On macOS: 58 (Dock safe area).
#[tauri::command]
fn get_taskbar_height() -> u32 {
    panel::get_taskbar_height()
}
```

- [ ] **Step 3: Register the command in the invoke_handler**

Find the `invoke_handler` call in `lib.rs`. It will look like:

```rust
.invoke_handler(tauri::generate_handler![
    check_permission,
    ...
    set_clickthrough,
    set_interactive_zone,
    ...
])
```

Add `get_taskbar_height` to the list (position doesn't matter):

```rust
    set_interactive_zone,
    get_taskbar_height,
```

- [ ] **Step 4: Compile check**

```powershell
cargo check --target x86_64-pc-windows-msvc 2>&1 | Select-String "error"
```

Expected: no `error` lines.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/capture/panel.rs src-tauri/src/lib.rs
git commit -m "feat(windows): expose get_taskbar_height Tauri command"
```

---

## Task 6: Frontend dynamic bar bottom position

**Files:**
- Modify: `src/lib/commands.ts`
- Modify: `src/App.tsx`

- [ ] **Step 1: Add getTaskbarHeight to commands.ts**

Open `src/lib/commands.ts`. Find the `setInteractiveZone` function:

```typescript
export async function setInteractiveZone(px: number): Promise<void> {
  return invoke("set_interactive_zone", { px });
}
```

Add immediately after it:

```typescript
export async function getTaskbarHeight(): Promise<number> {
  return invoke("get_taskbar_height");
}
```

- [ ] **Step 2: Import getTaskbarHeight in App.tsx**

Find the existing import line in `src/App.tsx`:

```typescript
import { checkPermission, openPermissionSettings, startRecording, isRecording, getTimeline, getDailyBrief, hideWindow, setClickthrough, setInteractiveZone } from "./lib/commands";
```

Replace with:

```typescript
import { checkPermission, openPermissionSettings, startRecording, isRecording, getTimeline, getDailyBrief, hideWindow, setClickthrough, setInteractiveZone, getTaskbarHeight } from "./lib/commands";
```

- [ ] **Step 3: Add barBottom state in App.tsx**

Find the state declarations near the top of the `App` function. They start around:

```typescript
const [mode, setMode] = useState<Mode>("bar");
```

Add this line anywhere in the state block:

```typescript
const [barBottom, setBarBottom] = useState(58);
```

- [ ] **Step 4: Load taskbar height on mount**

Find this `useEffect` in App.tsx (the permission check effect):

```typescript
  useEffect(() => {
    const check = () => checkPermission().then(setHasPermission).catch(() => {});
    check();
    const t = setInterval(check, 5000);
    return () => clearInterval(t);
  }, []);
```

Add a new `useEffect` immediately after it:

```typescript
  useEffect(() => {
    getTaskbarHeight().then(setBarBottom).catch(() => {});
  }, []);
```

- [ ] **Step 5: Use barBottom in the bar's style**

Find the bar container `div` (the white glass bar). Its style contains:

```typescript
      style={{
        position: "fixed", bottom: 58, left: 0, right: 0, height: 70,
```

Replace `bottom: 58` with `bottom: barBottom`:

```typescript
      style={{
        position: "fixed", bottom: barBottom, left: 0, right: 0, height: 70,
```

- [ ] **Step 6: Also update the panels' bottom offset**

The AI panel, Brief panel, and date picker all use `bottom: 78` (which is `58 + 20`). They need to use `barBottom + 20` to stay anchored above the bar.

Find all occurrences of `bottom: 78` inside the bar div and replace with `bottom: barBottom + 20`:

There are three panels:
1. Date picker panel: `position: "absolute", bottom: 78, left: 180`
2. AI chat panel: `position: "absolute", bottom: 78, right: 0`
3. Brief panel: `position: "absolute", bottom: 78, left: 0`

Replace each `bottom: 78` with `bottom: barBottom + 20`.

- [ ] **Step 6b: Update the rewind mode timeline position**

The rewind mode has a timeline positioned with a hardcoded `bottom: 58`. Find this div (inside the rewind mode `return` block):

```typescript
        <div onClick={(e) => e.stopPropagation()} style={{ position: "absolute", bottom: 58, left: 0, right: 0, padding: "0 50px 16px", zIndex: 20 }}>
```

Replace `bottom: 58` with `bottom: barBottom`:

```typescript
        <div onClick={(e) => e.stopPropagation()} style={{ position: "absolute", bottom: barBottom, left: 0, right: 0, padding: "0 50px 16px", zIndex: 20 }}>
```

- [ ] **Step 7: TypeScript check**

```powershell
npx tsc --noEmit 2>&1 | head -30
```

Expected: no errors.

- [ ] **Step 8: Commit**

```powershell
git add src/lib/commands.ts src/App.tsx
git commit -m "feat(windows): dynamic bar position above taskbar"
```

---

## Task 7: End-to-end smoke test on Windows

This task has no code changes — it is a manual verification checklist. Run on a Windows machine with ffmpeg and (optionally) Claude CLI installed.

- [ ] **Build the app**

```powershell
npm install
npx tauri build --target x86_64-pc-windows-msvc
```

Or for a dev run:
```powershell
npx tauri dev
```

- [ ] **Verify: Bar appears above taskbar**

Launch the app. The white glass bar should sit flush above the Windows taskbar with an 8px gap. If the taskbar is at the default position (bottom), the bar should be at approximately pixel 48 from the bottom (40px taskbar + 8px gap).

- [ ] **Verify: Screenshot capture works**

Wait 5 seconds after launch. Open `%USERPROFILE%\.mindscope\data\frames\<today>\` in Explorer. JPEG files should appear every 2–3 seconds.

- [ ] **Verify: OCR and FTS5 search work**

Type a word visible on your screen into the Search box. Results should appear. Click a result — the timeline should jump to that frame.

- [ ] **Verify: Audio recording works**

Click the mic button in the bar. It should turn red. Speak a sentence. Wait 5 seconds. Check `%USERPROFILE%\.mindscope\data\audio\<today>.json` — it should contain a transcript entry.

- [ ] **Verify: Meeting detection works (if a meeting app is installed)**

Open Zoom or Teams (any meeting app). The Transcript tab dot in the AI panel should appear. Open a meeting. The Transcript tab should populate with lines.

- [ ] **Verify: Synapse runs (if Claude CLI is installed)**

```powershell
claude --version
```

If Claude CLI is present, open the Brief panel and click Refresh. The working memory should update within 60 seconds. Check `%USERPROFILE%\.mindscope\vault\_working-memory.md` — it should be updated and contain only LF line endings (verify with `Format-Hex _working-memory.md | Select-String "0D 0A"` — should return nothing).

- [ ] **Verify: No regressions on macOS (build only)**

```bash
cargo check --target aarch64-apple-darwin 2>&1 | grep "^error"
```

Expected: no errors. (Run on macOS CI or from a macOS machine if available.)

- [ ] **Final commit if any fixups were needed during smoke test**

```powershell
git add -A
git commit -m "fix(windows): smoke test fixups"
```
