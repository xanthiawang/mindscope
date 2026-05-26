# Windows Parity Design

**Date:** 2026-05-26  
**Status:** Approved  
**Scope:** Make MindScope fully functional on Windows with feature parity to macOS  
**Approach:** Option B — macOS code frozen, Windows-only additions via `#[cfg(target_os = "windows")]`

---

## Goals

- Every macOS feature works identically on Windows: screenshot capture, OCR, FTS5 search, audio recording, meeting detection, Whisper transcription, Synapse memory loop, Ask AI, Daily Brief, Rewind, bar UI
- Zero changes to the macOS build — no risk to the frozen macOS codebase
- Windows users can test the complete feature set end-to-end after this work

## Non-Goals

- CUDA/GPU-accelerated Whisper on Windows (CPU inference is sufficient for ggml-base)
- whisper-rs migration on macOS (macOS keeps the binary shell-out)
- Windows Store packaging (NSIS/MSI installer is sufficient)

---

## Section 1 — Whisper (Windows)

### Approach
Windows uses `whisper-rs` (in-process, CPU) instead of shelling out to a compiled binary. The macOS binary shell-out in `whisper.rs` is untouched.

### Implementation
- Add `whisper-rs = { version = "0.13" }` under `[target.'cfg(target_os = "windows")'.dependencies]` in `Cargo.toml`
- In `src-tauri/src/capture/whisper.rs`, add a `#[cfg(target_os = "windows")]` impl of `run_whisper(audio_path) -> String`:
  - Lazy-init a `Mutex<Option<WhisperContext>>` holding the loaded model
  - Model path: `~/.mindscope/models/ggml-base.bin` (same as macOS — model is platform-agnostic)
  - Read WAV file, run `whisper_rs::FullParams` with default settings, return joined transcript
  - Model is loaded once at first transcription call; held for the app's lifetime
- The macOS `run_whisper()` (binary shell-out) lives under `#[cfg(target_os = "macos")]` — no change

### Build requirement
whisper-rs compiles whisper.cpp via cmake. Windows dev machines need:
- `cmake` (`winget install Kitware.CMake`)
- MSVC build tools (already required for Tauri on Windows)

### Model delivery
`ggml-base.bin` is a Tauri resource on both platforms. Add it to `"resources"` in `tauri.conf.json` if not already present. Windows users download it on first launch (same mechanism as macOS).

---

## Section 2 — Capture Pipeline

### Screenshot
`xcap` is already cross-platform — no changes to the capture call. The existing `#[cfg(target_os = "windows")]` impl of `get_topmost_app_window()` in `screenshot.rs` uses `platform::get_active_window_info()` (Win32 `GetForegroundWindow`).

**Self-exclusion guard:** Add `app_name != "MindScope"` (and `app_name != "mindscope"`) check in `screenshot.rs` to filter out the bar itself on Windows, mirroring what the macOS Swift helper does by bundle ID.

### OCR
`Windows.Media.Ocr` via `ocr_extract_with_regions()` in `windows.rs` — already implemented. No changes.

### Window detection
`platform::get_active_window_info()` in `windows.rs` — already implemented via `GetForegroundWindow` + `QueryFullProcessImageNameW`. No changes.

### Unchanged
Frame storage, FTS5 indexing, JPEG save path (`~/.mindscope/data/frames/<date>/`), PII redaction — all platform-agnostic.

---

## Section 3 — Audio & Meeting Detection

### Audio recording
`audio.rs` already has the Windows path using `platform::find_ffmpeg()` and `dshow` format.

**Fix device string:** `get_audio_device_arg()` on Windows must call `platform::get_default_audio_device()` (which queries ffmpeg's dshow device list and picks the first microphone-named device) instead of the hardcoded GUID fallback in `get_audio_input_device()`. The `list_audio_devices()` and `get_default_audio_device()` functions in `windows.rs` are already written — just wire them up.

### Audio format
Windows records directly to 16kHz mono WAV via dshow + pcm_s16le. whisper-rs receives the WAV directly — no conversion step needed on Windows. The `convert_audio_to_wav()` step exists only in the macOS path (`.m4a` → WAV via `afconvert`) and is not called on Windows.

### Meeting app detection
Add two Windows-specific functions to `windows.rs`:

```rust
pub fn is_meeting_app_running() -> bool { ... }
pub fn get_meeting_app_name() -> Option<String> { ... }
```

Both shell out to `tasklist /FO CSV /NH` and match process names against the known list:
- `zoom.exe` → "Zoom"
- `Teams.exe` → "Microsoft Teams"
- `chrome.exe` + window title contains "Meet" → "Google Meet"
- `lark.exe` → "Lark"
- `DingTalk.exe` → "DingTalk"
- `WeMeet.exe` → "WeMeet"
- `webex.exe` → "Webex"
- `discord.exe` → "Discord"

The mic-active check uses `platform::is_ffmpeg_recording()` (already in `windows.rs`) which checks `tasklist` for a running `ffmpeg.exe`.

Both conditions (meeting app running AND mic active) must be true to declare a meeting active — same dual-signal logic as macOS.

### Unchanged
Session management, transcript JSON storage, 2s buffer loop, session seal logic — platform-agnostic.

---

## Section 4 — Synapse & Claude CLI

### Claude CLI path
`find_claude_cli()` in `windows.rs` checks:
1. `%USERPROFILE%\.claude\claude.exe`
2. `%APPDATA%\Local\Programs\claude\claude.exe`
3. PATH via `where claude`

No changes needed.

### Vault path
`dirs_next::home_dir()` returns `C:\Users\<name>` on Windows → vault at `C:\Users\<name>\.mindscope\vault\`. Already handled by `dirs_next`. No changes.

### Shell invocation
`Command::new(&claude_cli).current_dir(&vault_path)` works on Windows. Tool-free mode flag (`--tool-use-off` or equivalent) applies on both platforms equally.

### Line ending normalization
Vault markdown files must be written with `\n` only. Add a normalize step in `vault_sync.rs` that strips `\r` before writing on all platforms. This prevents Claude CLI's Windows output (`\r\n`) from corrupting vault files over multiple Synapse cycles.

### Unchanged
Three-tier memory structure, Haiku routing, section-scoped patch logic, 30-minute timer, hard size caps — platform-agnostic.

---

## Section 5 — UI, Bar Positioning & Click-Through

### Taskbar height detection
At app startup, `panel.rs` calls `SHAppBarMessage(ABM_GETTASKBARPOS)` (from the `windows::Win32::UI::Shell` feature) to read the taskbar's bounding rect. The bar is positioned at `bottom: taskbar_height + 8px`.

Add `Win32_UI_Shell` to the `windows` crate feature list in `Cargo.toml` (currently missing).

Re-measure on `WM_TASKBARCREATED` (taskbar restart) and `ABN_POSCHANGED` (taskbar moved/resized).

### Tauri command
Add `get_taskbar_height() -> u32` as a Tauri command in `lib.rs`. The frontend calls this once on mount and uses the result to set the bar's `bottom` CSS value. Falls back to `58` on macOS (unchanged).

### Frontend change
In `App.tsx`, replace the hardcoded `bottom: 58` in the bar's `style` with a state variable initialized via `invoke("get_taskbar_height")`. Default is `58` so macOS is unaffected.

### Click-through
`INTERACTIVE_ZONE_PX: AtomicI32` in `panel.rs` already exists. The Windows implementation restricts the hit-testable region to the bottom `INTERACTIVE_ZONE_PX` pixels using `SetWindowRgn` on the `WS_EX_TRANSPARENT | WS_EX_LAYERED` window. Initial value is `taskbar_height + 200px` (bar + default interactive zone).

### Always-on-top
`"alwaysOnTop": true` in `tauri.conf.json` is sufficient. No extra Win32 `SetWindowPos(HWND_TOPMOST)` needed unless testing reveals Tauri's flag doesn't survive focus changes.

### Multi-monitor
Bar appears on the monitor where the cursor is (same as screenshot capture — already uses `mouse: true` in `ScreenInfo`). No additional changes.

---

## Section 6 — Build & Packaging

### Cargo.toml additions

```toml
[target.'cfg(target_os = "windows")'.dependencies]
whisper-rs = { version = "0.13" }

[target.'cfg(target_os = "windows")'.dependencies.windows]
version = "0.58"
features = [
  "Win32_UI_Shell",              # SHAppBarMessage — ADD THIS
  "Win32_UI_WindowsAndMessaging",
  "Win32_Graphics_Gdi",
  "Win32_System_Threading",
  "Media_Ocr",
  "Graphics_Imaging",
  "Storage",
]
```

### tauri.conf.json
Add `ggml-base.bin` to the `"resources"` array if not already present. No macOS bundle changes.

### macOS build
Zero changes. The macOS `Cargo.toml` deps, whisper binary, and bundle config are untouched.

### Windows installer
NSIS/MSI via `tauri build --target x86_64-pc-windows-msvc`. No extra bundled binaries beyond the Tauri defaults and `ggml-base.bin` resource.

### README — Windows install instructions
```
1. Download MindScope_0.1.0_x64.msi from Releases and run the installer.
2. Install ffmpeg: winget install Gyan.FFmpeg
3. (Optional) Install Claude CLI from claude.ai for AI features.
Requirements: Windows 10 1903+ (for Windows.Media.Ocr), x64.
```

---

## Risk Register

| Risk | Likelihood | Mitigation |
|---|---|---|
| whisper-rs cmake build fails on Windows CI | Medium | Pin cmake version; test locally first |
| dshow device string wrong for non-English Windows | Medium | Fallback: enumerate all dshow audio devices, pick first one |
| `SHAppBarMessage` returns zero height (auto-hide taskbar) | Low | Detect auto-hide and use `0` offset instead; bar sits at screen bottom |
| Claude CLI path not found on Windows | Low | Clear error in Settings panel: "Install Claude CLI to enable AI features" |
| xcap misses hardware-accelerated windows (games, DRM) | Low | Known xcap limitation on all platforms; out of scope |
