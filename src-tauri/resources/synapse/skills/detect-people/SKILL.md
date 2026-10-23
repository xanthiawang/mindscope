# Detect People

Reference rules for deciding which names in MindScope's screen text and
transcripts are real people, and what a `user.<slug>.md` entry looks like.
Candidates arrive inlined in the prompt (capitalised two-word names from OCR
and transcripts); there is no script to run and no web search available in the
tool-free loop — fill only what the activity itself supports and leave unknown
fields out rather than inventing them.

## Process

### 1. Validate with judgment

For each candidate, determine if it's a real person:

- **Create entry** -- real person with professional context
- **Link to existing** -- audio name matches existing user phonetically
- **Skip** -- UI element, device name, OCR artifact

**Skip rules** -- these are NOT people:
- UI elements: "App Store", "Enhanced Context", "System Preferences"
- Device names: "HyperX SoloCast", "MacBook Pro"
- OCR artifacts: "Starting VideoCapture", "Screenshot Taken"
- Generic labels: "Unknown User", "Admin Panel"

**When in doubt, skip.** A missed person can be added later; a false entry pollutes the vault.

### 2. Entry format

For each validated person, use a 23-character lowercase alphanumeric `id`
(any random string of that shape) and the current time in ms for `updated`/`created`.
Omit fields you cannot fill from the activity — never write placeholders like "[To be filled]".

**Entry template** (`user.<name-slug>.md`):

```markdown
---
id: <23-char-lowercase-alphanumeric>
title: Full Name
desc: 'One-line role/institution'
updated: <unix-ms>
created: <unix-ms>
---

## Contact Info

- Email: actual-email@example.com
- Institution: University / Company
- Web: https://profile-url

## Role & Expertise

Title and role description.

Brief research or professional summary.

## Context

**How we connected**: Detected from MindScope screen/audio activity

**Date**: YYYY-MM-DD

**Context**: Description of how they appeared (meeting, email, mention)

## Notes

Any relevant observations from the detection context.
```

### 3. Return detected people as JSON

Return results for the calling skill (e.g., daily-journal) to use for linking:

```json
{
  "people": [
    {"name": "Sarah Kim", "slug": "sarah-kim", "status": "existing"},
    {"name": "David Lee", "slug": "david-lee", "status": "created"}
  ],
  "summary": {
    "created": 1,
    "linked": 1,
    "skipped": 35
  }
}
```

Brief text summary:
- Created: N entries (list names)
- Linked: N audio names to existing users
- Skipped: N false positives (total only)

---

**Key Principle**: When in doubt, skip. Entries contain only information the activity supports. No placeholders.
