//! Brain v2 L2.1 — the MEMORY CONSOLIDATION / REFLECTION job (the generative-agents recipe,
//! deterministic where it counts).
//!
//! An hourly background pass (spawned from `lib.rs` setup, mirroring the topic-chunk backfill
//! block) that:
//!   1. SCORES every open, VISIBLE user fact into `memory_scores` — deterministic
//!      recency ([`compute_recency`], `0.995^hours`) blended with a batch-assessed importance
//!      ([`composite_score`], weights 0.4/0.4/0.2). Importance is assigned in THIS job by the
//!      LIGHT on-device reasoner (never at fact-write time — spec decision #7: zero pipeline
//!      latency), defaulting to [`DEFAULT_IMPORTANCE`] when the reasoner is the stub or the reply
//!      is unparseable. Steady-state passes are LLM-free: only never-scored facts are assessed.
//!   2. REFLECTS: entity groups with ≥ [`ROLLUP_MIN_FACTS`] open visible facts OR any fact with
//!      assessed importance ≥ [`IMPORTANT_FACT_MIN`] get a light-reasoner synthesis
//!      (≤ [`REFLECTION_MAX_TOKENS`] tokens, wall-clock-bounded) upserted into `memory_rollups`
//!      under scope `entity:<id>`; once per ISO week a `weekly:<YYYY-WNN>` rollup synthesizes the
//!      user's own memory. EVERY existing rollup is re-checked each pass against its scope's
//!      current visible fact set (`fact_set_hash`): ineligible ⇒ deleted (row + exported `.md`),
//!      changed ⇒ re-reflected (weekly included). THE STUB NEVER WRITES A ROLLUP — stub "text" is
//!      a debug echo, and rollups are exported to the user's vault, so a stub pass scores facts
//!      (with the default importance) and produces ZERO rollups (tested); stub GC deletion still
//!      runs (needs no LLM).
//!   3. EXPORTS each un-exported rollup as an atomic `.md` under `<vault>/brain/memory/`
//!      (frontmatter + the synthesis body, via the existing atomic overwrite helper).
//!
//! ## Lock model (the part the lock-security review audits)
//! * Every fact read is GATED: the job reads through `list_user_facts_visible` /
//!   `list_facts_visible` with the EMPTY unlock set — sealed-and-not-session-unlocked meetings are
//!   excluded BY DESIGN (a background job must never see session-unlocked plaintext, let alone
//!   sealed content).
//! * `memory_scores` rows are CONTENT-FREE (fact ids + floats) and cascade off `user_facts`
//!   (FK `ON DELETE CASCADE`), so the purge-on-seal / delete-meeting paths drop them transitively.
//! * `memory_rollups` are CROSS-MEETING SYNTHESIS with no single source meeting, so they get TWO
//!   protections: (1) EVERY seal path (`lock_folder` chain, relock, startup reconcile,
//!   `delete_meeting`) purges ALL rollup rows inside the seal transaction
//!   (`Db::purge_memory_rollups_tx`) and deletes their exported vault `.md`s — rollups are cheap
//!   re-derivable synthesis that regenerates on the next hourly pass FROM VISIBLE FACTS ONLY;
//!   (2) every pass GC's/REGENERATES each existing rollup against the CURRENT visible fact set
//!   (`fact_set_hash`): a no-longer-eligible scope is DELETED (row + exported file), a changed set
//!   is re-reflected + re-exported (weekly scopes included), so superseded/forgotten facts age out
//!   even without a seal.
//! * NO PII in logs: ids, scopes, counts, durations only.
//!
//! ## Egress
//! ZERO. The job only ever calls the LIGHT engine handle (`ReasonerCell::light` — local-or-stub,
//! NEVER a cloud fallback), so fact content never leaves the device from this path.
//!
//! ## Failure posture
//! The hourly loop never exits: any per-pass / per-entity error is `tracing::warn` + continue.
//! The DB lock is NEVER held across an LLM call — every read/write goes through short `Db`
//! methods; the reasoner runs between them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::Result;
use crate::facts::Fact;
use crate::reason::{GenOptions, LocalReasoner};
use crate::storage::Db;

/// Composite weight — recency component (spec L2.1: 0.4/0.4/0.2).
pub const W_RECENCY: f64 = 0.4;
/// Composite weight — importance component (importance is on a 1–10 scale, normalized inside).
pub const W_IMPORTANCE: f64 = 0.4;
/// Composite weight — relevance component. Relevance is a QUERY-TIME term; the job stores the
/// baseline `0.0` (see [`run_consolidation_pass`]).
pub const W_RELEVANCE: f64 = 0.2;

/// Exponential recency decay per hour (generative-agents recipe): `0.995^hours`.
pub const RECENCY_DECAY_PER_HOUR: f64 = 0.995;

/// Importance assigned when the reasoner is the stub / absent or its reply is unparseable —
/// the middle of the 1–10 scale (never blocks scoring on a missing model).
pub const DEFAULT_IMPORTANCE: f64 = 5.0;

/// Interval between consolidation passes (hourly).
pub const CONSOLIDATION_INTERVAL_SECS: u64 = 3_600;

/// An entity qualifies for a reflection rollup at this many OPEN visible facts.
const ROLLUP_MIN_FACTS: usize = 3;

/// Spec L2.1's second eligibility arm: an entity ALSO qualifies for a rollup when ANY of its open
/// visible facts has an assessed importance at or above this (a single critical fact rolls up).
const IMPORTANT_FACT_MIN: f64 = 7.0;

/// Hard token cap on one reflection synthesis (spec L2.1).
const REFLECTION_MAX_TOKENS: usize = 256;

/// Hard token cap on one importance-assessment batch reply.
const IMPORTANCE_MAX_TOKENS: usize = 256;

/// Wall-clock bound on every reasoner call this job makes (a background job must never wedge the
/// Metal queue; a timed-out call degrades to defaults / no rollup).
const LLM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Max never-scored facts assessed in ONE importance batch call (bounds the prompt).
const IMPORTANCE_BATCH: usize = 32;

/// Max reflection (rollup) syntheses per pass — bounds per-pass Metal time; the rest catch up on
/// later passes.
const MAX_REFLECTIONS_PER_PASS: usize = 5;

/// Max facts rendered into one reflection prompt (entity or weekly).
const REFLECT_MAX_FACTS: usize = 40;

/// What one pass did (counts only — safe to log).
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct PassStats {
    /// Open visible user facts (re)scored into `memory_scores`.
    pub scored: usize,
    /// Rollups upserted (entity + weekly, new + re-reflected) this pass.
    pub rollups: usize,
    /// Rollup `.md` files exported to the vault this pass.
    pub exported: usize,
    /// Rollups GC'd this pass (scope no longer eligible — row + exported file deleted).
    pub deleted: usize,
}

/// DETERMINISTIC recency: `0.995^hours_since(valid_from)`, clamped to `[0, 1]`. Pure (both instants
/// injected). A `now` BEFORE `valid_from` clamps the age to 0 (recency 1.0). Unparseable timestamps
/// degrade to a NEUTRAL `0.5` — never a panic, and neither buried nor artificially fresh.
pub fn compute_recency(valid_from_iso: &str, now_iso: &str) -> f64 {
    let (Ok(from), Ok(now)) = (
        chrono::DateTime::parse_from_rfc3339(valid_from_iso),
        chrono::DateTime::parse_from_rfc3339(now_iso),
    ) else {
        return 0.5;
    };
    let hours = (now - from).num_seconds().max(0) as f64 / 3_600.0;
    RECENCY_DECAY_PER_HOUR.powf(hours).clamp(0.0, 1.0)
}

/// DETERMINISTIC composite: `0.4·recency + 0.4·(importance/10) + 0.2·relevance` (named weight
/// consts). `importance` comes in on the 1–10 scale and is normalized here; every input is clamped
/// to its valid range first so a junk model reply can never produce an out-of-range score.
pub fn composite_score(recency: f64, importance: f64, relevance: f64) -> f64 {
    let r = recency.clamp(0.0, 1.0);
    let i = (importance.clamp(0.0, 10.0)) / 10.0;
    let v = relevance.clamp(0.0, 1.0);
    W_RECENCY * r + W_IMPORTANCE * i + W_RELEVANCE * v
}

/// DETERMINISTIC, dependency-free hash of a rollup's source fact set: FNV-1a 64 over the SORTED
/// open-fact ids (with a record separator). Fact rows are append-only under the bitemporal model
/// (a supersede CLOSES the old row and ADDS a new one; a seal DELETES rows), so the open-id set
/// changes exactly when the content a rollup was synthesized from changes. Stored per rollup
/// (`memory_rollups.fact_set_hash`) and compared each pass to decide re-reflection. Pure — no
/// RandomState/DefaultHasher (stable across processes AND rustc versions; MSRV-1.77-safe).
pub fn fact_set_hash(fact_ids: &[&str]) -> String {
    let mut ids: Vec<&str> = fact_ids.to_vec();
    ids.sort_unstable();
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET;
    for id in ids {
        for b in id.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(FNV_PRIME);
        }
        h ^= 0xff; // record separator — ["ab","c"] never collides with ["a","bc"].
        h = h.wrapping_mul(FNV_PRIME);
    }
    format!("{h:016x}")
}

/// Spec L2.1 reflection eligibility for an entity group: ≥ [`ROLLUP_MIN_FACTS`] open visible facts
/// OR any open fact with an assessed importance ≥ [`IMPORTANT_FACT_MIN`] (a single critical fact
/// rolls up). `importance` is the persisted `facts.importance` map — an unassessed fact contributes
/// nothing to the arm (fail-quiet, never invents importance).
fn entity_group_eligible(open: &[&Fact], importance: &HashMap<String, f64>) -> bool {
    open.len() >= ROLLUP_MIN_FACTS
        || open
            .iter()
            .any(|f| importance.get(&f.id).copied().unwrap_or(0.0) >= IMPORTANT_FACT_MIN)
}

/// The ISO-week scope key for `now` (`weekly:<YYYY-WNN>`), or `None` when `now` is unparseable.
fn weekly_scope(now_iso: &str) -> Option<String> {
    let now = chrono::DateTime::parse_from_rfc3339(now_iso).ok()?;
    let week = chrono::Datelike::iso_week(&now.date_naive());
    Some(format!("weekly:{}-W{:02}", week.year(), week.week()))
}

const IMPORTANCE_SYSTEM: &str = "You rate how important each durable memory about the user is to \
remember long-term, on a 1 (trivial) to 10 (critical) scale. Output STRICT JSON ONLY (no prose, \
no code fences): {\"scores\":[{\"id\":\"<the given id>\",\"importance\":7}]}. Rate EVERY listed \
memory, copy each id exactly, and output ONLY the JSON.";

#[derive(Debug, Deserialize)]
struct ImportanceReply {
    #[serde(default)]
    scores: Vec<RawImportance>,
}

#[derive(Debug, Deserialize)]
struct RawImportance {
    #[serde(default)]
    id: String,
    #[serde(default)]
    importance: f64,
}

/// BEST-EFFORT batch importance assessment on the LIGHT reasoner (bounded tokens + wall clock).
/// Stub / any failure ⇒ an EMPTY map — the caller falls back to [`DEFAULT_IMPORTANCE`]. Out-of-range
/// replies are clamped by [`composite_score`] later; ids not in `facts` are ignored.
fn assess_importance(reasoner: &dyn LocalReasoner, facts: &[&Fact]) -> HashMap<String, f64> {
    if facts.is_empty() || reasoner.id() == "stub" {
        return HashMap::new();
    }
    let lines = facts
        .iter()
        .take(IMPORTANCE_BATCH)
        .map(|f| format!("- id={} | {} {}: {}", f.id, f.subject, f.predicate, f.object))
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!("MEMORIES:\n{lines}");
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "scores": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "importance": { "type": "number" }
                    },
                    "required": ["id", "importance"]
                }
            }
        },
        "required": ["scores"]
    });
    let opts = GenOptions {
        max_tokens: Some(IMPORTANCE_MAX_TOKENS),
        temperature: Some(0.1),
        enable_thinking: false,
        timeout: Some(LLM_TIMEOUT),
        ..GenOptions::default()
    };
    let value = match reasoner.structured_with(IMPORTANCE_SYSTEM, &user, &schema, opts) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(target: "memory", error = %e, "importance assessment failed; using defaults");
            return HashMap::new();
        }
    };
    let reply: ImportanceReply = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(target: "memory", error = %e, "importance reply unparseable; using defaults");
            return HashMap::new();
        }
    };
    let known: HashSet<&str> = facts.iter().map(|f| f.id.as_str()).collect();
    reply
        .scores
        .into_iter()
        .filter(|s| known.contains(s.id.as_str()))
        .map(|s| (s.id, s.importance))
        .collect()
}

const REFLECT_SYSTEM: &str = "You are consolidating durable memory notes. Given a list of current \
facts, write a SHORT synthesis (3-6 plain sentences or bullets) of the higher-level picture they \
paint: patterns, roles, ongoing priorities. No preamble, no headings, no speculation beyond the \
facts. Write in the language the facts are written in.";

/// BEST-EFFORT reflection synthesis on the LIGHT reasoner. THE STUB NEVER PRODUCES A ROLLUP
/// (its output is a debug echo and rollups are exported to the user's vault) — `None` on stub or
/// any failure/empty reply.
fn reflect(reasoner: &dyn LocalReasoner, label: &str, facts: &[&Fact]) -> Option<String> {
    if reasoner.id() == "stub" || facts.is_empty() {
        return None;
    }
    let lines = facts
        .iter()
        .take(REFLECT_MAX_FACTS)
        .map(|f| format!("- {} {}: {}", f.subject, f.predicate, f.object))
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!("SCOPE: {label}\n\nCURRENT FACTS:\n{lines}");
    let opts = GenOptions {
        max_tokens: Some(REFLECTION_MAX_TOKENS),
        temperature: Some(0.3),
        enable_thinking: false,
        timeout: Some(LLM_TIMEOUT),
        ..GenOptions::default()
    };
    match reasoner.reason_with(REFLECT_SYSTEM, &user, opts) {
        Ok(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(target: "memory", error = %e, "reflection synthesis failed; no rollup this pass");
            None
        }
    }
}

/// GC ONE no-longer-eligible rollup: delete the row and remove its exported vault `.md` — ONLY the
/// path the row recorded at export time (never any other file; a missing file is fine). Best-effort:
/// a DB error warns and leaves the row for the next pass. Logs the scope only (content-free).
fn gc_rollup(db: &Db, scope: &str, stats: &mut PassStats) {
    match db.delete_memory_rollup(scope) {
        Ok(exported) => {
            if let Some(p) = exported {
                let _ = std::fs::remove_file(&p);
            }
            stats.deleted += 1;
            tracing::debug!(target: "memory", scope = %scope, "rollup GC'd (scope no longer eligible)");
        }
        Err(e) => {
            tracing::warn!(target: "memory", scope = %scope, error = %e, "rollup GC delete failed; retrying next pass");
        }
    }
}

/// Assess + persist (`facts.importance`) the importance of any never-assessed facts in `open` on
/// the LIGHT reasoner — best-effort, batch, bounded. Only called for SMALL groups (the count arm
/// already made a bigger group eligible), so the prompt stays tiny. Facts the model failed to rate
/// persist [`DEFAULT_IMPORTANCE`] so steady-state passes stay LLM-free (the same contract as the
/// user-fact scoring step). No-op on the stub.
fn ensure_entity_importance(
    db: &Db,
    reasoner: &dyn LocalReasoner,
    open: &[&Fact],
    known: &mut HashMap<String, f64>,
) {
    if reasoner.id() == "stub" {
        return;
    }
    let unassessed: Vec<&Fact> = open
        .iter()
        .filter(|f| !known.contains_key(&f.id))
        .copied()
        .collect();
    if unassessed.is_empty() {
        return;
    }
    let assessed = assess_importance(reasoner, &unassessed);
    for f in unassessed {
        let imp = assessed.get(&f.id).copied().unwrap_or(DEFAULT_IMPORTANCE);
        match db.set_fact_importance(&f.id, imp) {
            Ok(()) => {
                known.insert(f.id.clone(), imp);
            }
            Err(e) => {
                tracing::warn!(target: "memory", error = %e, "fact importance persist failed; retrying next pass");
            }
        }
    }
}

/// The vault path a rollup exports to: `<vault>/brain/memory/<scope with ':' → '-'>.md`. The scope
/// carries only ids / week keys (never fact text), so the FILENAME is content-free.
fn rollup_export_path(vault_dir: &Path, scope: &str) -> PathBuf {
    let name: String = scope
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    vault_dir.join("brain").join("memory").join(format!("{name}.md"))
}

/// Render one rollup `.md` (frontmatter + synthesis body).
fn rollup_markdown(scope: &str, updated_at: &str, content: &str) -> String {
    format!(
        "---\nmurmur: memory-rollup\nscope: {scope}\nupdated: {updated_at}\n---\n\n{content}\n"
    )
}

/// ONE consolidation pass — the testable orchestrator (no AppHandle, both the reasoner and `now`
/// injected). Steps and their failure posture:
///   1. sweep scores of CLOSED facts (forgotten/superseded);
///   2. score every open VISIBLE user fact (empty unlock set — sealed excluded by design):
///      recency from `valid_from`, importance from the batch assessment (only never-scored facts
///      are assessed; stub/failure ⇒ [`DEFAULT_IMPORTANCE`]), stored relevance `0.0` (relevance is
///      a query-time term — the stored composite is the query-independent baseline);
///   3. GC + regenerate EVERY existing rollup against the CURRENT visible fact set
///      ([`fact_set_hash`]): a no-longer-eligible scope is DELETED (row + exported `.md`), a
///      changed set is re-reflected (weekly scopes included); then reflect NEW eligible entity
///      groups (≥ [`ROLLUP_MIN_FACTS`] open visible facts OR any fact with importance ≥
///      [`IMPORTANT_FACT_MIN`]) + the once-per-ISO-week user rollup — reflection NEVER on the stub
///      (GC deletion still runs);
///   4. export un-exported rollups to `<vault>/brain/memory/` (atomic write), when a vault is set.
///
/// Per-entity / per-file errors warn + continue; only a hard DB error on the scoring path errors
/// the pass (the caller loop warns + retries next tick).
pub fn run_consolidation_pass(
    db: &Db,
    reasoner: &dyn LocalReasoner,
    vault_dir: Option<&Path>,
    now: &str,
) -> Result<PassStats> {
    let mut stats = PassStats::default();
    let no_unlocks: HashSet<String> = HashSet::new();

    // 1. Closed facts keep no score rows (purged/deleted ones already cascaded off the FK).
    if let Err(e) = db.delete_memory_scores_for_closed_facts() {
        tracing::warn!(target: "memory", error = %e, "closed-fact score sweep failed; continuing");
    }

    // 2. Score the open, VISIBLE user facts.
    let facts = db.list_user_facts_visible(&no_unlocks)?;
    let known_importance = db.memory_importance_map()?;
    let unscored: Vec<&Fact> = facts
        .iter()
        .filter(|f| !known_importance.contains_key(&f.id))
        .collect();
    // The ONLY LLM call of the scoring step — skipped entirely when nothing is new.
    let assessed = assess_importance(reasoner, &unscored);
    for f in &facts {
        let importance = known_importance
            .get(&f.id)
            .or_else(|| assessed.get(&f.id))
            .copied()
            .unwrap_or(DEFAULT_IMPORTANCE);
        let recency = compute_recency(&f.valid_from, now);
        let relevance = 0.0; // query-time term — stored baseline (see the fn doc).
        let composite = composite_score(recency, importance, relevance);
        db.upsert_memory_score(&f.id, "user", recency, importance, relevance, composite, now)?;
        stats.scored += 1;
    }

    // 3. Rollup GC + regeneration + creation. The GC half (deleting a no-longer-eligible scope's
    //    row + exported file) runs under EVERY reasoner — deletion needs no LLM; (re-)reflection
    //    is non-stub only (stub text must never reach the vault).
    let entities = db.list_entities_visible(&no_unlocks).unwrap_or_default();
    let entity_names: HashMap<String, String> = entities
        .iter()
        .map(|e| (e.id.clone(), e.name.clone()))
        .collect();
    let mut fact_importance = db.fact_importance_map().unwrap_or_default();
    let existing = db.list_memory_rollups()?;
    let existing_scopes: HashSet<String> = existing.iter().map(|r| r.scope.clone()).collect();
    let user_ids: Vec<&str> = facts.iter().map(|f| f.id.as_str()).collect();
    let user_hash = fact_set_hash(&user_ids);
    let mut reflections = 0usize;

    // 3a. EVERY existing rollup is re-derived against the CURRENT visible fact set: a scope that is
    //     no longer eligible (entity invisible / below both eligibility arms / weekly with no facts)
    //     is DELETED (row + exported vault `.md`); a scope whose fact set changed (`fact_set_hash`
    //     mismatch — a supersede, forget, or new fact) is RE-REFLECTED + re-exported. Weekly scopes
    //     INCLUDED — a frozen weekly rollup would preserve stale/superseded synthesis forever.
    for r in &existing {
        if let Some(entity_id) = r.scope.strip_prefix("entity:") {
            let visible = entity_names.contains_key(entity_id);
            let ent_facts = if visible {
                match db.list_facts_visible(entity_id, &no_unlocks) {
                    Ok(fs) => fs,
                    Err(e) => {
                        tracing::warn!(target: "memory", scope = %r.scope, error = %e, "entity fact read failed; leaving rollup for next pass");
                        continue;
                    }
                }
            } else {
                Vec::new()
            };
            let open: Vec<&Fact> = ent_facts.iter().filter(|f| f.valid_to.is_none()).collect();
            if open.len() < ROLLUP_MIN_FACTS {
                ensure_entity_importance(db, reasoner, &open, &mut fact_importance);
            }
            if !visible || !entity_group_eligible(&open, &fact_importance) {
                gc_rollup(db, &r.scope, &mut stats);
                continue;
            }
            let ids: Vec<&str> = open.iter().map(|f| f.id.as_str()).collect();
            let hash = fact_set_hash(&ids);
            if r.fact_set_hash.as_deref() == Some(hash.as_str()) {
                continue; // unchanged — nothing to redo.
            }
            if reasoner.id() == "stub" || reflections >= MAX_REFLECTIONS_PER_PASS {
                continue; // stale but not re-reflectable this pass — caught next pass.
            }
            let label = entity_names.get(entity_id).cloned().unwrap_or_default();
            if let Some(content) = reflect(reasoner, &label, &open) {
                if let Err(e) = db.upsert_memory_rollup(&r.scope, &content, &hash, now) {
                    tracing::warn!(target: "memory", scope = %r.scope, error = %e, "rollup re-reflect upsert failed; continuing");
                    continue;
                }
                stats.rollups += 1;
                reflections += 1;
            }
        } else if r.scope.starts_with("weekly:") {
            // A weekly rollup synthesizes the user's WHOLE visible open fact set.
            if facts.is_empty() {
                gc_rollup(db, &r.scope, &mut stats);
                continue;
            }
            if r.fact_set_hash.as_deref() == Some(user_hash.as_str())
                || reasoner.id() == "stub"
                || reflections >= MAX_REFLECTIONS_PER_PASS
            {
                continue;
            }
            let refs: Vec<&Fact> = facts.iter().collect();
            if let Some(content) =
                reflect(reasoner, "what the user is working on and prefers", &refs)
            {
                if let Err(e) = db.upsert_memory_rollup(&r.scope, &content, &user_hash, now) {
                    tracing::warn!(target: "memory", scope = %r.scope, error = %e, "weekly re-reflect upsert failed; continuing");
                    continue;
                }
                stats.rollups += 1;
                reflections += 1;
            }
        }
        // Unknown scope shapes are left untouched (forward-compat).
    }

    // 3b. NEW rollups — never on the stub (stub text must never reach the vault).
    if reasoner.id() != "stub" {
        // Entity groups without a rollup yet (gated reads, empty unlock set). Eligibility is
        // ≥ ROLLUP_MIN_FACTS open facts OR any fact with importance ≥ IMPORTANT_FACT_MIN.
        for ent in &entities {
            if reflections >= MAX_REFLECTIONS_PER_PASS {
                break;
            }
            let scope = format!("entity:{}", ent.id);
            if existing_scopes.contains(&scope) {
                continue; // regenerated (or GC'd as ineligible) in 3a.
            }
            let ent_facts = match db.list_facts_visible(&ent.id, &no_unlocks) {
                Ok(fs) => fs,
                Err(e) => {
                    tracing::warn!(target: "memory", entity_id = %ent.id, error = %e, "entity fact read failed; skipping");
                    continue;
                }
            };
            let open: Vec<&Fact> = ent_facts.iter().filter(|f| f.valid_to.is_none()).collect();
            if open.len() < ROLLUP_MIN_FACTS {
                ensure_entity_importance(db, reasoner, &open, &mut fact_importance);
            }
            if !entity_group_eligible(&open, &fact_importance) {
                continue;
            }
            let ids: Vec<&str> = open.iter().map(|f| f.id.as_str()).collect();
            let hash = fact_set_hash(&ids);
            if let Some(content) = reflect(reasoner, &ent.name, &open) {
                if let Err(e) = db.upsert_memory_rollup(&scope, &content, &hash, now) {
                    tracing::warn!(target: "memory", scope = %scope, error = %e, "rollup upsert failed; continuing");
                    continue;
                }
                stats.rollups += 1;
                reflections += 1;
            }
        }

        // The weekly user rollup — created once per ISO week (then hash-regenerated in 3a).
        if let Some(scope) = weekly_scope(now) {
            if !existing_scopes.contains(&scope)
                && !facts.is_empty()
                && reflections < MAX_REFLECTIONS_PER_PASS
            {
                let refs: Vec<&Fact> = facts.iter().collect();
                if let Some(content) =
                    reflect(reasoner, "what the user is working on and prefers", &refs)
                {
                    if let Err(e) = db.upsert_memory_rollup(&scope, &content, &user_hash, now) {
                        tracing::warn!(target: "memory", scope = %scope, error = %e, "weekly rollup upsert failed");
                    } else {
                        stats.rollups += 1;
                    }
                }
            }
        }
    }

    // 4. Export un-exported rollups to the vault (atomic write; best-effort per file).
    if let Some(vault) = vault_dir {
        for r in db.list_memory_rollups()? {
            if r.exported_path.is_some() {
                continue;
            }
            let path = rollup_export_path(vault, &r.scope);
            let md = rollup_markdown(&r.scope, &r.updated_at, &r.content);
            match crate::export::obsidian::overwrite_note(&path, &md) {
                Ok(()) => {
                    if let Err(e) =
                        db.set_memory_rollup_exported(&r.scope, &path.to_string_lossy())
                    {
                        tracing::warn!(target: "memory", scope = %r.scope, error = %e, "rollup export stamp failed");
                    } else {
                        stats.exported += 1;
                    }
                }
                Err(e) => {
                    tracing::warn!(target: "memory", scope = %r.scope, error = %e, "rollup vault export failed; will retry next pass");
                }
            }
        }
    }

    Ok(stats)
}

/// ONE production tick: resolve everything from the LIVE `AppState` (flag, LIGHT reasoner, vault),
/// then run [`run_consolidation_pass`]. Skips when the feature flag is off or the light engine is
/// the stub (no model ⇒ importance defaults would be the only output — not worth an hourly wake;
/// scores start accruing on the first tick after the user downloads a model). NEVER panics; every
/// failure is a warn. Logs counts only.
pub fn consolidation_tick(handle: &tauri::AppHandle) {
    use tauri::Manager;
    let Some(state) = handle.try_state::<crate::state::AppState>() else {
        return; // init failed — nothing to consolidate.
    };
    let enabled = state
        .config
        .lock()
        .map(|c| c.memory_consolidation_enabled && c.user_memory_enabled)
        .unwrap_or(false); // poisoned config ⇒ skip this tick (fail quiet, retry next hour).
    if !enabled {
        return;
    }
    // LIGHT engine — local-or-stub, NEVER cloud (zero egress from this job by construction).
    let reasoner = state.reasoner.light();
    if reasoner.id() == "stub" {
        tracing::debug!(target: "memory", "no local light model; skipping consolidation tick");
        return;
    }
    let vault = state
        .config
        .lock()
        .ok()
        .and_then(|c| c.vault_path.clone())
        .map(PathBuf::from);
    let now = chrono::Utc::now().to_rfc3339();
    match run_consolidation_pass(&state.db, reasoner.as_ref(), vault.as_deref(), &now) {
        Ok(stats) => {
            if stats.scored > 0 || stats.rollups > 0 || stats.exported > 0 {
                tracing::info!(
                    target: "memory",
                    scored = stats.scored,
                    rollups = stats.rollups,
                    exported = stats.exported,
                    "memory consolidation pass complete"
                );
            }
        }
        Err(e) => {
            tracing::warn!(target: "memory", error = %e, "memory consolidation pass failed; retrying next tick");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{FactOp, NewFact};
    use crate::reason::StubReasoner;
    use crate::storage::models::{Folder, Meeting, MeetingStatus, NoteRecord};

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn file_db(label: &str) -> Db {
        let p = crate::storage::db::unique_temp_path(&format!("murmur-memory-{label}"), "sqlite");
        Db::open_with_key(&p, TEST_DEK).unwrap()
    }

    fn temp_vault(label: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "murmur-memory-vault-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn seed_meeting(db: &Db, id: &str) {
        db.insert_meeting(&Meeting {
            id: id.to_string(),
            started_at: "2026-07-01T09:00:00Z".to_string(),
            ended_at: None,
            title: Some(format!("title-{id}")),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
    }

    fn seed_user_fact(db: &Db, predicate: &str, object: &str, meeting_id: &str) {
        db.apply_user_fact_ops(&[FactOp::Add(NewFact {
            entity_id: crate::user_memory::USER_SCOPE.to_string(),
            subject: "You".to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            valid_from: "2026-07-01T09:00:00Z".to_string(),
            recorded_at: "2026-07-01T09:00:00Z".to_string(),
            confidence: 1.0,
            meeting_id: Some(meeting_id.to_string()),
        })])
        .unwrap();
    }

    /// A deterministic mock "real" brain: a non-stub id, canned reflection text, and a canned
    /// importance for every listed fact — so the reflection/rollup path is testable headless.
    struct MockBrain;
    impl LocalReasoner for MockBrain {
        fn id(&self) -> &str {
            "mock-brain"
        }
        fn reason(&self, _system: &str, _user: &str) -> Result<String> {
            Ok("The user is heads-down on Project Atlas and prefers Polish replies.".to_string())
        }
        fn structured(
            &self,
            _system: &str,
            user: &str,
            _schema: &serde_json::Value,
        ) -> Result<serde_json::Value> {
            // Rate every `id=<id>` line at 8.0.
            let scores: Vec<serde_json::Value> = user
                .lines()
                .filter_map(|l| {
                    let rest = l.split("id=").nth(1)?;
                    let id = rest.split(' ').next()?.trim();
                    Some(serde_json::json!({ "id": id, "importance": 8.0 }))
                })
                .collect();
            Ok(serde_json::json!({ "scores": scores }))
        }
    }

    /// 0 h old ⇒ recency 1.0 (no decay yet).
    #[test]
    fn recency_is_one_at_zero_hours() {
        let r = compute_recency("2026-07-09T12:00:00Z", "2026-07-09T12:00:00Z");
        assert!((r - 1.0).abs() < 1e-9, "got {r}");
    }

    /// 24 h old ⇒ 0.995^24 ≈ 0.887 (the generative-agents decay curve).
    #[test]
    fn recency_decays_to_0887_at_24_hours() {
        let r = compute_recency("2026-07-08T12:00:00Z", "2026-07-09T12:00:00Z");
        assert!((r - 0.887).abs() < 0.001, "got {r}");
    }

    /// Unparseable timestamps degrade to the neutral 0.5; a future valid_from clamps to fresh.
    #[test]
    fn recency_degrades_gracefully() {
        assert!((compute_recency("garbage", "2026-07-09T12:00:00Z") - 0.5).abs() < 1e-9);
        let future = compute_recency("2026-07-10T12:00:00Z", "2026-07-09T12:00:00Z");
        assert!((future - 1.0).abs() < 1e-9, "future valid_from clamps to age 0");
    }

    /// The 0.4/0.4/0.2 blend, with importance normalized from the 1–10 scale and inputs clamped.
    #[test]
    fn composite_blends_weights() {
        let c = composite_score(1.0, 10.0, 1.0);
        assert!((c - 1.0).abs() < 1e-9, "max inputs ⇒ 1.0, got {c}");
        let c = composite_score(0.5, 5.0, 0.0);
        assert!((c - (0.4 * 0.5 + 0.4 * 0.5)).abs() < 1e-9, "got {c}");
        // Junk model output is clamped, never out-of-range.
        let c = composite_score(2.0, 99.0, -3.0);
        assert!((0.0..=1.0).contains(&c));
    }

    /// STUB PASS (the default install): every open visible user fact gets a score with the DEFAULT
    /// importance, and NO rollup is written (stub text must never reach the vault). RED-first for
    /// the L2.1 store: fails before `memory_scores`/`run_consolidation_pass` existed.
    #[test]
    fn stub_pass_scores_defaults_and_writes_no_rollups() {
        let db = file_db("stub-pass");
        seed_meeting(&db, "m1");
        seed_user_fact(&db, "prefer", "Polish replies", "m1");
        seed_user_fact(&db, "works on", "Project Atlas", "m1");

        let stats =
            run_consolidation_pass(&db, &StubReasoner, None, "2026-07-09T12:00:00Z").unwrap();
        assert_eq!(stats.scored, 2);
        assert_eq!(stats.rollups, 0, "the stub must never produce a rollup");
        assert_eq!(stats.exported, 0);

        let scores = db.list_memory_scores().unwrap();
        assert_eq!(scores.len(), 2);
        for s in &scores {
            assert!((s.importance - DEFAULT_IMPORTANCE).abs() < 1e-9);
            assert!((0.0..=1.0).contains(&s.composite));
        }
        assert!(db.list_memory_rollups().unwrap().is_empty());
    }

    /// GATE: a sealed-and-not-unlocked meeting's user facts are NEVER scored (the job reads with
    /// the EMPTY unlock set by design).
    #[test]
    fn pass_excludes_sealed_facts() {
        let db = file_db("sealed-pass");
        db.insert_folder(&Folder {
            id: "f-lock".to_string(),
            name: "Secret".to_string(),
            path: "Secret".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-01T00:00:00Z".to_string(),
        })
        .unwrap();
        seed_meeting(&db, "m-sealed");
        db.upsert_note(&NoteRecord {
            meeting_id: "m-sealed".to_string(),
            provider_id: "claude_code".to_string(),
            markdown: "secret".to_string(),
            created_at: "2026-07-01T00:00:00Z".to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_note_folder("m-sealed", Some("f-lock")).unwrap();
        seed_user_fact(&db, "salary", "confidential", "m-sealed");
        db.set_folder_locked("f-lock", true, None).unwrap();

        let stats =
            run_consolidation_pass(&db, &StubReasoner, None, "2026-07-09T12:00:00Z").unwrap();
        assert_eq!(stats.scored, 0, "sealed facts must not be scored");
        assert!(db.list_memory_scores().unwrap().is_empty());
    }

    /// REAL-BRAIN PASS: importance comes from the batch assessment, the weekly rollup is written
    /// ONCE per ISO week (idempotent on re-run), and the rollup exports atomically to
    /// `<vault>/brain/memory/`. Steady-state re-run makes no further LLM-derived changes.
    #[test]
    fn mock_brain_pass_scores_reflects_and_exports_once_per_week() {
        let db = file_db("mock-pass");
        let vault = temp_vault("mock-pass");
        seed_meeting(&db, "m1");
        seed_user_fact(&db, "prefer", "Polish replies", "m1");
        seed_user_fact(&db, "works on", "Project Atlas", "m1");

        let now = "2026-07-09T12:00:00Z";
        let stats = run_consolidation_pass(&db, &MockBrain, Some(&vault), now).unwrap();
        assert_eq!(stats.scored, 2);
        assert_eq!(stats.rollups, 1, "the weekly rollup (no entity has ≥3 facts)");
        assert_eq!(stats.exported, 1);

        // Importance came from the mock assessment (8.0), not the default.
        for s in db.list_memory_scores().unwrap() {
            assert!((s.importance - 8.0).abs() < 1e-9, "got {}", s.importance);
        }

        // The rollup landed in the vault, under the content-free scope filename.
        let rollups = db.list_memory_rollups().unwrap();
        assert_eq!(rollups.len(), 1);
        assert!(rollups[0].scope.starts_with("weekly:2026-W"));
        let exported = rollups[0].exported_path.clone().unwrap();
        let on_disk = std::fs::read_to_string(&exported).unwrap();
        assert!(on_disk.contains("murmur: memory-rollup"));
        assert!(on_disk.contains("Project Atlas"));

        // SAME ISO WEEK re-run: no second weekly rollup, nothing re-exported.
        let again = run_consolidation_pass(&db, &MockBrain, Some(&vault), now).unwrap();
        assert_eq!(again.rollups, 0, "weekly rollup is once per ISO week");
        assert_eq!(again.exported, 0);
        assert_eq!(db.list_memory_rollups().unwrap().len(), 1);
    }

    /// Rollup upsert is idempotent PER SCOPE: a re-upsert replaces content (one row), keeps the
    /// created_at, resets exported_path for re-export, and records the new fact-set hash.
    #[test]
    fn rollup_upsert_is_idempotent_per_scope() {
        let db = file_db("rollup-upsert");
        db.upsert_memory_rollup("entity:e1", "v1", "h1", "2026-07-09T12:00:00Z")
            .unwrap();
        db.set_memory_rollup_exported("entity:e1", "/vault/x.md")
            .unwrap();
        db.upsert_memory_rollup("entity:e1", "v2", "h2", "2026-07-09T13:00:00Z")
            .unwrap();
        let rollups = db.list_memory_rollups().unwrap();
        assert_eq!(rollups.len(), 1, "one row per scope");
        assert_eq!(rollups[0].content, "v2");
        assert_eq!(rollups[0].created_at, "2026-07-09T12:00:00Z");
        assert_eq!(rollups[0].updated_at, "2026-07-09T13:00:00Z");
        assert!(rollups[0].exported_path.is_none(), "reset for re-export");
        assert_eq!(rollups[0].fact_set_hash.as_deref(), Some("h2"));
    }

    /// The rollup change-detector: deterministic, order-insensitive over the id SET, and sensitive
    /// to any membership change (a supersede closes one id and adds another ⇒ new hash).
    #[test]
    fn fact_set_hash_is_order_insensitive_and_membership_sensitive() {
        let a = fact_set_hash(&["f1", "f2", "f3"]);
        assert_eq!(a, fact_set_hash(&["f3", "f1", "f2"]), "order must not matter");
        assert_ne!(a, fact_set_hash(&["f1", "f2"]), "membership change must change the hash");
        assert_ne!(a, fact_set_hash(&["f1", "f2", "f4"]));
        assert_ne!(fact_set_hash(&["ab", "c"]), fact_set_hash(&["a", "bc"]), "separator guards concat collisions");
        assert_eq!(a, fact_set_hash(&["f2", "f3", "f1"]), "stable across calls");
    }

    /// FK/cascade decision test: purge-on-seal (direct DELETE) and delete_meeting both drop the
    /// fact's score row via the `memory_scores` FK cascade.
    #[test]
    fn scores_cascade_with_fact_purge_and_meeting_delete() {
        let db = file_db("score-cascade");
        seed_meeting(&db, "m1");
        seed_meeting(&db, "m2");
        seed_user_fact(&db, "prefer", "Polish replies", "m1");
        seed_user_fact(&db, "works on", "Project Atlas", "m2");
        run_consolidation_pass(&db, &StubReasoner, None, "2026-07-09T12:00:00Z").unwrap();
        assert_eq!(db.list_memory_scores().unwrap().len(), 2);

        // Purge-on-seal path (direct DELETE inside the seal tx).
        db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
        assert_eq!(
            db.list_memory_scores().unwrap().len(),
            1,
            "purged fact's score row must cascade away"
        );

        // delete_meeting path.
        db.delete_meeting("m2").unwrap();
        assert!(
            db.list_memory_scores().unwrap().is_empty(),
            "deleted meeting's fact score must cascade away"
        );
    }

    /// A mock brain whose reflection ECHOES the fact lines it was given — so stale-vs-fresh rollup
    /// content is assertable (the constant-text [`MockBrain`] cannot distinguish a re-reflection).
    struct EchoBrain;
    impl LocalReasoner for EchoBrain {
        fn id(&self) -> &str {
            "echo-brain"
        }
        fn reason(&self, _system: &str, user: &str) -> Result<String> {
            Ok(user.to_string())
        }
        fn structured(
            &self,
            _system: &str,
            user: &str,
            _schema: &serde_json::Value,
        ) -> Result<serde_json::Value> {
            let scores: Vec<serde_json::Value> = user
                .lines()
                .filter_map(|l| {
                    let rest = l.split("id=").nth(1)?;
                    let id = rest.split(' ').next()?.trim();
                    Some(serde_json::json!({ "id": id, "importance": 8.0 }))
                })
                .collect();
            Ok(serde_json::json!({ "scores": scores }))
        }
    }

    fn seed_entity_fact(db: &Db, entity_id: &str, predicate: &str, object: &str, meeting_id: &str) {
        db.apply_fact_ops(&[FactOp::Add(NewFact {
            entity_id: entity_id.to_string(),
            subject: "Atlas Corp".to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            valid_from: "2026-07-01T09:00:00Z".to_string(),
            recorded_at: "2026-07-01T09:00:00Z".to_string(),
            confidence: 1.0,
            meeting_id: Some(meeting_id.to_string()),
        })])
        .unwrap();
    }

    /// FIX 1b RED (adversarial finding 1, the non-seal half): a SUPERSEDED fact must age out of the
    /// rollup — the next pass detects the fact-set change ([`fact_set_hash`]) and RE-REFLECTS +
    /// re-exports (weekly scopes INCLUDED; the old "never re-touched after creation" freeze was the
    /// bug). RED on the pre-fix code: the weekly rollup was created once per ISO week and never
    /// regenerated, so the stale "Polish replies" synthesis persisted in the DB and the vault.
    #[test]
    fn superseded_fact_re_reflects_rollup_on_next_pass() {
        let db = file_db("regen-supersede");
        let vault = temp_vault("regen-supersede");
        seed_meeting(&db, "m1");
        seed_user_fact(&db, "prefer", "Polish replies", "m1");

        let now = "2026-07-09T12:00:00Z";
        let first = run_consolidation_pass(&db, &EchoBrain, Some(&vault), now).unwrap();
        assert_eq!(first.rollups, 1, "the weekly rollup");
        let rollup = &db.list_memory_rollups().unwrap()[0];
        assert!(rollup.content.contains("Polish replies"));
        let exported = rollup.exported_path.clone().unwrap();
        assert!(std::fs::read_to_string(&exported).unwrap().contains("Polish replies"));

        // Supersede the fact (bitemporal close + add) — NOT a seal.
        let old = db.list_user_facts_visible(&HashSet::new()).unwrap();
        db.apply_user_fact_ops(&[FactOp::Invalidate {
            id: old[0].id.clone(),
            valid_to: "2026-07-09T13:00:00Z".to_string(),
        }])
        .unwrap();
        seed_user_fact(&db, "prefer", "English replies", "m1");

        // Next pass (same ISO week): the hash changed ⇒ re-reflect + re-export, not a frozen copy.
        let second =
            run_consolidation_pass(&db, &EchoBrain, Some(&vault), "2026-07-09T14:00:00Z").unwrap();
        assert_eq!(second.rollups, 1, "the weekly rollup must be RE-reflected");
        assert_eq!(second.exported, 1, "and re-exported");
        let rollup = &db.list_memory_rollups().unwrap()[0];
        assert!(rollup.content.contains("English replies"));
        assert!(
            !rollup.content.contains("Polish replies"),
            "the superseded fact must age out of the rollup: {}",
            rollup.content
        );
        let on_disk = std::fs::read_to_string(&exported).unwrap();
        assert!(on_disk.contains("English replies"));
        assert!(!on_disk.contains("Polish replies"), "stale export must be overwritten");
    }

    /// FIX 1b GC: a rollup whose scope is NO LONGER ELIGIBLE (here: the entity's only source
    /// meeting became invisible via a folder lock at the DB level) is DELETED — row AND exported
    /// vault `.md` — on the next pass, even though nothing re-reflects it. RED on the pre-fix code:
    /// the below-threshold group was skipped with `continue` and the row + file lingered forever.
    #[test]
    fn ineligible_scope_rollup_is_gcd_with_its_export() {
        let db = file_db("gc-ineligible");
        let vault = temp_vault("gc-ineligible");
        db.insert_folder(&Folder {
            id: "f-e".to_string(),
            name: "Clients".to_string(),
            path: "Clients".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-01T00:00:00Z".to_string(),
        })
        .unwrap();
        seed_meeting(&db, "m-e");
        db.upsert_note(&NoteRecord {
            meeting_id: "m-e".to_string(),
            provider_id: "claude_code".to_string(),
            markdown: "note".to_string(),
            created_at: "2026-07-01T00:00:00Z".to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_note_folder("m-e", Some("f-e")).unwrap();
        let eid = db
            .upsert_entity("Atlas Corp", crate::storage::models::EntityKind::Project)
            .unwrap();
        db.add_mention(&eid, "m-e").unwrap();
        seed_entity_fact(&db, &eid, "ships", "ZenithSecret", "m-e");
        seed_entity_fact(&db, &eid, "hq", "Warsaw", "m-e");
        seed_entity_fact(&db, &eid, "owner", "Kim", "m-e");

        let now = "2026-07-09T12:00:00Z";
        let stats = run_consolidation_pass(&db, &MockBrain, Some(&vault), now).unwrap();
        assert_eq!(stats.rollups, 1, "the entity rollup (no user facts ⇒ no weekly)");
        let exported = db.list_memory_rollups().unwrap()[0]
            .exported_path
            .clone()
            .unwrap();
        assert!(std::path::Path::new(&exported).exists());

        // The entity's only source meeting becomes invisible (DB-level lock flag — the command-layer
        // seal purge is tested separately; this is the GC safety net).
        db.set_folder_locked("f-e", true, None).unwrap();
        let stats =
            run_consolidation_pass(&db, &MockBrain, Some(&vault), "2026-07-09T13:00:00Z").unwrap();
        assert_eq!(stats.deleted, 1, "the invisible scope must be GC'd");
        assert!(db.list_memory_rollups().unwrap().is_empty(), "row deleted");
        assert!(
            !std::path::Path::new(&exported).exists(),
            "the exported vault .md must be deleted with the row"
        );
    }

    /// FIX 4 (spec L2.1): the SECOND eligibility arm — an entity group BELOW [`ROLLUP_MIN_FACTS`]
    /// still rolls up when any fact's assessed importance is ≥ [`IMPORTANT_FACT_MIN`]. The mock
    /// brain rates everything 8.0, so a 2-fact entity qualifies (RED before the arm existed: only
    /// the ≥3-facts arm was implemented and this asserted zero rollups). The assessments persist to
    /// `facts.importance` so the steady-state pass stays LLM-free.
    #[test]
    fn single_important_fact_entity_is_eligible_for_rollup() {
        let db = file_db("importance-arm");
        seed_meeting(&db, "m1");
        let eid = db
            .upsert_entity("Atlas Corp", crate::storage::models::EntityKind::Project)
            .unwrap();
        db.add_mention(&eid, "m1").unwrap();
        seed_entity_fact(&db, &eid, "ships", "Project Atlas", "m1");
        seed_entity_fact(&db, &eid, "deadline", "Friday", "m1");

        let stats =
            run_consolidation_pass(&db, &MockBrain, None, "2026-07-09T12:00:00Z").unwrap();
        assert_eq!(
            stats.rollups, 1,
            "2 facts with importance 8 ⇒ eligible via the importance arm"
        );
        let rollups = db.list_memory_rollups().unwrap();
        assert_eq!(rollups[0].scope, format!("entity:{eid}"));
        // Assessments persisted — the next pass needs no LLM to re-evaluate the arm.
        let imp = db.fact_importance_map().unwrap();
        assert_eq!(imp.len(), 2);
        assert!(imp.values().all(|v| (*v - 8.0).abs() < 1e-9));
    }
}
