# How MindScope Works

A 5-minute walkthrough of what happens after you launch the app.

---

## The Big Picture

MindScope is a thin bar at the bottom of your screen, but behind it three loops run silently:

```
    ┌────────────────────────────────────────────────────────┐
    │                  Your Mac (everything)                  │
    │                                                          │
    │    ① Capture Loop      ② Meeting Loop    ③ Memory Loop   │
    │    (every ~2 sec)      (when meeting)    (every 30 min)  │
    │         │                   │                  │         │
    │         ▼                   ▼                  ▼         │
    │    ~/.mindscope/data/   ~/.mindscope/     ~/.mindscope/  │
    │    (frames + SQLite)    data/audio/          vault/      │
    │                                                          │
    │                          ▲                               │
    │                          │                               │
    │                   You — via the Bar                      │
    └────────────────────────────────────────────────────────┘
```

All three loops run on your Mac. No cloud. The only time anything leaves your
machine is if you explicitly ask Claude a question via the Ask AI button — and
even then, only the question (plus a small excerpt of matched OCR) goes out.

---

## ① The Capture Loop — how your screen becomes searchable

Every ~2 seconds, MindScope:

1. **Picks a monitor** (if you have multiple, it chooses the one with recent activity)
2. **Takes a screenshot** using macOS's native screen capture API
3. **Runs OCR** with Apple's Vision framework (same thing Photos uses) — fully on-device
4. **Captures the app name + window title** via NSWorkspace
5. **Saves the JPEG** to `~/.mindscope/data/frames/<date>/`
6. **Writes one row** to SQLite, and updates the FTS5 full-text index

The whole cycle takes a few hundred milliseconds and uses maybe 3–5% CPU.

> **Privacy guardrails**: if you're in an Incognito window, the frame is skipped.
> If the app is on your exclusion list (Settings → Privacy), it's skipped.
> If MindScope's own window is frontmost, it's skipped.

### Why is search instant?

Because SQLite's **FTS5** (Full-Text Search v5) builds an inverted index of
every word you've seen on screen. When you type "transformer" in the search
bar, FTS5 looks up the word in O(1) and returns the exact frames — usually in
under 10 ms, even after months of captures. **Zero tokens, zero network.**

---

## ② The Meeting Loop — how conversations become transcripts

MindScope doesn't always record audio. It watches for two signals:

1. **A known meeting app is running** (Zoom, Teams, Google Meet, Tencent Meeting, FaceTime, Lark, DingTalk, WeMeet, Webex, Discord)
2. **The microphone is actively in use**

**Both must be true.** This is why dictation tools like Wispr Flow don't trigger a false meeting — the mic is active but no meeting app is running.

When a meeting starts:

```
  Mic activity detected
          │
          ▼
  ffmpeg captures audio (16 kHz, mono)
          │
          ▼
  Buffer 2 seconds of samples
          │
          ▼
  whisper.cpp transcribes locally (Metal-accelerated on Apple Silicon)
          │
          ▼
  Append to ~/.mindscope/data/audio/<date>.json
          │
          ▼
  Stream text to the AI panel's Transcript tab
```

The Whisper model is `ggml-base.bin` (141 MB multilingual) — downloaded on
first launch, then always local. **Your voice never leaves the Mac.** End-to-end
latency from spoken word to transcript on screen: 2–4 seconds.

Sessions are **activity-based, not time-based**: one Zoom call = one session,
even if it runs for hours. When the meeting ends (app closes or mic stops),
the session seals and the transcript becomes searchable under Search → Meetings.

---

## ③ The Memory Loop — how MindScope builds context over time

This is the part that feels most like "AI." Every 30 minutes, a small
background task called **Synapse** wakes up and asks Claude to digest your
recent activity into structured memory.

### The Three-Tier Memory

Synapse keeps your knowledge in three layers, mirroring how human memory works:

| Tier | File | What's in it | Updated |
|---|---|---|---|
| 🔥 **Hot** | `_working-memory.md` | What you're doing *right now* — today's focus, recent apps, active people | Every 30 min |
| 🌤 **Warm** | `_warm-memory.md` | Rolling context — follow-ups, project momentum, collaborator state | End of each cycle |
| ❄ **Cold** | `meet.*.md`, `user.*.md`, `proj.*.md`, `daily.journal.*.md` | Long-term vault — one file per meeting, person, project, day | Only when new data arrives |

When you open the **Daily Brief** or click **Ask AI**, MindScope reads from
these files — so the more you use it, the more contextual the answers get.

### What Synapse actually does each cycle

1. **Reads** the last 2 hours of frames from SQLite (app usage, OCR snippets)
2. **Reads** any new meeting transcripts from today
3. **Calls Claude CLI** with a strict prompt: "use the Edit tool only, don't exceed 4000 chars, preserve the `## User Notes` section"
4. **Claude patches** the Hot tier with updated focus and activity
5. **Claude promotes** stale items from Hot → Warm (tasks older than 14 days go to "Needs Triage")
6. **A size guard** trims the files if Claude goes over budget

Three safety rules prevent the memory from drifting:

- **Section-scoped edits**: Claude can only touch the headings Synapse owns (`## Current Focus`, `## Today's Activity`, etc.). Everything else — especially `## User Notes` — is untouchable.
- **Hard size caps**: Hot ≤ 4000 chars, Warm ≤ 8000 chars. Auto-trimmed if exceeded.
- **Patch, never rewrite**: Claude uses Edit tool (`old_string → new_string`), never Write. Regression risk ≈ zero.

### Why Haiku, not Sonnet?

The Synapse loop runs 48 times a day. If it used Sonnet, that'd burn your
Claude subscription quota fast. Instead, Synapse routes through **Claude
Haiku** — good enough for "read summary, apply patch," and ~10× cheaper.

Only the **Ask AI button** (which you click yourself) uses Sonnet, because
that's where reasoning quality matters.

---

## When AI is involved (and when it isn't)

This is important for understanding cost and privacy:

| Action | Uses AI? | Data sent to cloud? |
|---|---|---|
| Screen capture + OCR | ❌ Local Vision framework | None |
| Search bar | ❌ SQLite FTS5 | None |
| Meeting transcription | ❌ Local whisper.cpp | None |
| Timeline rewind | ❌ Reads local frames | None |
| Daily Brief display | ❌ Reads local markdown | None |
| **Synapse loop** (every 30 min) | ✅ Claude Haiku via CLI | Summary of last 2h activity |
| **Ask AI button** (you click it) | ✅ Claude Sonnet via CLI | Your question + top 15 FTS5 hits |
| **Meeting notes generation** (on meeting end) | ✅ Claude Haiku via CLI | Meeting transcript + screen context |

**Key point**: AI is only invoked in three narrow places. 99% of the app —
capture, search, replay, timeline — runs entirely offline on your Mac. If you
never install Claude CLI (`brew install claude`), MindScope still works; you
just lose the three AI features above.

### Who pays for the tokens?

MindScope doesn't have its own API keys. It shells out to the `claude` CLI,
which uses **your own Claude Pro / Max subscription**. MindScope itself costs
nothing to run — you're just using AI features against your own quota.

---

## Where your data lives

Everything is under `~/.mindscope/`:

```
~/.mindscope/
├── data/
│   ├── frames/<date>/       # JPEG screenshots
│   ├── segments/            # HEVC video (if enabled)
│   ├── audio/<date>.json    # Transcripts
│   └── mindscope.db         # SQLite + FTS5 index
│
├── vault/                   # Your knowledge base (markdown)
│   ├── _working-memory.md   # Hot tier
│   ├── _warm-memory.md      # Warm tier
│   ├── meet.*.md            # One per meeting session
│   ├── daily.journal.*.md   # One per day
│   └── user.*.md            # One per person detected
│
├── synapse/                 # Bundled AI skill definitions
├── models/ggml-base.bin     # Whisper model (141 MB)
├── bin/                     # Compiled Swift helpers
└── settings.json
```

**Typical footprint**: ~400 MB per day of active use with default settings.
You can cap retention in Settings → Storage.

---

## The 30-second summary

1. **Three loops** run on your Mac: capture every 2s, meeting transcription on demand, memory digest every 30 min.
2. **Search is free and instant** — SQLite FTS5, no AI, no network, <10 ms.
3. **AI only runs in 3 places**: Synapse loop (Haiku), meeting notes (Haiku), Ask AI button (Sonnet).
4. **Memory has three tiers**: Hot (now) → Warm (this week) → Cold (forever). Edit-only patches prevent drift.
5. **Everything lives under `~/.mindscope/`** — nothing in the cloud unless you explicitly ask.

That's it. Open the bar, type to search, click Ask AI when you need reasoning.
The rest happens quietly in the background.
