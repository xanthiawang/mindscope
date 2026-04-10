# MindScope

**Your AI-powered digital memory. Never forget what you saw, heard, or discussed.**

MindScope runs silently in the background, capturing your screen activity and transforming it into searchable, structured knowledge — all processed locally on your Mac.

---

## Quick Start

### 1. Install
Download `MindScope_0.1.0_aarch64.dmg` from [Releases](https://github.com/xanthiawang/mindscope/releases). Open the DMG and drag MindScope to Applications.

### 2. Grant Permission
On first launch, go to **System Settings > Privacy & Security > Screen Recording** and enable MindScope.

### 3. Use

| Action | How |
|--------|-----|
| Open / Hide | `Cmd + Shift + Space` |
| Hide | `Esc` |
| Browse history | Two-finger swipe on the timeline bar |
| Rewind | Click on the timeline |
| Search | Click the Search field |
| Ask AI | Click the Ask field |
| Daily Brief | Click the clipboard icon |
| Jump to date | Click the time label |
| Settings | Click the gear icon |

### 4. Optional: AI Features
Install [Claude CLI](https://docs.anthropic.com/en/docs/claude-cli) for AI-powered features:
```sh
brew install claude
```

### 5. Optional: Whisper
For better transcription, download the Whisper model in **Settings > Pipes > Download** (142MB).

---

## Features

### Screen Recording & Search
- Automatic capture every 2 seconds, HEVC video compression
- Smart frame dedup (histogram + perceptual hash), saves 60%+ storage
- Full-text OCR search (English & Chinese) with yellow highlight boxes
- App filtering and date/time jump

### Timeline & Rewind
- Color-coded timeline bar with real app icons
- Trackpad two-finger swipe to browse history
- Calendar picker to jump to any date and hour
- Cross-day scrolling (auto-loads previous/next day)

### Meeting Intelligence
- Auto-detects Zoom, Teams, FaceTime, Lark, DingTalk meetings
- Auto-starts recording and Whisper transcription
- Speaker identification
- Auto-generates meeting notes to Knowledge Vault
- Real-time transcript display in AI panel

### Knowledge Vault
- People profiles with auto-updated last contact
- Meeting notes linked to attendees and projects
- Daily journal auto-generated at end of day
- Cross-references via wikilinks

### AI Assistant
- Chat + Transcript tabs in one panel
- Quick actions: "What should I say?", "Recap", "Follow-up questions"
- Context-aware answers using screen history + vault knowledge
- Daily Brief with focus, tasks, and schedule

### Automation (Pipes)
- YAML-defined pipelines with scheduled runs
- Built-in: Daily Summary, Meeting Notes
- Output to clipboard, file, or notification

### Privacy First
- 100% local processing, nothing uploaded
- PII auto-redaction (credit cards, IDs, phone numbers)
- App exclusion list and private browsing mode

---

## Data Storage

All data at `~/.mindscope/`:
```
data/frames/     # Screenshots (JPEG, by date)
data/segments/   # HEVC video segments
data/audio/      # Audio recordings + transcripts
data/mindscope.db  # SQLite database
vault/           # Knowledge vault (markdown)
models/          # Whisper model
pipes/           # Automation configs
```

~400MB/day with default settings.

## Build from Source

```sh
git clone https://github.com/xanthiawang/mindscope.git
cd mindscope
npm install
MACOSX_DEPLOYMENT_TARGET=11.0 npx tauri build
```

## Requirements
- macOS 11.0+
- Apple Silicon or Intel
- Screen Recording permission

## License

MIT — Copyright (c) 2026 Zixin Wang

