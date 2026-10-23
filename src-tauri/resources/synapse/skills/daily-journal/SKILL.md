# Daily Journal Generator

Reference format for `daily.journal.YYYY.MM.DD.md`. In MindScope the journal is
produced at 23:00 by a tool-free Claude call: MindScope inlines the day's app
usage, meeting notes and transcripts and asks for the entry below. There is no
script step and no screenpipe — the data comes from MindScope's own SQLite
frames and Whisper transcripts.

## Key Principles

- Optimize for future retrieval value, not completeness
- Compress continuous work sessions into single entries
- Only include audio quotes with actual content (skip noise/testing)
- State & Open Questions first (what you'll actually need when resuming work)
- Notes section contains valuable synthesis (make it prominent)

## NON-NEGOTIABLE Requirements

Every journal MUST have:

1. **People**: Every person mentioned in audio/OCR is named and linked with `[[Name|user.slug]]` (see `detect-people` for what counts as a person).
2. **Synthesized Content**: Timeline entries must contain human-readable synthesis. Raw OCR dumps are REJECTED. If OCR is garbled, describe what was happening based on context clues (app names, timestamps).
3. **Meetings**: Any Zoom/Teams/Meet participant list in OCR = a meeting happened. Identify it, extract participants, link people. Substantial meetings already have a `meet.*.md` note — reference it rather than repeating it.
4. **Correct Timeline Ordering**: Day runs 6 AM to 6 AM next day. Morning first, late night last.
5. **No screenshots or scripts**: the tool-free call cannot extract keyframes or run commands; describe moments in text.

## Meeting Notes vs Daily Journal

When audio indicates a substantial meeting (>1 hour, rich content):

1. **Create a separate meeting note** with factual, topic-based organization (action items, topic sections, direct quotes as blockquotes)
2. **Reference it in the daily journal** timeline with a brief summary and link
3. Keep timeline entry brief -- full content lives in the meeting note

## Timestamps

MindScope records local time already; keep all times in the journal in the user's local time zone.

## Journal Template

```markdown
---
id: <23-char-lowercase-alphanumeric>
title: 'YYYY.MM.DD'
desc: 'Brief description naming actual projects/activities'
updated: <unix-ms>
created: <unix-ms>
---

## Day Summary

2-3 sentences covering main activities and people involved. Name specific projects.

## State & Open Questions

**Project A (with Collaborator)**
- Status: Current state
- Blocker: What's blocking (if anything)
- Next: What needs to happen next

**Project B**
- Status: Where things stand
- Decision: Key decision made today

## Tasks

### Inferred from Today

- [ ] Specific actionable task
- [ ] Follow up with [[Person|user.person]] about X

### Completed Today

- [x] Specific accomplishment
- [x] Meeting with [[Person|user.person]] about X

## Timeline

<!-- Use activity-type cards: dev, research, meeting, email, general -->
<!-- Group by work sessions and context switches, not time-of-day labels -->
<!-- Use HTML formatting inside divs: <strong>, <em>, <a href="...">, <p> -->

<div class="timeline-container">
  <div class="timeline-line"></div>
  <div class="timeline-item">
    <div class="timeline-dot dot-meeting"></div>
    <div class="timeline-time">9:00 AM - 10:30 AM</div>
    <div class="timeline-card card-meeting">
      <div class="timeline-title">Meeting Title</div>
      <div class="timeline-content">
        <p>Summary with <a href="user.person.md">Person Name</a>.</p>
      </div>
    </div>
  </div>
</div>

<div class="timeline-container">
  <div class="timeline-line"></div>
  <div class="timeline-item">
    <div class="timeline-dot dot-dev"></div>
    <div class="timeline-time">11:00 AM - 1:00 PM</div>
    <div class="timeline-card card-dev">
      <div class="timeline-title">Development Session</div>
      <div class="timeline-content">
        <p>Built X feature. <strong>Blocker</strong>: Missing API docs.</p>
      </div>
    </div>
  </div>
</div>

## Notes

Patterns, meta-observations, collaboration dynamics, technical insights, strategic context.
This section is the highest-value synthesis -- not a restatement of the timeline.
```

## Validation Checklist

Before marking complete, verify:

- [ ] People named and linked (all names from audio/OCR)
- [ ] Meetings identified (Zoom/Teams/Meet participant lists)
- [ ] Timeline ordering correct (6 AM top, late night bottom)
- [ ] Content synthesized (no raw OCR dumps)
- [ ] Audio quotes meaningful (decisions/ideas only, no noise)
- [ ] Notes section has synthesis (not restating timeline)

**If any item fails, fix it before answering.**
