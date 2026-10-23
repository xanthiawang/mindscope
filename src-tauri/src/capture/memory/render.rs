//! The markdown vault as a RENDERED VIEW of the fact store, plus the one-time
//! migration that imports existing vault files into the store, plus the
//! entry-aware compressor for the hot/warm tiers.
//!
//! Safety rule (so a live run never clobbers hand-written vault files):
//! a cold file is (over)written only when it does not yet exist, or when it
//! already carries the `synapse-store: 1` frontmatter marker that the store
//! itself stamps. Pre-existing user-authored files are imported into the store
//! read-only and left untouched on disk until the user opts in.

use std::path::Path;

use super::store::{Fact, Kind, MemoryStore};

/// Frontmatter marker identifying a file the store owns and may re-render.
pub const STORE_MARKER: &str = "synapse-store: 1";

/// Title-case a slug: `sarah-kim` → `Sarah Kim`.
fn display_name(slug: &str) -> String {
    slug.split(['-', '.', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', ""))
}

/// Stable per-subject frontmatter id; generated once and remembered in `meta`
/// so re-renders keep the same id (the UI and wikilinks rely on it).
fn subject_id(store: &MemoryStore, kind: Kind, subject: &str) -> String {
    let key = format!("subjectid:{}:{}", kind.as_str(), subject);
    if let Some(id) = store.get_meta(&key) {
        return id;
    }
    let id = super::store::fid();
    store.set_meta(&key, &id);
    id
}

/// Render one subject's live facts to markdown. `archived` is how many facts for
/// the subject have decayed out of view (shown as a one-line summary, not lost).
fn render_subject(store: &MemoryStore, kind: Kind, subject: &str, facts: &[Fact], archived: usize) -> String {
    let id = subject_id(store, kind, subject);
    let title = display_name(subject);
    let updated_ms = facts.iter().map(|f| f.updated_at).max().unwrap_or(0) * 1000;
    let desc = facts
        .iter()
        .max_by_key(|f| f.importance)
        .map(|f| f.text.chars().take(80).collect::<String>())
        .unwrap_or_else(|| format!("{} notes", kind.as_str()));

    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("id: {}\n", id));
    out.push_str(&format!("title: {}\n", title));
    out.push_str(&format!("desc: {}\n", single_quote(&desc)));
    out.push_str(&format!("updated: {}\n", updated_ms));
    out.push_str(&format!("{}\n", STORE_MARKER));
    out.push_str("---\n\n");
    out.push_str("<!-- Rendered view of the MindScope Synapse fact store. Edits here are\n");
    out.push_str("     overwritten on the next deep pass; change facts via the store. -->\n\n");
    out.push_str("## Facts\n\n");

    // Highest-importance first so the most relevant facts lead.
    let mut ordered: Vec<&Fact> = facts.iter().collect();
    ordered.sort_by(|a, b| b.importance.cmp(&a.importance).then(a.created_at.cmp(&b.created_at)));
    for f in ordered {
        let refs = if f.source_refs.is_empty() { "—".to_string() } else { f.source_refs.join(",") };
        // Provenance trailer: every line traces to what produced it + its weight.
        out.push_str(&format!("- {}  <!-- fact:{} src:{} imp:{} -->\n", f.text, f.id, refs, f.importance));
    }

    if archived > 0 {
        out.push_str(&format!("\n_{} older fact(s) decayed out of this view; recoverable from fact_history._\n", archived));
    }
    out
}

#[derive(Debug, Default, Clone)]
pub struct RenderReport {
    pub written: usize,
    pub skipped_user_owned: usize,
    pub pruned: usize,
}

/// Render every live subject to its cold file, honouring the store-marker
/// safety rule, and prune store-owned cold files whose subject no longer has
/// any live facts (so the vault is a faithful, bounded view of the store).
/// Returns how many files were (re)written, skipped (user-authored), or pruned.
pub fn render_cold(store: &MemoryStore, vault: &Path) -> RenderReport {
    let mut report = RenderReport::default();
    if std::fs::create_dir_all(vault).is_err() {
        return report;
    }
    // Count archived (tombstoned) facts per subject for the summary line.
    let mut archived: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for f in store.all_facts(true) {
        if f.tombstoned {
            *archived.entry(f.subject.clone()).or_insert(0) += 1;
        }
    }

    let mut live_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (kind, subject) in store.live_subjects() {
        let facts = store.facts_for_subject(&subject);
        if facts.is_empty() {
            continue;
        }
        let filename = format!("{}.{}.md", kind.file_prefix(), subject);
        let path = vault.join(&filename);
        if !is_safe_to_write(&path) {
            report.skipped_user_owned += 1;
            continue;
        }
        let body = render_subject(store, kind, &subject, &facts, archived.get(&subject).copied().unwrap_or(0));
        if std::fs::write(&path, body).is_ok() {
            report.written += 1;
            live_files.insert(filename);
        }
    }

    // Prune store-owned cold files with no live subject left (never user files).
    if let Ok(rd) = std::fs::read_dir(vault) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_cold = name.starts_with("user.") || name.starts_with("proj.") || name.starts_with("meet.") || name.starts_with("note.");
            if !is_cold || !name.ends_with(".md") || live_files.contains(&name) {
                continue;
            }
            // Only remove files the store owns (carry the marker).
            match std::fs::read_to_string(entry.path()) {
                Ok(c) if c.contains(STORE_MARKER) => {
                    if std::fs::remove_file(entry.path()).is_ok() {
                        report.pruned += 1;
                    }
                }
                _ => {}
            }
        }
    }
    report
}

/// A path is safe for the store to write if it doesn't exist, or it exists and
/// already carries the store marker.
fn is_safe_to_write(path: &Path) -> bool {
    match std::fs::read_to_string(path) {
        Err(_) => true, // doesn't exist (or unreadable) → create
        Ok(content) => content.contains(STORE_MARKER),
    }
}

// ─── migration (item 1) ──────────────────────────────────────────

/// Meta flag marking the one-time vault→store migration done.
const MIGRATED_KEY: &str = "migrated_v1";

/// Import existing cold vault files into the store exactly once. Read-only on
/// disk: originals are never modified or deleted, so there is no data loss and
/// the operation is safe to run on first start. Each imported line becomes a
/// fact tagged `migrated:<filename>` for provenance; the later deep pass dedups
/// them. Returns the number of facts imported (0 if already migrated).
pub fn migrate_vault(store: &MemoryStore, vault: &Path, now: i64) -> usize {
    if store.get_meta(MIGRATED_KEY).as_deref() == Some("1") {
        return 0;
    }
    let mut imported = 0usize;
    if let Ok(rd) = std::fs::read_dir(vault) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".md") {
                continue;
            }
            let kind = if name.starts_with("user.") {
                Kind::Person
            } else if name.starts_with("proj.") {
                Kind::Project
            } else if name.starts_with("meet.") {
                Kind::Meeting
            } else {
                continue; // skip _working/_warm/daily.journal/CLAUDE etc.
            };
            let subject = name
                .trim_end_matches(".md")
                .splitn(2, '.')
                .nth(1)
                .unwrap_or("")
                .to_string();
            if subject.is_empty() {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(entry.path()) else { continue };
            // A store-generated file is skipped (its facts are already in the store).
            if content.contains(STORE_MARKER) {
                continue;
            }
            for fact_text in extract_facts_from_markdown(&content) {
                let c = super::store::Candidate::new(kind, subject.clone(), fact_text)
                    .with_source(format!("migrated:{}", name));
                store.add_fact(&c, now);
                imported += 1;
            }
        }
    }
    store.set_meta(MIGRATED_KEY, "1");
    imported
}

/// Pull candidate fact lines out of a cold markdown file body: non-empty lines
/// that aren't frontmatter, headings, comments, separators or table chrome.
fn extract_facts_from_markdown(content: &str) -> Vec<String> {
    let body = strip_frontmatter(content);
    let mut facts = Vec::new();
    for raw in body.lines() {
        let line = raw.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with("<!--")
            || line.starts_with("---")
            || line.starts_with("|")
        {
            continue;
        }
        let text = line.trim_start_matches(['-', '*', ' ']).trim();
        // Drop empty bullets and the seed placeholders.
        if text.len() < 3 || text.starts_with('(') {
            continue;
        }
        facts.push(text.to_string());
    }
    facts
}

fn strip_frontmatter(content: &str) -> &str {
    if let Some(rest) = content.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            return &rest[end + 4..];
        }
    }
    content
}

// ─── hot/warm compression (item 3) ───────────────────────────────

/// A managed section and the max number of entries it may keep.
pub struct SectionCap {
    pub heading: &'static str,
    pub max_entries: usize,
}

/// Compress a hot/warm markdown file by removing whole OLDEST entries from its
/// managed sections when it is over `char_cap` or a section is over its entry
/// cap — never by cutting mid-line or mid-table-row. Frontmatter, headings,
/// comments, separators, the user-owned zone and everything below it are
/// preserved verbatim. A dropped-entries marker records what was summarised.
pub fn compress_managed(content: &str, char_cap: usize, caps: &[SectionCap]) -> String {
    #[derive(Clone)]
    struct L {
        text: String,
        section: Option<usize>, // index into caps, if inside a managed section
        is_entry: bool,
        dropped: bool,
    }

    let mut lines: Vec<L> = Vec::new();
    let mut current: Option<usize> = None;
    let mut sep_seen = false;
    let mut frozen = false; // user-owned zone reached

    for raw in content.lines() {
        let trimmed = raw.trim_start();
        if raw.contains("USER-OWNED") || trimmed.starts_with("## User Notes") {
            frozen = true;
        }
        if trimmed.starts_with("## ") && !frozen {
            let h = trimmed.trim_start_matches('#').trim();
            current = caps.iter().position(|c| h.contains(c.heading));
            sep_seen = false;
            lines.push(L { text: raw.to_string(), section: None, is_entry: false, dropped: false });
            continue;
        }
        let mut is_entry = false;
        if let Some(_sec) = current {
            if !frozen {
                let is_bullet = trimmed.starts_with("- ") || trimmed.starts_with("* ");
                let is_table = trimmed.starts_with('|');
                if is_table && trimmed.contains("---") {
                    sep_seen = true;
                } else if is_table && sep_seen {
                    is_entry = true; // table data row (after the separator)
                } else if is_bullet {
                    // A seed placeholder like "- (none yet)" is not a real entry.
                    let body = trimmed.trim_start_matches(['-', '*', ' ']);
                    is_entry = !body.starts_with('(');
                }
            }
        }
        lines.push(L { text: raw.to_string(), section: if frozen { None } else { current }, is_entry, dropped: false });
    }

    // 1. Per-section entry cap: drop earliest entries beyond the cap.
    for (sec, cap) in caps.iter().enumerate() {
        let entry_idxs: Vec<usize> =
            lines.iter().enumerate().filter(|(_, l)| l.section == Some(sec) && l.is_entry).map(|(i, _)| i).collect();
        if entry_idxs.len() > cap.max_entries {
            let drop_n = entry_idxs.len() - cap.max_entries;
            for &i in entry_idxs.iter().take(drop_n) {
                lines[i].dropped = true;
            }
        }
    }

    // 2. Global char cap: drop earliest remaining entries from the largest
    //    managed section until under the cap or nothing droppable remains.
    let rendered = |lines: &[L]| -> String {
        lines.iter().filter(|l| !l.dropped).map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n")
    };
    while rendered(&lines).chars().count() > char_cap {
        // Pick the section with the most surviving entries.
        let mut counts = vec![0usize; caps.len()];
        for l in &lines {
            if let Some(s) = l.section {
                if l.is_entry && !l.dropped {
                    counts[s] += 1;
                }
            }
        }
        let Some((biggest, &n)) = counts.iter().enumerate().max_by_key(|(_, &c)| c) else { break };
        if n == 0 {
            break; // no managed entries left to drop; avoid mid-item truncation
        }
        // Drop the earliest surviving entry of that section.
        if let Some(i) = lines
            .iter()
            .position(|l| l.section == Some(biggest) && l.is_entry && !l.dropped)
        {
            lines[i].dropped = true;
        } else {
            break;
        }
    }

    // 3. Insert a summary marker after each section header that lost entries.
    let mut out_lines: Vec<String> = Vec::new();
    let mut dropped_per_section = vec![0usize; caps.len()];
    for l in &lines {
        if l.dropped {
            if let Some(s) = l.section {
                dropped_per_section[s] += 1;
            }
        }
    }
    let mut marker_emitted = vec![false; caps.len()];
    let mut i = 0usize;
    while i < lines.len() {
        let l = &lines[i];
        if !l.dropped {
            out_lines.push(l.text.clone());
            // After a managed header with drops, emit the marker once.
            if let Some(sec) = caps.iter().position(|c| l.text.contains(c.heading)) {
                if l.text.trim_start().starts_with("## ") && dropped_per_section[sec] > 0 && !marker_emitted[sec] {
                    out_lines.push(format!(
                        "- _(+{} older entr{} summarized to stay under the cap)_",
                        dropped_per_section[sec],
                        if dropped_per_section[sec] == 1 { "y" } else { "ies" }
                    ));
                    marker_emitted[sec] = true;
                }
            }
        }
        i += 1;
    }
    let mut result = out_lines.join("\n");
    if content.ends_with('\n') && !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Managed sections for `_working-memory.md` (hot tier).
pub fn working_memory_caps() -> Vec<SectionCap> {
    vec![
        SectionCap { heading: "Current Focus", max_entries: 6 },
        SectionCap { heading: "Today's Activity", max_entries: 8 },
        SectionCap { heading: "Recent People", max_entries: 6 },
        SectionCap { heading: "Source Sync Status", max_entries: 4 },
    ]
}

/// Managed sections for `_warm-memory.md` (warm tier).
pub fn warm_memory_caps() -> Vec<SectionCap> {
    vec![
        SectionCap { heading: "Active Follow-Ups", max_entries: 10 },
        SectionCap { heading: "Project Momentum", max_entries: 8 },
        SectionCap { heading: "Collaborator State", max_entries: 10 },
        SectionCap { heading: "Needs Triage", max_entries: 5 },
        SectionCap { heading: "Recent Decisions", max_entries: 5 },
        SectionCap { heading: "Decision Patterns", max_entries: 8 },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::store::{Candidate, Kind, MemoryStore};

    #[test]
    fn compress_drops_whole_entries_never_mid_line() {
        let mut content = String::from("---\nid: x\n---\n\n## Recent People\n\n");
        for i in 0..20 {
            content.push_str(&format!("- Person number {} with a reasonably long descriptive line\n", i));
        }
        content.push_str("\n## User Notes\n- keep me verbatim exactly\n");
        let out = compress_managed(&content, 400, &working_memory_caps());
        assert!(out.chars().count() <= 400 + 80, "within cap (+marker slack)");
        // No line was cut mid-way: every non-empty line is a complete original line or the marker.
        for line in out.lines() {
            if line.starts_with("- Person number") {
                assert!(line.ends_with("descriptive line"), "entry kept whole: {line:?}");
            }
        }
        assert!(out.contains("keep me verbatim exactly"), "user zone preserved");
        assert!(out.contains("summarized"), "drop marker present");
    }

    #[test]
    fn migrate_is_read_only_and_once() {
        let tmp = std::env::temp_dir().join(format!("ms-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("user.sarah-kim.md");
        let original = "---\ntitle: Sarah Kim\n---\n\n## Role\n\n- Senior PM at Acme\n- Leads the memory team\n";
        std::fs::write(&file, original).unwrap();

        let s = MemoryStore::open_in_memory().unwrap();
        let n = migrate_vault(&s, &tmp, 100);
        assert_eq!(n, 2, "two body lines imported");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original, "original file untouched");
        // Second run is a no-op.
        assert_eq!(migrate_vault(&s, &tmp, 200), 0);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn render_respects_user_owned_files() {
        let tmp = std::env::temp_dir().join(format!("ms-render-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let s = MemoryStore::open_in_memory().unwrap();
        s.add_fact(&Candidate::new(Kind::Person, "amy-lee", "PM at Acme").with_source("day:a"), 1);

        // A hand-written file WITHOUT the marker must not be overwritten.
        let handwritten = tmp.join("user.amy-lee.md");
        std::fs::write(&handwritten, "my own notes, do not touch").unwrap();
        let r = render_cold(&s, &tmp);
        assert_eq!(r.skipped_user_owned, 1);
        assert_eq!(std::fs::read_to_string(&handwritten).unwrap(), "my own notes, do not touch");

        // A brand-new subject gets a store-owned, re-renderable file.
        s.add_fact(&Candidate::new(Kind::Person, "bob-ng", "designer").with_source("day:b"), 1);
        let r = render_cold(&s, &tmp);
        assert!(r.written >= 1);
        let bob = std::fs::read_to_string(tmp.join("user.bob-ng.md")).unwrap();
        assert!(bob.contains(STORE_MARKER));
        assert!(bob.contains("designer"));
        // Re-render overwrites the store-owned file in place.
        assert!(render_cold(&s, &tmp).written >= 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
