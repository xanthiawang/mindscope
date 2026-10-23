//! Compression, decay and the "dreaming" deep pass.
//!
//! Borrowed from OpenClaw's memory design: a light pass handles fresh
//! candidates (see `consolidate`), while a heavier periodic "deep" pass does
//! merge/dedup and decays unreferenced facts. Only the deep pass rewrites the
//! long-term (cold) files, and every change keeps a before/after preimage in
//! `fact_history`, so a bad pass is recoverable.

use std::collections::HashSet;
use std::path::Path;

use super::store::{jaccard, tokens, Fact, MemoryStore};

/// A fact unreferenced for this many days starts losing importance.
pub const DECAY_AFTER_DAYS: i64 = 14;
/// Importance lost per deep pass once a fact is stale.
pub const DECAY_STEP: i64 = 10;
/// Near-duplicate threshold for the dedup phase (kept in sync with consolidate).
const DEDUP_THRESHOLD: f64 = 0.85;

#[derive(Debug, Default, Clone)]
pub struct DeepReport {
    pub merged: usize,
    pub decayed: usize,
    pub archived: usize,
    pub subjects: usize,
}

/// Run the deep "dreaming" pass against the store: merge near-duplicates,
/// decay stale facts, archive the fully-decayed. Idempotent within a local day
/// — running it twice in a row makes no further change (dedup finds nothing on
/// the second run, and each fact's decay watermark guards same-day re-decay).
///
/// `now` is unix seconds; `today` is the local `YYYY-MM-DD` used as the decay
/// watermark. Returns a tally. Rendering of cold files is done separately by
/// the caller (see `render`), so this function is pure store maintenance and
/// easy to test.
pub fn deep_pass(store: &MemoryStore, now: i64, today: &str) -> DeepReport {
    let mut report = DeepReport::default();

    // ── 1. Merge/dedup per subject ──────────────────────────────
    let subjects: Vec<String> = {
        let mut s: Vec<String> = store.live_subjects().into_iter().map(|(_, s)| s).collect();
        s.dedup();
        s
    };
    report.subjects = subjects.len();

    for subject in &subjects {
        let facts = store.facts_for_subject(subject);
        let mut consumed: HashSet<String> = HashSet::new();
        for i in 0..facts.len() {
            if consumed.contains(&facts[i].id) {
                continue;
            }
            // Gather this fact's near-duplicate cluster.
            let base_tokens = tokens(&facts[i].text);
            let mut cluster: Vec<&Fact> = vec![&facts[i]];
            for j in (i + 1)..facts.len() {
                if consumed.contains(&facts[j].id) {
                    continue;
                }
                if jaccard(&base_tokens, &tokens(&facts[j].text)) >= DEDUP_THRESHOLD {
                    cluster.push(&facts[j]);
                }
            }
            if cluster.len() < 2 {
                continue;
            }
            // Canonical = longest text (most information); merge the rest in.
            cluster.sort_by(|a, b| {
                b.text.chars().count().cmp(&a.text.chars().count()).then(a.created_at.cmp(&b.created_at))
            });
            let canonical = cluster[0];
            let mut sources: Vec<String> = Vec::new();
            let mut importance = canonical.importance;
            let mut last_seen = canonical.last_seen;
            for f in &cluster {
                sources.extend(f.source_refs.iter().cloned());
                importance = importance.max(f.importance);
                last_seen = last_seen.max(f.last_seen);
            }
            store.merge_canonical(&canonical.id, &canonical.text, &sources, importance, last_seen, now);
            consumed.insert(canonical.id.clone());
            for f in cluster.iter().skip(1) {
                store.tombstone(&f.id, "MERGE", "deep-pass", now);
                consumed.insert(f.id.clone());
                report.merged += 1;
            }
        }
    }

    // ── 2. Decay unreferenced facts (once per local day per fact) ──
    for f in store.all_facts(false) {
        if f.last_decay_day == today {
            continue; // already decayed today → idempotent
        }
        let idle_days = (now - f.last_seen) / 86_400;
        if idle_days > DECAY_AFTER_DAYS {
            let new_imp = f.importance - DECAY_STEP;
            if new_imp <= 0 {
                // Fully decayed → archive (tombstone, recoverable), not deleted.
                store.tombstone(&f.id, "DECAY", "archived", now);
                report.archived += 1;
            } else {
                store.set_importance(&f.id, new_imp, today, now);
                report.decayed += 1;
            }
        } else {
            // Still fresh — just stamp the watermark (no history row).
            store.set_importance(&f.id, f.importance, today, now);
        }
    }

    report
}

#[derive(Debug, Default, Clone)]
pub struct BudgetReport {
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub facts_dropped: usize,
    pub facts_pruned: usize,
    pub over_budget: bool,
}

/// Total bytes of the markdown vault files (the rendered view) under `vault`.
pub fn vault_md_bytes(vault: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(vault) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".md") {
                if let Ok(meta) = e.metadata() {
                    total += meta.len();
                }
            }
        }
    }
    total
}

/// Enforce a hard ceiling on the combined store + vault bytes, with the same
/// discipline as the data-layer retention budget: shed the lowest-value facts
/// first (tombstone), then hard-prune tombstoned rows and VACUUM to reclaim
/// space, until under the ceiling or nothing droppable remains. Never touches
/// files outside the vault.
pub fn enforce_budget(store: &MemoryStore, vault: &Path, ceiling_bytes: u64, now: i64) -> BudgetReport {
    let mut report = BudgetReport::default();
    if ceiling_bytes == 0 {
        return report;
    }
    store.vacuum();
    report.bytes_before = store.disk_bytes() + vault_md_bytes(vault);

    let mut iter = 0;
    loop {
        let total = store.disk_bytes() + vault_md_bytes(vault);
        if total <= ceiling_bytes || iter > 10_000 {
            report.over_budget = total > ceiling_bytes;
            break;
        }
        iter += 1;

        // First reclaim bytes already-tombstoned facts are holding.
        let pruned = store.prune_tombstoned(64);
        if pruned > 0 {
            report.facts_pruned += pruned;
            store.vacuum();
            continue;
        }
        // Then shed the lowest-value live facts (they prune next iteration).
        let victims = store.lowest_value_facts(64);
        if victims.is_empty() {
            report.over_budget = total > ceiling_bytes;
            break; // nothing left to drop
        }
        for id in victims {
            store.tombstone(&id, "DECAY", "budget", now);
            report.facts_dropped += 1;
        }
    }

    store.vacuum();
    report.bytes_after = store.disk_bytes() + vault_md_bytes(vault);
    report
}

#[cfg(test)]
mod tests {
    use super::super::store::{Candidate, Kind, MemoryStore};
    use super::*;

    #[test]
    fn deep_pass_dedups_and_is_idempotent() {
        let s = MemoryStore::open_in_memory().unwrap();
        let now = 1_000_000;
        // Three near-identical facts (would normally be caught at ingest, but
        // can arrive via migration or different subjects of the same import).
        for _ in 0..3 {
            s.add_fact(&Candidate::new(Kind::Person, "p", "works at Acme as a PM").with_source("day:a"), now);
        }
        assert_eq!(s.count_live(), 3);
        let r1 = deep_pass(&s, now, "2026-10-23");
        assert_eq!(r1.merged, 2, "two dupes merged into one");
        assert_eq!(s.count_live(), 1);

        // Snapshot store state, run again, assert nothing changed.
        let hist = s.total_history_count();
        let live: Vec<_> = s.all_facts(false).iter().map(|f| (f.id.clone(), f.text.clone(), f.importance)).collect();
        let r2 = deep_pass(&s, now, "2026-10-23");
        assert_eq!(r2.merged, 0);
        assert_eq!(r2.decayed, 0);
        assert_eq!(s.total_history_count(), hist, "idempotent: no new history");
        let live2: Vec<_> = s.all_facts(false).iter().map(|f| (f.id.clone(), f.text.clone(), f.importance)).collect();
        assert_eq!(live, live2, "idempotent: live facts unchanged");
    }

    #[test]
    fn stale_facts_decay_then_archive() {
        let s = MemoryStore::open_in_memory().unwrap();
        let day = 86_400;
        let created = 0i64;
        let id = s.add_fact(&Candidate::new(Kind::Person, "p", "old fact").with_source("day:x"), created);
        // Importance 50; not seen for 20 days → decays by 10 each distinct day.
        let now = 20 * day;
        for d in 0..5 {
            let today = format!("2026-11-{:02}", d + 1);
            deep_pass(&s, now + d * day, &today);
        }
        // 50 → 40 → 30 → 20 → 10 → archived on the 5th.
        assert!(s.get_fact(&id).map(|f| f.tombstoned).unwrap_or(false), "fully decayed → archived");
        assert_eq!(s.count_live(), 0);
        assert_eq!(s.count_tombstoned(), 1, "archived, not deleted");
    }
}
