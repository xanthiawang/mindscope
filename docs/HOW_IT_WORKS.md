# How MindScope Works

Three loops run behind the bar. Everything is local unless you click Ask AI.

```
   ① Capture (every 2s)   ② Meeting (on demand)   ③ Memory (every 30m)
          │                       │                        │
          ▼                       ▼                        ▼
   frames + FTS5           audio transcripts          vault markdown
          │                       │                        │
          └───────────────┬───────┴────────────────────────┘
                          ▼
                       The Bar
```

---

## ① Capture Loop

Every ~2 seconds on the active monitor:

| Step | Tool | Where it runs |
|---|---|---|
| Screenshot | macOS screen capture API | Local |
| OCR | Apple Vision framework | Local |
| App + window title | NSWorkspace | Local |
| Save JPEG | `~/.mindscope/data/frames/<date>/` | Local |
| Index text | SQLite FTS5 | Local |

~3–5% CPU, a few hundred ms per cycle. Incognito windows, excluded apps, and MindScope's own window are skipped.

**Search is instant** because FTS5 keeps an inverted index — typing `transformer` returns the matching frames in <10 ms, even after months of captures.

---

## ② Meeting Loop

Triggered when **a known meeting app is running** (Zoom / Teams / Google Meet / Tencent / FaceTime / Lark / DingTalk / WeMeet / Webex / Discord) **AND the mic is active**. Both must be true — that's what keeps dictation tools out.

```
ffmpeg (16 kHz mono) → 2 s buffer → whisper.cpp (Metal) → ~/.mindscope/data/audio/<date>.json
```

Whisper model: `ggml-base.bin`, 141 MB, multilingual, downloaded on first launch. End-to-end latency: **2–4 seconds**. Voice never leaves the Mac.

One meeting = one session (activity-based, not time-based). Sessions seal when the app closes or the mic stops.

---

## ③ Memory Loop — Synapse

Every 30 minutes, Synapse reads your recent activity and asks Claude to patch the vault.

### Three tiers

| Tier | File | Purpose | Cap |
|---|---|---|---|
| 🔥 Hot | `_working-memory.md` | Today's focus, recent apps, active people | 4 KB |
| 🌤 Warm | `_warm-memory.md` | Follow-ups, project momentum, collaborator state | 8 KB |
| ❄ Cold | `meet.*.md`, `user.*.md`, `proj.*.md`, `daily.journal.*.md` | Long-term vault, one file per entity | — |

Daily Brief and Ask AI read from these files. The more you use MindScope, the richer the context.

### How Synapse avoids memory drift

- **Edit-only, section-scoped**: Claude patches specific `## Headings` via `old_string → new_string`. Never a full rewrite. Your manual notes in `## User Notes` are off-limits.
- **Hard size caps**: a Rust guard auto-trims the oldest Today's Activity rows and Recent People entries before and after every Claude call.
- **Auto-decay**: unchecked tasks > 14 days drop from Hot → Warm "Needs Triage"; meeting follow-ups > 7 days get tagged Overdue.

### Why Haiku for the loop

Synapse fires 48 times a day. Sonnet would burn through a Pro subscription fast. Haiku handles "read activity, apply patch" perfectly and costs ~10× less. **Ask AI stays on Sonnet** because that's where reasoning matters.

---

## When AI runs (and when it doesn't)

| Action | AI? | What leaves the Mac |
|---|---|---|
| Screen capture + OCR | ❌ | — |
| Search bar (FTS5) | ❌ | — |
| Meeting transcription | ❌ | — |
| Timeline rewind | ❌ | — |
| Daily Brief display | ❌ | — |
| **Synapse loop** (every 30 min) | ✅ Haiku | 2 h activity summary |
| **Meeting notes** (on meeting end) | ✅ Haiku | Transcript + screen context |
| **Ask AI button** (you click it) | ✅ Sonnet | Your question + top 15 FTS5 hits |

**Without Claude CLI**, the top 5 rows still work. You lose the bottom 3.

**Who pays**: MindScope has no API keys. It shells out to `claude`, which uses **your own Claude Pro / Max subscription**. MindScope itself costs nothing.

---

## Where your data lives

```
~/.mindscope/
├── data/
│   ├── frames/<date>/       JPEG screenshots
│   ├── audio/<date>.json    Transcripts
│   └── mindscope.db         SQLite + FTS5
├── vault/
│   ├── _working-memory.md   🔥 Hot (≤ 4 KB)
│   ├── _warm-memory.md      🌤 Warm (≤ 8 KB)
│   ├── meet.*.md            ❄ Per session
│   ├── daily.journal.*.md   ❄ Per day
│   └── user.*.md            ❄ Per person
├── synapse/                 Bundled skills
├── models/ggml-base.bin     Whisper (141 MB)
├── bin/                     Swift helpers
└── settings.json
```

~400 MB/day with defaults. Adjust retention in Settings → Storage.

---

## 30-second summary

1. Three loops: capture (2s), meeting (on demand), memory (30 min).
2. Search = FTS5, instant, no AI, no network.
3. AI runs in exactly 3 places: Synapse loop, meeting notes, Ask AI.
4. Memory has 3 tiers with hard caps and Edit-only patches — no drift.
5. Everything under `~/.mindscope/`. Nothing leaves your Mac unless you click Ask AI.
