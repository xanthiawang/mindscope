//! mem0-style consolidation: reconcile each candidate fact against the existing
//! facts for its subject with an explicit ADD / UPDATE / DELETE / NONE decision.
//!
//! Deterministic rules decide the common cases (exact/near-duplicate → NONE,
//! superset → UPDATE, retraction/contradiction → DELETE, else ADD). Only when
//! the rules are genuinely unsure — a moderate-overlap neighbour that is
//! neither a clear duplicate, superset, nor contradiction — is the Claude CLI
//! asked to adjudicate, and ANY unusable model output falls back to the rules.
//!
//! Shape follows mem0's `DEFAULT_UPDATE_MEMORY_PROMPT` (ADD/UPDATE/DELETE/NONE
//! keyed by existing memory id). Re-implemented here; no mem0 code is copied.

use std::collections::HashSet;

use super::super::claude_cli;
use super::store::{jaccard, normalize, tokens, Candidate, Fact, Kind, MemoryStore};

/// The reconciliation verdict for one candidate against the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Already known — refresh last_seen on the matched fact, store nothing new.
    None_ { matched: String },
    /// Genuinely new fact.
    Add,
    /// Rewrite an existing fact (by id) to this fuller/newer form.
    Update(String),
    /// An existing fact (by id) is contradicted/retracted; tombstone it.
    /// When `and_add` is true, the candidate is a replacement and is also added.
    Delete { id: String, and_add: bool },
}

/// Thresholds. Tuned to be conservative: when unsure, prefer leaving existing
/// content in place (mirrors the CLAUDE.md "confidence < 0.8 → keep" rule).
const DUP_THRESHOLD: f64 = 0.85;
const UNSURE_LOW: f64 = 0.4;
/// Min overlap-coefficient of a retraction's positive tokens against a fact for
/// it to count as retracting that fact.
const RETRACT_THRESHOLD: f64 = 0.6;

/// Overlap coefficient: |a ∩ b| / min(|a|, |b|). Robust when one set is much
/// smaller than the other (a short retraction vs a long fact).
fn overlap_coeff(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    let min = a.len().min(b.len());
    if min == 0 {
        return 0.0;
    }
    a.intersection(b).count() as f64 / min as f64
}

const NEGATIONS: &[&str] = &[
    "no longer", "not ", "never ", "cancelled", "canceled", "stopped", "left ",
    "quit", "resigned", "deprecated", "removed", "declined", "abandoned", "dropped",
];

fn is_retraction(text: &str) -> bool {
    let n = normalize(text);
    let padded = format!(" {n} ");
    NEGATIONS.iter().any(|m| padded.contains(&normalize(m)) || n.starts_with(&normalize(m)))
}

/// Tokens with the negation markers removed, so a retraction still matches the
/// fact it retracts ("no longer at Acme" ~ "works at Acme").
fn positive_tokens(text: &str) -> HashSet<String> {
    let neg: HashSet<String> = NEGATIONS
        .iter()
        .flat_map(|m| normalize(m).split(' ').map(|s| s.to_string()).collect::<Vec<_>>())
        .collect();
    tokens(text).into_iter().filter(|t| !neg.contains(t)).collect()
}

/// A leading `key: value`, `key is value`, or `key = value` attribute, if any.
fn attribute(text: &str) -> Option<(String, String)> {
    for sep in [":", " is ", "="] {
        if let Some(idx) = text.find(sep) {
            let key = normalize(&text[..idx]);
            let val = normalize(&text[idx + sep.len()..]);
            if !key.is_empty() && !val.is_empty() && key.split(' ').count() <= 3 {
                return Some((key, val));
            }
        }
    }
    None
}

/// Pure, deterministic reconciliation. Returns the decision plus whether the
/// case is "unsure" (so the caller may consult Claude).
pub fn reconcile_rules(cand: &Candidate, existing: &[Fact]) -> (Decision, bool) {
    let cand_tokens = tokens(&cand.text);
    let cand_norm = normalize(&cand.text);
    let cand_attr = attribute(&cand.text);
    let retraction = is_retraction(&cand.text);
    let cand_pos = positive_tokens(&cand.text);

    let mut best_sim = 0.0f64;
    let mut best_id: Option<String> = None;
    let mut exact: Option<String> = None;
    let mut superset_of: Option<String> = None; // candidate is fuller than this fact
    let mut subset_of = false; // existing already covers candidate
    let mut attr_conflict: Option<String> = None;
    let mut retract_target: Option<String> = None;
    let mut retract_sim = 0.0f64;

    for f in existing {
        let f_tokens = tokens(&f.text);
        let f_norm = normalize(&f.text);
        if f_norm == cand_norm {
            exact = Some(f.id.clone());
        }
        let sim = jaccard(&cand_tokens, &f_tokens);
        if sim > best_sim {
            best_sim = sim;
            best_id = Some(f.id.clone());
        }
        // Superset / subset by token containment.
        if cand_tokens.len() > f_tokens.len() && f_tokens.is_subset(&cand_tokens) && !f_tokens.is_empty() {
            superset_of = Some(f.id.clone());
        }
        if !cand_tokens.is_empty() && cand_tokens.is_subset(&f_tokens) && f_tokens.len() >= cand_tokens.len() {
            subset_of = true;
        }
        // Attribute conflict: same key, different value.
        if let (Some((ck, cv)), Some((fk, fv))) = (&cand_attr, attribute(&f.text)) {
            if *ck == fk && *cv != fv {
                attr_conflict = Some(f.id.clone());
            }
        }
        // Retraction target: a fact whose positive content the retraction
        // negates. Use the overlap coefficient (intersection / smaller set) so a
        // short retraction still matches a longer, fuller fact it refers to.
        if retraction {
            let f_pos = positive_tokens(&f.text);
            let sim_pos = overlap_coeff(&cand_pos, &f_pos);
            if sim_pos > retract_sim && sim_pos >= RETRACT_THRESHOLD {
                retract_sim = sim_pos;
                retract_target = Some(f.id.clone());
            }
        }
    }

    // 1. Exact duplicate → NONE.
    if let Some(id) = exact {
        return (Decision::None_ { matched: id }, false);
    }
    // 2. Retraction of an existing fact → DELETE (no replacement fact added).
    if retraction {
        if let Some(id) = retract_target {
            return (Decision::Delete { id, and_add: false }, false);
        }
    }
    // 3. Near-duplicate (very high overlap, no new tokens) → NONE.
    if best_sim >= DUP_THRESHOLD && superset_of.is_none() {
        if let Some(id) = best_id.clone() {
            return (Decision::None_ { matched: id }, false);
        }
    }
    // 4. Candidate is a strict superset of an existing fact → UPDATE it.
    if let Some(id) = superset_of {
        return (Decision::Update(id), false);
    }
    // 5. Attribute conflict (same key, new value) → UPDATE the stale fact.
    if let Some(id) = attr_conflict {
        return (Decision::Update(id), false);
    }
    // 6. Existing already covers the candidate → NONE.
    if subset_of {
        if let Some(id) = best_id.clone() {
            return (Decision::None_ { matched: id }, false);
        }
    }
    // 7. Moderate overlap with some neighbour but no clear rule → UNSURE.
    if best_sim >= UNSURE_LOW && best_sim < DUP_THRESHOLD {
        return (Decision::Add, true);
    }
    // 8. No meaningful neighbour → ADD.
    (Decision::Add, false)
}

/// Ask Claude to adjudicate an unsure candidate. Returns a decision only if the
/// model produces usable output; otherwise None (caller keeps the rule result).
fn reconcile_with_claude(cand: &Candidate, existing: &[Fact]) -> Option<Decision> {
    if !claude_cli::available() {
        return None;
    }
    let mut listing = String::new();
    for f in existing {
        listing.push_str(&format!("- id={} text={:?}\n", f.id, f.text));
    }
    let prompt = format!(
        "You reconcile a new memory against existing memories about the same subject.\n\
         Reply with ONE line of JSON and nothing else:\n\
         {{\"event\":\"ADD|UPDATE|DELETE|NONE\",\"id\":\"<existing id or empty>\"}}\n\
         ADD = genuinely new. UPDATE = rewrite the given existing id to the new text.\n\
         DELETE = the given existing id is now contradicted. NONE = already known.\n\n\
         EXISTING:\n{}\nNEW: {:?}\n",
        listing, cand.text
    );
    let out = claude_cli::run_background("memory-consolidate", &prompt).ok()?;
    let line = out.lines().find(|l| l.contains('{'))?;
    let start = line.find('{')?;
    let end = line.rfind('}')? + 1;
    let v: serde_json::Value = serde_json::from_str(&line[start..end]).ok()?;
    let event = v.get("event")?.as_str()?.to_uppercase();
    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let valid = |id: &str| existing.iter().any(|f| f.id == id);
    match event.as_str() {
        "ADD" => Some(Decision::Add),
        "NONE" => existing
            .first()
            .map(|f| Decision::None_ { matched: f.id.clone() })
            .or(Some(Decision::Add)),
        "UPDATE" if valid(&id) => Some(Decision::Update(id)),
        "DELETE" if valid(&id) => Some(Decision::Delete { id, and_add: false }),
        _ => None, // unusable → fall back to rules
    }
}

/// Reconcile one candidate: rules first, Claude only when unsure.
pub fn reconcile(cand: &Candidate, existing: &[Fact], use_claude: bool) -> Decision {
    let (rule_decision, unsure) = reconcile_rules(cand, existing);
    if unsure && use_claude {
        if let Some(d) = reconcile_with_claude(cand, existing) {
            return d;
        }
    }
    rule_decision
}

#[derive(Debug, Default, Clone)]
pub struct IngestReport {
    pub added: usize,
    pub updated: usize,
    pub deleted: usize,
    pub noop: usize,
}

impl IngestReport {
    pub fn total(&self) -> usize {
        self.added + self.updated + self.deleted + self.noop
    }
}

/// Reconcile and apply a batch of candidate facts. `use_claude` lets the unsure
/// cases consult the CLI (always false in tests / when disabled). Returns a
/// tally of the decisions taken.
pub fn ingest(store: &MemoryStore, candidates: &[Candidate], now: i64, use_claude: bool) -> IngestReport {
    let mut report = IngestReport::default();
    for c in candidates {
        if c.text.trim().is_empty() || c.subject.trim().is_empty() {
            continue;
        }
        let existing = store.facts_for_subject(&c.subject);
        match reconcile(c, &existing, use_claude) {
            Decision::None_ { matched } => {
                store.touch_fact(&matched, &c.source_refs, now);
                report.noop += 1;
            }
            Decision::Add => {
                store.add_fact(c, now);
                report.added += 1;
            }
            Decision::Update(id) => {
                store.update_fact(&id, &c.text, &c.source_refs, now);
                report.updated += 1;
            }
            Decision::Delete { id, and_add } => {
                store.tombstone(&id, "DELETE", "contradiction", now);
                report.deleted += 1;
                if and_add {
                    store.add_fact(c, now);
                    report.added += 1;
                }
            }
        }
    }
    report
}

/// Convenience used by the live loop: ingest against the default store kind.
pub fn candidate(kind: Kind, subject: &str, text: &str, sources: &[String]) -> Candidate {
    let mut c = Candidate::new(kind, subject, text);
    c.source_refs = sources.to_vec();
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(text: &str) -> Candidate {
        Candidate::new(Kind::Person, "p", text).with_source("day:2026-10-23")
    }

    #[test]
    fn rules_cover_the_four_events() {
        let s = MemoryStore::open_in_memory().unwrap();
        // ADD
        let r = ingest(&s, &[mk("works at Acme as a PM")], 1, false);
        assert_eq!(r.added, 1);
        // NONE (near-duplicate)
        let r = ingest(&s, &[mk("works at Acme as PM")], 2, false);
        assert_eq!(r.noop, 1, "near-duplicate is NONE");
        assert_eq!(s.count_live(), 1);
        // UPDATE (superset)
        let r = ingest(&s, &[mk("works at Acme as a PM leading the memory team")], 3, false);
        assert_eq!(r.updated, 1, "superset is UPDATE");
        assert_eq!(s.count_live(), 1, "update supersedes, not append");
        // DELETE (retraction)
        let r = ingest(&s, &[mk("no longer works at Acme")], 4, false);
        assert_eq!(r.deleted, 1, "retraction is DELETE");
        assert_eq!(s.count_live(), 0);
    }

    #[test]
    fn attribute_conflict_updates() {
        let s = MemoryStore::open_in_memory().unwrap();
        ingest(&s, &[mk("role: PM")], 1, false);
        let r = ingest(&s, &[mk("role: Director")], 2, false);
        assert_eq!(r.updated, 1);
        assert_eq!(s.count_live(), 1);
        assert_eq!(s.facts_for_subject("p")[0].text, "role: Director");
    }

    #[test]
    fn distinct_facts_both_add() {
        let s = MemoryStore::open_in_memory().unwrap();
        ingest(&s, &[mk("works at Acme")], 1, false);
        let r = ingest(&s, &[mk("enjoys rock climbing on weekends")], 2, false);
        assert_eq!(r.added, 1, "unrelated fact is a new ADD");
        assert_eq!(s.count_live(), 2);
    }
}
