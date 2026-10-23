//! MindScope Synapse memory: a structured, provenanced, bounded fact store
//! with mem0-style consolidation, decay/compression and a "dreaming" deep pass.
//! The markdown vault is a rendered view of this store.
//!
//! Submodules:
//!   * `store`       — the SQLite-backed fact store + history (identity, provenance)
//!   * `consolidate` — mem0-style ADD/UPDATE/DELETE/NONE reconciliation
//!   * `maintenance` — dedup, decay, the deep pass, and the byte-budget ceiling
//!   * `render`      — vault rendering, one-time migration, hot/warm compression

pub mod consolidate;
pub mod maintenance;
pub mod render;
pub mod store;

use std::path::Path;
use std::sync::Mutex;

use super::claude_cli;
use super::paths;
use super::settings;
use super::timeutil;
use super::util::LockExt;

pub use consolidate::IngestReport;
pub use store::{Candidate, Kind, MemoryStore};

/// Lazily-opened process-wide default store.
static STORE: Mutex<Option<MemoryStore>> = Mutex::new(None);

/// Run `f` against the default store, opening it on first use. Returns None if
/// the store could not be opened.
fn with_store<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&MemoryStore) -> R,
{
    let mut guard = STORE.lock_or_recover();
    if guard.is_none() {
        match MemoryStore::open_default() {
            Ok(s) => *guard = Some(s),
            Err(e) => {
                log::warn!("MindScope memory: could not open store: {}", e);
                return None;
            }
        }
    }
    guard.as_ref().map(f)
}

/// Combined hard ceiling (bytes) for the store file + rendered vault markdown.
pub fn budget_bytes() -> u64 {
    let mb = settings::load_settings().memory_budget_mb;
    mb.saturating_mul(1024 * 1024)
}

/// One-time migration of existing vault markdown into the store. Read-only on
/// disk — safe on first start. Called from `synapse::bootstrap`.
pub fn migrate_if_needed() {
    let vault = paths::vault_dir();
    if !vault.exists() {
        return;
    }
    let now = timeutil::now_secs();
    with_store(|s| {
        let n = render::migrate_vault(s, &vault, now);
        if n > 0 {
            log::info!("MindScope memory: migrated {} fact(s) from existing vault files", n);
        }
    });
}

/// Light consolidation of freshly-extracted candidate facts: reconcile each
/// against the store (rules first; Claude only for unsure cases, when
/// available). Renders nothing cold — that is the deep pass's job.
pub fn consolidate_candidates(candidates: &[Candidate]) -> IngestReport {
    let now = timeutil::now_secs();
    let use_claude = claude_cli::available();
    with_store(|s| consolidate::ingest(s, candidates, now, use_claude)).unwrap_or_default()
}

/// The deep "dreaming" pass: dedup + decay in the store, then re-render the
/// cold files (store-owned only) and enforce the byte ceiling. This is the only
/// place that rewrites long-term files, and every change keeps a preimage.
pub fn deep_pass() {
    let vault = paths::vault_dir();
    let now = timeutil::now_secs();
    let today = store::today();
    with_store(|s| {
        let dr = maintenance::deep_pass(s, now, &today);
        let rr = render::render_cold(s, &vault);
        let br = maintenance::enforce_budget(s, &vault, budget_bytes(), now);
        compress_hot_warm(&vault);
        log::info!(
            "MindScope memory deep pass: {} subjects, {} merged, {} decayed, {} archived; \
             rendered {} cold file(s) ({} user-owned skipped); store+vault {:.1} KB (dropped {}, pruned {})",
            dr.subjects, dr.merged, dr.decayed, dr.archived,
            rr.written, rr.skipped_user_owned,
            br.bytes_after as f64 / 1024.0, br.facts_dropped, br.facts_pruned
        );
    });
}

/// Forget-by-source: remove every fact (and its history) derived from one
/// source token (`frame:ID`, `day:DATE`, `app:NAME`, or a `day:` / `app:`
/// prefix). Hard delete for privacy. Re-renders affected cold files.
pub fn forget_source(source: &str) -> usize {
    let vault = paths::vault_dir();
    let now = timeutil::now_secs();
    let removed = with_store(|s| {
        let n = s.forget_by_source(source, now);
        if n > 0 {
            // Re-render so the vault no longer shows forgotten facts.
            render::render_cold(s, &vault);
        }
        n
    })
    .unwrap_or(0);
    log::info!("MindScope memory: forget source {:?} removed {} fact(s)", source, removed);
    removed
}

/// Snapshot of the store size for diagnostics / the budget report.
pub fn stats() -> (usize, usize, u64) {
    with_store(|s| {
        let vault = paths::vault_dir();
        (s.count_live(), s.count_tombstoned(), s.disk_bytes() + maintenance::vault_md_bytes(&vault))
    })
    .unwrap_or((0, 0, 0))
}

// ─── hot/warm compression wiring (item 3) ────────────────────────

const WORKING_MEMORY_HARD_CAP: usize = 4000;
const WARM_MEMORY_HARD_CAP: usize = 8000;

/// Gracefully compress the hot and warm tiers by dropping whole oldest entries
/// (never cutting mid-item). Replaces the old `chars().take()` hard-cut.
pub fn compress_hot_warm(vault: &Path) {
    compress_file(&vault.join("_working-memory.md"), WORKING_MEMORY_HARD_CAP, &render::working_memory_caps());
    compress_file(&vault.join("_warm-memory.md"), WARM_MEMORY_HARD_CAP, &render::warm_memory_caps());
}

fn compress_file(path: &Path, cap: usize, caps: &[render::SectionCap]) {
    let Ok(content) = std::fs::read_to_string(path) else { return };
    if content.chars().count() <= cap {
        return;
    }
    let compressed = render::compress_managed(&content, cap, caps);
    if compressed != content {
        if let Err(e) = std::fs::write(path, &compressed) {
            log::warn!("MindScope memory: failed to compress {:?}: {}", path, e);
        } else {
            log::info!(
                "MindScope memory: compressed {:?} {} → {} chars (entry-aware)",
                path.file_name().unwrap_or_default(),
                content.chars().count(),
                compressed.chars().count()
            );
        }
    }
}

// ─── candidate extraction helper (used by synapse.rs) ────────────

/// Parse a JSON array of candidate facts as produced by the Synapse extraction
/// prompt. Shape: `[{"kind":"person","subject":"sarah-kim","text":"..."}]`.
/// Returns only well-formed entries; a malformed blob yields an empty vec (the
/// caller then simply ingests nothing, never crashes).
pub fn parse_candidates_json(json: &str, sources: &[String]) -> Vec<Candidate> {
    // Tolerate models that wrap the array in prose or a ```json fence.
    let slice = match (json.find('['), json.rfind(']')) {
        (Some(a), Some(b)) if b > a => &json[a..=b],
        _ => return Vec::new(),
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(slice) else { return Vec::new() };
    let Some(arr) = value.as_array() else { return Vec::new() };
    let mut out = Vec::new();
    for item in arr {
        let kind = item.get("kind").and_then(|v| v.as_str()).unwrap_or("note");
        let subject = item.get("subject").and_then(|v| v.as_str()).unwrap_or("").trim();
        let text = item.get("text").and_then(|v| v.as_str()).unwrap_or("").trim();
        if subject.is_empty() || text.is_empty() {
            continue;
        }
        let subject_slug = slugify(subject);
        if subject_slug.is_empty() {
            continue;
        }
        let mut c = Candidate::new(Kind::from_str(kind), subject_slug, text);
        c.source_refs = sources.to_vec();
        c.importance = item.get("importance").and_then(|v| v.as_i64());
        out.push(c);
    }
    out
}

/// Lowercase, hyphenate a subject name into a vault-safe slug.
fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = true;
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            for c in ch.to_lowercase() {
                out.push(c);
            }
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_candidate_json_tolerantly() {
        let blob = "Here you go:\n```json\n[{\"kind\":\"person\",\"subject\":\"Sarah Kim\",\"text\":\"PM at Acme\"},\
                    {\"kind\":\"project\",\"subject\":\"mindscope\",\"text\":\"memory layer rewrite\",\"importance\":70}]\n```";
        let cands = parse_candidates_json(blob, &["day:2026-10-23".into()]);
        assert_eq!(cands.len(), 2);
        assert_eq!(cands[0].subject, "sarah-kim");
        assert_eq!(cands[0].kind, Kind::Person);
        assert_eq!(cands[1].importance, Some(70));
        assert_eq!(cands[1].source_refs, vec!["day:2026-10-23".to_string()]);
    }

    #[test]
    fn malformed_json_yields_nothing() {
        assert!(parse_candidates_json("not json at all", &[]).is_empty());
        assert!(parse_candidates_json("[{\"kind\":\"person\"}]", &[]).is_empty(), "missing subject/text");
    }
}
