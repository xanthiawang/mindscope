---
description: "How MindScope's Synapse keeps the three memory tiers (hot / warm / cold) coherent — what goes where, decay rules, and the shape of a briefing."
---

# Command Center

**Reference for the Synapse loop and the Brief panel.** It describes how
MindScope's local activity data becomes briefings and memory updates. There
are no external services, no agents to dispatch and no scripts: every Claude
call is tool-free with the data inlined in the prompt.

## Three-Tier Memory System

| Tier | File | Purpose | Refresh |
|------|------|---------|---------|
| **Working Memory** | `_working-memory.md` | Hot index: today's focus, recent activity, recent people, sync status. | Every Synapse pass (~30 min, only when new activity exists) |
| **Warm Memory** | `_warm-memory.md` | Medium-term: follow-ups, momentum, collaborator state, decision patterns. | End of each Synapse pass |
| **Vault** | `meet.*.md`, `user.*.md`, `proj.*.md`, `daily.journal.*.md` | Long-term: meeting notes, person profiles, projects, journals. | When MindScope generates them (meeting end, 23:00 journal) |

**Always read in this order**: Working Memory → Warm Memory → Vault.

## Data Sources (all local)

| Source | What it gives | Where it lands |
|--------|---------------|----------------|
| Screen frames (SQLite FTS5, OCR every few seconds) | App time per app, screen text samples | `## Today's Activity`, `## Current Focus` |
| Audio transcripts (`data/audio/YYYY-MM-DD.jsonl`) | Meeting speech with session ids | `## Recent People`, meeting notes |
| Meeting notes (`meet.*.md`) | Summary, key points, action items | `## Active Follow-Ups`, `## Collaborator State` |

### Rules

1. **Short-term writes go to `_working-memory.md`.** It is the single pane of glass for "what's active right now".
2. **Long-term facts belong in vault notes.** When a transcript or meeting note reveals a new person or follow-up, reflect it in warm memory; MindScope creates the vault files.
3. **People are the connective tissue.** Every source can surface people; record the last contact date and context.
4. **Incremental by default.** Never reprocess unchanged data; keep stable lines stable.

## Briefing shape (Brief panel)

MindScope assembles the Brief from vault state and recent frames; when it
asks Claude for a Focus line, answer with one plain sentence (≤ 80 chars).
A fuller briefing, when requested, uses this layout:

```markdown
## Right Now
- current app / window, what the last 2 hours were about
- carry-forward from the last session (from working memory)

## This Week
- overdue follow-ups (> 7 days) with source
- open meeting action items

## Strategic View
- project momentum table (from warm memory)
- stale items needing triage (max 3)

## Suggested Actions (max 5, ordered by urgency)
- each with a one-line reason
```

**Deduplication**: if the same item appears in several sources, show it once in the most relevant section.

## Auto-Decay Thresholds

| Item Type | Threshold | Action |
|-----------|-----------|--------|
| Unchecked tasks (`- [ ]`) | > 14 days | Move to warm memory "Needs Triage" |
| Meeting follow-ups | > 7 days no completion evidence | Mark "Overdue" |
| Project notes | No update > 30 days | Mark "Stale" |
| Today's Activity rows | > 8 | Drop the oldest |
| Recent People entries | > 6 | Drop the least recent |

## Working Memory Template

```markdown
---
id: [existing-id]
title: Working Memory
desc: 'Hot index — auto-updated by MindScope Synapse loop. Hard cap 4000 chars.'
updated: [timestamp]
created: [existing-created]
---

## Current Focus
**Primary**: [main task/project]
**Secondary**: [other active item]
**Tertiary**: —

## Today's Activity
| Time | App | Activity | Duration |
|------|-----|----------|----------|

## Recent People
- **[Name]**: [context]

## Source Sync Status
| Source | Last Synced | Status |
|--------|-------------|--------|
| MindScope Frames | HH:MM | ok |
| MindScope Audio | HH:MM | ok / none today |

---

## User Notes
<!-- USER-OWNED ZONE — never modified by Synapse -->
```

## Warm Memory Management

- **Auto-update (low-risk)**: session date and focus, momentum, collaborator last-contact dates, completed follow-ups.
- **Leave to the user**: priority changes, permanent suppressions, archiving projects — surface them in `## Needs Triage` instead of deciding.
- **Pattern learning** is descriptive ("tends to"), never prescriptive ("always").
