//! The MindScope Synapse fact store — a structured, provenanced, bounded store
//! for cold-tier knowledge (people / projects / meetings facts).
//!
//! The markdown vault (`user.*.md`, `proj.*.md`, `meet.*.md`) becomes a RENDERED
//! VIEW of this store; the store is the source of truth. Each fact carries an
//! identity, provenance (which frames/transcripts/day/app fed it), timestamps,
//! an importance score and a decay watermark, and every mutation writes a
//! history row with the before/after preimage so a bad pass is recoverable.
//!
//! Design borrowed (and re-implemented here, no code copied) from:
//!   * mem0 `DEFAULT_UPDATE_MEMORY_PROMPT` — the ADD/UPDATE/DELETE/NONE event
//!     model and a per-memory history table with before/after values.
//!     (mem0ai/mem0: mem0/configs/prompts.py, mem0/memory/main.py)
//!   * OpenClaw memory docs — write-time provenance, decay of unreferenced
//!     facts, forget-by-source, and a deep "dreaming" pass that keeps rewrite
//!     preimages. (OpenClaw memory design notes)

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension};

use super::super::paths;
use super::super::timeutil;

/// Default importance for a freshly added fact (0..=100).
pub const DEFAULT_IMPORTANCE: i64 = 50;

/// Kind of subject a fact belongs to. Maps to a cold-tier file prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Person,
    Project,
    Meeting,
    Note,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Person => "person",
            Kind::Project => "project",
            Kind::Meeting => "meeting",
            Kind::Note => "note",
        }
    }
    pub fn from_str(s: &str) -> Kind {
        match s {
            "person" => Kind::Person,
            "project" => Kind::Project,
            "meeting" => Kind::Meeting,
            _ => Kind::Note,
        }
    }
    /// File prefix for the rendered cold file (`user`, `proj`, `meet`, `note`).
    pub fn file_prefix(&self) -> &'static str {
        match self {
            Kind::Person => "user",
            Kind::Project => "proj",
            Kind::Meeting => "meet",
            Kind::Note => "note",
        }
    }
}

/// One atomic fact about a subject.
#[derive(Debug, Clone)]
pub struct Fact {
    pub id: String,
    pub kind: Kind,
    /// Subject slug the fact hangs off (person slug, project slug, meeting date-dots).
    pub subject: String,
    pub text: String,
    /// Provenance tokens, e.g. `frame:1234`, `day:2026-10-23`, `app:Safari`,
    /// `transcript:ab12`, `migrated:user.sarah-kim`.
    pub source_refs: Vec<String>,
    pub importance: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_seen: i64,
    /// Local date (`YYYY-MM-DD`) decay last ran for this fact ("" = never).
    pub last_decay_day: String,
    pub tombstoned: bool,
}

/// A candidate fact extracted from recent activity, awaiting reconciliation.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub kind: Kind,
    pub subject: String,
    pub text: String,
    pub source_refs: Vec<String>,
    pub importance: Option<i64>,
}

impl Candidate {
    pub fn new(kind: Kind, subject: impl Into<String>, text: impl Into<String>) -> Self {
        Candidate {
            kind,
            subject: subject.into(),
            text: text.into(),
            source_refs: Vec::new(),
            importance: None,
        }
    }
    pub fn with_source(mut self, src: impl Into<String>) -> Self {
        self.source_refs.push(src.into());
        self
    }
}

static FID_CTR: AtomicU64 = AtomicU64::new(0);

/// A 23-char lowercase alphanumeric id, matching the vault's frontmatter `id`
/// convention. Uniqueness comes from the wall clock in nanoseconds mixed with a
/// per-process counter, so two facts created in the same tick still differ.
pub fn fid() -> String {
    const CHARS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let ctr = FID_CTR.fetch_add(1, Ordering::Relaxed) as u128;
    let pid = std::process::id() as u128;
    let mut seed = nanos ^ (ctr << 48) ^ (pid << 96) ^ 0x9e3779b97f4a7c15;
    let mut out = String::with_capacity(23);
    for _ in 0..23 {
        out.push(CHARS[(seed % 36) as usize] as char);
        // Mix so low entropy in the clock doesn't produce a short cycle.
        seed = (seed / 36) ^ seed.wrapping_mul(0x5851f42d4c957f2d).wrapping_add(1);
    }
    out
}

/// Normalise a fact's text for similarity comparison: lowercase, strip
/// punctuation to spaces, collapse whitespace.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_space = true; // trims leading space
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            for c in ch.to_lowercase() {
                out.push(c);
            }
            prev_space = false;
        } else if !prev_space {
            out.push(' ');
            prev_space = true;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Token set of a fact's text (normalised words).
pub fn tokens(text: &str) -> HashSet<String> {
    normalize(text).split(' ').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect()
}

/// Jaccard similarity of two token sets (0.0..=1.0). Empty/empty = 0.
pub fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 { 0.0 } else { inter / union }
}

/// The structured fact store, backed by its own SQLite file so it can be
/// byte-budgeted and migrated independently of the capture database.
pub struct MemoryStore {
    conn: Connection,
    path: Option<PathBuf>,
}

impl MemoryStore {
    /// Default on-disk store at `$MINDSCOPE_HOME/data/memory.db`.
    pub fn open_default() -> Result<Self, String> {
        Self::open(&paths::data_dir().join("memory.db"))
    }

    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| format!("memory store open: {e}"))?;
        let mut store = MemoryStore { conn, path: Some(path.to_path_buf()) };
        store.init()?;
        Ok(store)
    }

    /// In-memory store (tests).
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("memory store open: {e}"))?;
        let mut store = MemoryStore { conn, path: None };
        store.init()?;
        Ok(store)
    }

    fn init(&mut self) -> Result<(), String> {
        self.conn
            .execute_batch(
                "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA busy_timeout = 5000;

            CREATE TABLE IF NOT EXISTS facts (
                id            TEXT PRIMARY KEY,
                kind          TEXT NOT NULL,
                subject       TEXT NOT NULL,
                text          TEXT NOT NULL,
                source_refs   TEXT NOT NULL DEFAULT '[]',
                importance    INTEGER NOT NULL DEFAULT 50,
                created_at    INTEGER NOT NULL,
                updated_at    INTEGER NOT NULL,
                last_seen     INTEGER NOT NULL,
                last_decay_day TEXT NOT NULL DEFAULT '',
                tombstoned    INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_facts_subject ON facts(subject);
            CREATE INDEX IF NOT EXISTS idx_facts_kind ON facts(kind);
            CREATE INDEX IF NOT EXISTS idx_facts_tomb ON facts(tombstoned);

            CREATE TABLE IF NOT EXISTS fact_history (
                hid         INTEGER PRIMARY KEY AUTOINCREMENT,
                fact_id     TEXT NOT NULL,
                op          TEXT NOT NULL,
                before_text TEXT,
                after_text  TEXT,
                before_json TEXT,
                after_json  TEXT,
                source      TEXT,
                at          INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_hist_fact ON fact_history(fact_id);
            CREATE INDEX IF NOT EXISTS idx_hist_source ON fact_history(source);

            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
        ",
            )
            .map_err(|e| format!("memory store schema: {e}"))?;
        Ok(())
    }

    // ─── meta helpers ────────────────────────────────────────────

    pub fn get_meta(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get::<_, String>(0))
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_meta(&self, key: &str, value: &str) {
        let _ = self
            .conn
            .execute("INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)", rusqlite::params![key, value]);
    }

    // ─── reads ───────────────────────────────────────────────────

    fn row_to_fact(row: &rusqlite::Row) -> rusqlite::Result<Fact> {
        let refs_json: String = row.get(4)?;
        let source_refs: Vec<String> = serde_json::from_str(&refs_json).unwrap_or_default();
        Ok(Fact {
            id: row.get(0)?,
            kind: Kind::from_str(&row.get::<_, String>(1)?),
            subject: row.get(2)?,
            text: row.get(3)?,
            source_refs,
            importance: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
            last_seen: row.get(8)?,
            last_decay_day: row.get(9)?,
            tombstoned: row.get::<_, i64>(10)? != 0,
        })
    }

    const SELECT: &'static str = "SELECT id, kind, subject, text, source_refs, importance, \
         created_at, updated_at, last_seen, last_decay_day, tombstoned FROM facts";

    /// Live (non-tombstoned) facts for a subject.
    pub fn facts_for_subject(&self, subject: &str) -> Vec<Fact> {
        let sql = format!("{} WHERE subject = ?1 AND tombstoned = 0 ORDER BY created_at", Self::SELECT);
        self.conn
            .prepare(&sql)
            .and_then(|mut s| {
                let rows = s.query_map([subject], Self::row_to_fact)?;
                Ok(rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
            })
            .unwrap_or_default()
    }

    /// All live facts (optionally including tombstoned).
    pub fn all_facts(&self, include_tombstoned: bool) -> Vec<Fact> {
        let sql = if include_tombstoned {
            format!("{} ORDER BY subject, created_at", Self::SELECT)
        } else {
            format!("{} WHERE tombstoned = 0 ORDER BY subject, created_at", Self::SELECT)
        };
        self.conn
            .prepare(&sql)
            .and_then(|mut s| {
                let rows = s.query_map([], Self::row_to_fact)?;
                Ok(rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
            })
            .unwrap_or_default()
    }

    pub fn get_fact(&self, id: &str) -> Option<Fact> {
        let sql = format!("{} WHERE id = ?1", Self::SELECT);
        self.conn.query_row(&sql, [id], Self::row_to_fact).optional().ok().flatten()
    }

    /// Distinct subjects that currently have live facts, with their kind.
    pub fn live_subjects(&self) -> Vec<(Kind, String)> {
        self.conn
            .prepare("SELECT DISTINCT kind, subject FROM facts WHERE tombstoned = 0 ORDER BY subject")
            .and_then(|mut s| {
                let rows = s.query_map([], |r| {
                    Ok((Kind::from_str(&r.get::<_, String>(0)?), r.get::<_, String>(1)?))
                })?;
                Ok(rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
            })
            .unwrap_or_default()
    }

    pub fn count_live(&self) -> usize {
        self.conn
            .query_row("SELECT COUNT(*) FROM facts WHERE tombstoned = 0", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as usize
    }

    pub fn count_tombstoned(&self) -> usize {
        self.conn
            .query_row("SELECT COUNT(*) FROM facts WHERE tombstoned = 1", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as usize
    }

    pub fn history_count(&self, fact_id: &str) -> usize {
        self.conn
            .query_row("SELECT COUNT(*) FROM fact_history WHERE fact_id = ?1", [fact_id], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_or(0) as usize
    }

    pub fn total_history_count(&self) -> usize {
        self.conn
            .query_row("SELECT COUNT(*) FROM fact_history", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as usize
    }

    // ─── writes (each records a history preimage) ────────────────

    fn snapshot_json(fact: &Fact) -> String {
        serde_json::json!({
            "id": fact.id,
            "kind": fact.kind.as_str(),
            "subject": fact.subject,
            "text": fact.text,
            "source_refs": fact.source_refs,
            "importance": fact.importance,
            "created_at": fact.created_at,
            "updated_at": fact.updated_at,
            "last_seen": fact.last_seen,
            "tombstoned": fact.tombstoned,
        })
        .to_string()
    }

    fn write_history(
        &self,
        fact_id: &str,
        op: &str,
        before: Option<&Fact>,
        after: Option<&Fact>,
        source: &str,
        at: i64,
    ) {
        let _ = self.conn.execute(
            "INSERT INTO fact_history(fact_id, op, before_text, after_text, before_json, after_json, source, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                fact_id,
                op,
                before.map(|f| f.text.clone()),
                after.map(|f| f.text.clone()),
                before.map(Self::snapshot_json),
                after.map(Self::snapshot_json),
                source,
                at,
            ],
        );
    }

    /// Insert a brand-new fact (op ADD). Returns the new id.
    pub fn add_fact(&self, c: &Candidate, now: i64) -> String {
        let id = fid();
        let refs = merge_sources(&[], &c.source_refs);
        let fact = Fact {
            id: id.clone(),
            kind: c.kind,
            subject: c.subject.clone(),
            text: c.text.clone(),
            source_refs: refs,
            importance: c.importance.unwrap_or(DEFAULT_IMPORTANCE),
            created_at: now,
            updated_at: now,
            last_seen: now,
            last_decay_day: String::new(),
            tombstoned: false,
        };
        let _ = self.conn.execute(
            "INSERT INTO facts(id, kind, subject, text, source_refs, importance, created_at, updated_at, last_seen, last_decay_day, tombstoned)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,0)",
            rusqlite::params![
                fact.id, fact.kind.as_str(), fact.subject, fact.text,
                serde_json::to_string(&fact.source_refs).unwrap_or_else(|_| "[]".into()),
                fact.importance, fact.created_at, fact.updated_at, fact.last_seen, fact.last_decay_day,
            ],
        );
        let src = c.source_refs.first().cloned().unwrap_or_default();
        self.write_history(&id, "ADD", None, Some(&fact), &src, now);
        id
    }

    /// Rewrite an existing fact to a fuller/newer form (op UPDATE). Unions
    /// sources, bumps importance and refreshes last_seen.
    pub fn update_fact(&self, id: &str, new_text: &str, add_sources: &[String], now: i64) -> bool {
        let Some(before) = self.get_fact(id) else { return false };
        let refs = merge_sources(&before.source_refs, add_sources);
        let importance = (before.importance + 5).min(100).max(before.importance);
        let after = Fact {
            text: new_text.to_string(),
            source_refs: refs.clone(),
            importance,
            updated_at: now,
            last_seen: now,
            ..before.clone()
        };
        let _ = self.conn.execute(
            "UPDATE facts SET text=?1, source_refs=?2, importance=?3, updated_at=?4, last_seen=?5 WHERE id=?6",
            rusqlite::params![
                after.text,
                serde_json::to_string(&refs).unwrap_or_else(|_| "[]".into()),
                after.importance, after.updated_at, after.last_seen, id
            ],
        );
        let src = add_sources.first().cloned().unwrap_or_default();
        self.write_history(id, "UPDATE", Some(&before), Some(&after), &src, now);
        true
    }

    /// Touch a fact as seen again (op NONE is not recorded as history; we only
    /// refresh last_seen and union any new sources). Keeps duplicates from
    /// accumulating while still recording that the fact is still current.
    pub fn touch_fact(&self, id: &str, add_sources: &[String], now: i64) -> bool {
        let Some(before) = self.get_fact(id) else { return false };
        let refs = merge_sources(&before.source_refs, add_sources);
        let _ = self.conn.execute(
            "UPDATE facts SET source_refs=?1, last_seen=?2 WHERE id=?3",
            rusqlite::params![serde_json::to_string(&refs).unwrap_or_else(|_| "[]".into()), now, id],
        );
        true
    }

    /// Tombstone a fact (op DELETE / DECAY / etc.). The row and its history
    /// stay for recovery; `all_facts(true)` and the history table preserve the
    /// preimage.
    pub fn tombstone(&self, id: &str, op: &str, source: &str, now: i64) -> bool {
        let Some(before) = self.get_fact(id) else { return false };
        if before.tombstoned {
            return false;
        }
        let _ = self.conn.execute(
            "UPDATE facts SET tombstoned=1, updated_at=?1 WHERE id=?2",
            rusqlite::params![now, id],
        );
        self.write_history(id, op, Some(&before), None, source, now);
        true
    }

    /// Merge duplicate facts into a canonical one (op MERGE). Updates the
    /// canonical's text/sources/importance/last_seen in place (created_at is
    /// preserved) and records a history row only when something actually
    /// changed, so a second dedup pass that finds nothing is a no-op.
    pub fn merge_canonical(
        &self,
        id: &str,
        new_text: &str,
        add_sources: &[String],
        importance: i64,
        last_seen: i64,
        now: i64,
    ) -> bool {
        let Some(before) = self.get_fact(id) else { return false };
        let refs = merge_sources(&before.source_refs, add_sources);
        let importance = importance.clamp(0, 100).max(before.importance);
        let last_seen = last_seen.max(before.last_seen);
        let changed = before.text != new_text
            || before.source_refs != refs
            || before.importance != importance
            || before.last_seen != last_seen;
        if !changed {
            return false;
        }
        let after = Fact {
            text: new_text.to_string(),
            source_refs: refs.clone(),
            importance,
            last_seen,
            updated_at: now,
            ..before.clone()
        };
        let _ = self.conn.execute(
            "UPDATE facts SET text=?1, source_refs=?2, importance=?3, last_seen=?4, updated_at=?5 WHERE id=?6",
            rusqlite::params![
                after.text,
                serde_json::to_string(&refs).unwrap_or_else(|_| "[]".into()),
                after.importance, after.last_seen, after.updated_at, id
            ],
        );
        self.write_history(id, "MERGE", Some(&before), Some(&after), "deep-pass", now);
        true
    }

    /// Ids of the lowest-value live facts (lowest importance, then least
    /// recently seen) — the first to shed when over the byte budget.
    pub fn lowest_value_facts(&self, limit: usize) -> Vec<String> {
        self.conn
            .prepare(
                "SELECT id FROM facts WHERE tombstoned = 0 \
                 ORDER BY importance ASC, last_seen ASC LIMIT ?1",
            )
            .and_then(|mut s| {
                let rows = s.query_map([limit as i64], |r| r.get::<_, String>(0))?;
                Ok(rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
            })
            .unwrap_or_default()
    }

    /// Set importance + decay watermark (used by the decay pass). Writes a
    /// DECAY history row only when the value actually changes.
    pub fn set_importance(&self, id: &str, importance: i64, decay_day: &str, now: i64) {
        let Some(before) = self.get_fact(id) else { return };
        let importance = importance.clamp(0, 100);
        let changed = before.importance != importance;
        let after = Fact { importance, last_decay_day: decay_day.to_string(), ..before.clone() };
        let _ = self.conn.execute(
            "UPDATE facts SET importance=?1, last_decay_day=?2 WHERE id=?3",
            rusqlite::params![importance, decay_day, id],
        );
        if changed {
            self.write_history(id, "DECAY", Some(&before), Some(&after), "deep-pass", now);
        }
    }

    // ─── forget-by-source (item 4) ───────────────────────────────

    /// Remove every fact (and its history) whose provenance contains a source
    /// token matching `needle` (exact token, or `prefix:` such as `day:` /
    /// `app:` / `frame:`). For privacy this is a HARD delete — unlike decay it
    /// leaves no preimage. Returns the number of facts removed.
    pub fn forget_by_source(&self, needle: &str, now: i64) -> usize {
        let _ = now;
        let all = self.all_facts(true);
        let mut removed = 0usize;
        for f in &all {
            if source_matches(&f.source_refs, needle) {
                let _ = self.conn.execute("DELETE FROM fact_history WHERE fact_id = ?1", [&f.id]);
                let _ = self.conn.execute("DELETE FROM facts WHERE id = ?1", [&f.id]);
                removed += 1;
            }
        }
        if removed > 0 {
            self.vacuum();
        }
        removed
    }

    /// Hard-prune the oldest tombstoned facts and their history (reclaim bytes
    /// for the budget). Returns the number pruned.
    pub fn prune_tombstoned(&self, max_prune: usize) -> usize {
        let ids: Vec<String> = self
            .conn
            .prepare("SELECT id FROM facts WHERE tombstoned = 1 ORDER BY updated_at ASC LIMIT ?1")
            .and_then(|mut s| {
                let rows = s.query_map([max_prune as i64], |r| r.get::<_, String>(0))?;
                Ok(rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
            })
            .unwrap_or_default();
        for id in &ids {
            let _ = self.conn.execute("DELETE FROM fact_history WHERE fact_id = ?1", [id]);
            let _ = self.conn.execute("DELETE FROM facts WHERE id = ?1", [id]);
        }
        ids.len()
    }

    pub fn vacuum(&self) {
        let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;");
    }

    /// On-disk size of the store (db file + WAL/SHM sidecars). 0 for in-memory.
    pub fn disk_bytes(&self) -> u64 {
        let Some(path) = &self.path else { return 0 };
        let mut total = 0u64;
        for suffix in ["", "-wal", "-shm"] {
            let mut p = path.as_os_str().to_owned();
            p.push(suffix);
            if let Ok(meta) = std::fs::metadata(PathBuf::from(p)) {
                total += meta.len();
            }
        }
        total
    }
}

/// Union two source-ref lists, de-duplicated, order-stable, capped so a single
/// long-lived fact cannot accumulate unbounded provenance.
pub fn merge_sources(existing: &[String], add: &[String]) -> Vec<String> {
    const MAX_SOURCES: usize = 32;
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for s in existing.iter().chain(add.iter()) {
        if s.is_empty() {
            continue;
        }
        if seen.insert(s.as_str()) {
            out.push(s.clone());
        }
    }
    if out.len() > MAX_SOURCES {
        // Keep the most recent (tail) tokens.
        out = out.split_off(out.len() - MAX_SOURCES);
    }
    out
}

/// Does any source ref match `needle`? Matches a whole token exactly, or, when
/// `needle` ends in `:`, any token with that prefix (so `day:` forgets all days).
fn source_matches(refs: &[String], needle: &str) -> bool {
    refs.iter().any(|r| {
        r == needle
            || (needle.ends_with(':') && r.starts_with(needle))
            || (!needle.contains(':') && r.split(':').nth(1).map(|v| v == needle).unwrap_or(false))
    })
}

/// Today's local date, for decay watermarks.
pub fn today() -> String {
    timeutil::today()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_and_jaccard() {
        assert_eq!(normalize("Role: Senior  PM!!"), "role senior pm");
        let a = tokens("works at Acme as PM");
        let b = tokens("works at Acme as a PM");
        assert!(jaccard(&a, &b) > 0.7);
    }

    #[test]
    fn fid_is_23_lowercase_alnum_and_unique() {
        let a = fid();
        let b = fid();
        assert_eq!(a.len(), 23);
        assert!(a.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
        assert_ne!(a, b);
    }

    #[test]
    fn add_update_tombstone_writes_history() {
        let s = MemoryStore::open_in_memory().unwrap();
        let now = 1_000;
        let c = Candidate::new(Kind::Person, "sarah-kim", "PM at Acme").with_source("day:2026-10-23");
        let id = s.add_fact(&c, now);
        assert_eq!(s.count_live(), 1);
        assert_eq!(s.history_count(&id), 1);

        s.update_fact(&id, "Senior PM at Acme, leads the memory team", &["frame:5".into()], now + 1);
        let f = s.get_fact(&id).unwrap();
        assert!(f.text.contains("Senior"));
        assert!(f.source_refs.contains(&"frame:5".to_string()));
        assert_eq!(s.history_count(&id), 2);

        s.tombstone(&id, "DELETE", "contradiction", now + 2);
        assert_eq!(s.count_live(), 0);
        assert_eq!(s.count_tombstoned(), 1);
        assert_eq!(s.history_count(&id), 3);
        // Preimage recoverable.
        let hist_before = s
            .conn
            .query_row("SELECT before_text FROM fact_history WHERE fact_id=?1 AND op='DELETE'", [&id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .unwrap();
        assert!(hist_before.unwrap().contains("Senior PM"));
    }

    #[test]
    fn forget_by_source_removes_cleanly() {
        let s = MemoryStore::open_in_memory().unwrap();
        let now = 10;
        let a = s.add_fact(&Candidate::new(Kind::Person, "amy", "likes tea").with_source("day:2026-01-01"), now);
        let b = s.add_fact(&Candidate::new(Kind::Person, "bob", "likes coffee").with_source("day:2026-01-02"), now);
        let _ = (&a, &b);
        assert_eq!(s.count_live(), 2);
        let removed = s.forget_by_source("day:2026-01-01", now);
        assert_eq!(removed, 1);
        assert_eq!(s.count_live(), 1);
        // Prefix forget removes the rest.
        assert_eq!(s.forget_by_source("day:", now), 1);
        assert_eq!(s.all_facts(true).len(), 0);
        assert_eq!(s.total_history_count(), 0, "history gone too");
    }
}
