//! Item 5 — bound & verify. Simulates many days of synthetic candidate facts
//! (no real screen data) and asserts the store stays bounded, de-duplicated,
//! supersedes on UPDATE, tombstones on DELETE, forgets cleanly by source, and
//! that the deep pass is idempotent. Also prints a before/after report of fact
//! count and vault/store bytes WITH vs WITHOUT consolidation.
//!
//! Run with:  MINDSCOPE_DISABLE_CLAUDE=1 cargo test --lib memory_simulation -- --nocapture

use super::consolidate::ingest;
use super::maintenance::{deep_pass, enforce_budget, vault_md_bytes};
use super::render;
use super::store::{Candidate, Kind, MemoryStore};

const DAYS: i64 = 45;
const DAY_SECS: i64 = 86_400;
const BASE: i64 = 1_700_000_000;

/// 10 people (p00..p09) + 5 projects (j00..j04). p00..p04 and projects are seen
/// every day (durable); p05..p09 are seen only on days 0..2 (so they go stale
/// and decay); p00 is retracted on the last day; a one-off `zz-secret` fact is
/// tagged with a unique source so forget-by-source can remove it cleanly.
fn gen_day(d: i64) -> Vec<Candidate> {
    let day_src = format!("day:d{:03}", d);
    let mut out = Vec::new();

    let mut person = |slug: &str, text: &str, out: &mut Vec<Candidate>| {
        out.push(Candidate::new(Kind::Person, slug, text).with_source(day_src.clone()));
    };

    // Durable people, seen every day with a repeating core fact.
    for i in 0..5 {
        let slug = format!("p{:02}", i);
        // Core fact repeats daily → should de-dup to ONE live fact (NONE).
        person(&slug, "works at Acme in the platform group", &mut out);
        // Every 10th day a FULLER restatement → should UPDATE in place, not append.
        if d % 10 == 0 && d > 0 {
            person(&slug, "works at Acme in the platform group as a senior staff engineer on memory", &mut out);
        }
    }
    // Stale people: seen only the first three days, then never again → decay.
    if d < 3 {
        for i in 5..10 {
            let slug = format!("p{:02}", i);
            person(&slug, "contractor seen briefly early in the window", &mut out);
        }
    }
    // Durable projects.
    for i in 0..5 {
        let slug = format!("j{:02}", i);
        out.push(Candidate::new(Kind::Project, &slug, "project is active and on track").with_source(day_src.clone()));
    }
    // One-off fact with a UNIQUE source, for the forget-by-source test.
    if d == 0 {
        out.push(
            Candidate::new(Kind::Person, "zz-secret", "mentioned once in a private app")
                .with_source("app:SecretApp"),
        );
    }
    // Retraction on the final day: p00 no longer at Acme → DELETE (tombstone).
    if d == DAYS - 1 {
        person("p00", "no longer works at Acme", &mut out);
    }
    out
}

/// A comparable snapshot of the store's state, for idempotency checks.
fn snapshot(store: &MemoryStore) -> (usize, Vec<(String, String, i64, bool)>) {
    let mut facts: Vec<(String, String, i64, bool)> = store
        .all_facts(true)
        .into_iter()
        .map(|f| (f.id, f.text, f.importance, f.tombstoned))
        .collect();
    facts.sort();
    (store.total_history_count(), facts)
}

#[test]
fn memory_simulation_bounds_dedups_and_is_idempotent() {
    // Isolated temp locations; never the real ~/.mindscope.
    let root = std::env::temp_dir().join(format!("ms-sim-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let with_vault = root.join("with_vault");
    let without_vault = root.join("without_vault");
    std::fs::create_dir_all(&with_vault).unwrap();
    std::fs::create_dir_all(&without_vault).unwrap();

    let with_store = MemoryStore::open(&root.join("with.db")).unwrap();
    let without_store = MemoryStore::open(&root.join("without.db")).unwrap();

    let mut total_candidates = 0usize;
    let mut last_now = BASE;
    let mut last_today = String::new();

    for d in 0..DAYS {
        let now = BASE + d * DAY_SECS;
        let today = format!("d{:03}", d);
        last_now = now;
        last_today = today.clone();
        let cands = gen_day(d);
        total_candidates += cands.len();

        // WITH consolidation: mem0-style reconcile + daily deep pass.
        ingest(&with_store, &cands, now, false);
        deep_pass(&with_store, now, &today);

        // WITHOUT consolidation: append every candidate verbatim (the old
        // "every observation is a new line" behaviour), no dedup, no decay.
        for c in &cands {
            without_store.add_fact(c, now);
        }
    }

    // ── duplicates don't accumulate ──────────────────────────────
    let with_live = with_store.count_live();
    let without_live = without_store.count_live();
    assert!(
        with_live < without_live / 5,
        "consolidation should shrink fact count massively ({with_live} vs {without_live})"
    );
    // p01 is seen every day but is ONE person → exactly one live fact.
    assert_eq!(with_store.facts_for_subject("p01").len(), 1, "daily repeats de-dup to one fact");

    // ── UPDATE supersedes rather than appends ────────────────────
    let p01 = &with_store.facts_for_subject("p01")[0];
    assert!(
        p01.text.contains("senior staff engineer"),
        "the fuller restatement replaced the core fact in place: {:?}",
        p01.text
    );

    // ── DELETE tombstones ────────────────────────────────────────
    assert!(with_store.facts_for_subject("p00").is_empty(), "retracted fact is gone from the live view");
    assert!(with_store.count_tombstoned() > 0, "retraction/decay produced tombstones (recoverable)");

    // ── deep pass is idempotent (running it twice changes nothing) ─
    let snap_a = snapshot(&with_store);
    deep_pass(&with_store, last_now, &last_today);
    let snap_b = snapshot(&with_store);
    deep_pass(&with_store, last_now, &last_today);
    let snap_c = snapshot(&with_store);
    assert_eq!(snap_b, snap_c, "deep pass is idempotent: a second run changes nothing");
    assert_eq!(snap_a, snap_b, "an extra deep pass after the daily run is already a no-op");

    // ── render both to measure vault bytes, then report ──────────
    render::render_cold(&with_store, &with_vault);
    render::render_cold(&without_store, &without_vault);
    with_store.vacuum();
    without_store.vacuum();
    let with_total = with_store.disk_bytes() + vault_md_bytes(&with_vault);
    let without_total = without_store.disk_bytes() + vault_md_bytes(&without_vault);

    println!("\n──────── MindScope Synapse memory: {DAYS}-day simulation ────────");
    println!("  candidate facts fed:        {total_candidates}");
    println!("  WITHOUT consolidation: {without_live:>5} live facts, {:>7.1} KB (store+vault)", without_total as f64 / 1024.0);
    println!("  WITH    consolidation: {with_live:>5} live facts, {:>7.1} KB (store+vault)", with_total as f64 / 1024.0);
    println!(
        "  reduction:                  {:.1}x fewer facts, {:.1}x smaller",
        without_live as f64 / with_live.max(1) as f64,
        without_total as f64 / with_total.max(1) as f64
    );

    // ── forget-by-source removes cleanly ─────────────────────────
    let removed = with_store.forget_by_source("app:SecretApp", last_now);
    assert_eq!(removed, 1, "the one-off private fact is forgotten");
    assert!(
        with_store.all_facts(true).iter().all(|f| !f.source_refs.iter().any(|s| s == "app:SecretApp")),
        "no trace of the forgotten source remains (facts or history)"
    );

    // ── hard byte ceiling holds ──────────────────────────────────
    // The achievable floor is an empty vacuumed store (schema + indexes); the
    // ceiling must sit above it to be satisfiable. Measure that floor, then pick
    // a ceiling between it and the current footprint so the budget must shed
    // facts yet can still come under the cap.
    let floor = {
        let empty = MemoryStore::open(&root.join("floor.db")).unwrap();
        empty.vacuum();
        empty.disk_bytes()
    };
    let before = with_store.disk_bytes() + vault_md_bytes(&with_vault);
    assert!(before > floor, "footprint {before} exceeds the empty-store floor {floor}");
    let ceiling = (floor + before) / 2;
    let report = enforce_budget(&with_store, &with_vault, ceiling, last_now);
    let after = with_store.disk_bytes() + vault_md_bytes(&with_vault);
    assert!(after <= ceiling, "store+vault under the ceiling: {after} <= {ceiling} (report: {report:?})");
    assert!(report.facts_dropped > 0, "the ceiling actually shed facts");
    println!(
        "  byte ceiling:               {:.1} KB ceiling enforced → {:.1} KB (dropped {} facts)\n",
        ceiling as f64 / 1024.0,
        after as f64 / 1024.0,
        report.facts_dropped
    );

    let _ = std::fs::remove_dir_all(&root);
}
