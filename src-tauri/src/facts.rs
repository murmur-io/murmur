//! Bitemporal FACTS layer + a DETERMINISTIC reconcile (brain2 R2). Meeting-native, local, no
//! external graph DB. Answers "what is CURRENT vs SUPERSEDED / what changed" about the user's
//! entities — e.g. "Project Atlas status: in-progress → shipped", with full history.
//!
//! ## The two time axes (bitemporal)
//! Every fact carries TWO independent times:
//!   * `valid_from` / `valid_to` — **valid time**: when the fact was true in the world. `valid_to`
//!     NULL means *currently valid*; it is set (closed) when a later meeting supersedes the fact.
//!   * `recorded_at` — **transaction time**: when WE learned the fact (the reconcile run).
//!
//! Keeping both means we never DELETE a superseded fact — we close it (`valid_to`), preserving the
//! timeline ("was in-progress until 2026-06-20, shipped since"). History is additive.
//!
//! ## The load-bearing core is DETERMINISTIC
//! [`reconcile_facts`] is a PURE function (no LLM, no DB, no clock) — it is the headless-testable
//! heart of this layer. The only non-deterministic part is [`extract_fact_candidates`], which is
//! BEST-EFFORT: it asks the on-device reasoner for entity·predicate·object triples and degrades to
//! an EMPTY result (never an error, never a panic, never a block) when the brain/model is
//! unavailable. A note pipeline that extracts nothing simply records no new facts that run.
//!
//! ## Lock model (see `.claude/rules/lock-model.md`)
//! Facts are DERIVED content tied to a meeting. Like `note_chunks` / `correction_log` /
//! `assistant_interactions`, they are PURGED on seal (dropped, not key-sealed) in the same atomic
//! seal transaction, and every READ is visibility-gated (`Db::list_facts_visible`) so a
//! sealed-and-not-session-unlocked meeting's facts surface NOTHING.

use serde::{Deserialize, Serialize};

use crate::reason::LocalReasoner;

/// A persisted bitemporal fact row (DB-shaped). `valid_to == None` ⇒ currently valid; `Some` ⇒
/// closed (superseded) at that instant. `meeting_id` is the meeting we learned it from (the gating
/// + purge anchor).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Fact {
    pub id: String,
    pub entity_id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    /// Valid time start (when the fact became true) — the meeting's time.
    pub valid_from: String,
    /// Valid time end — `None` while currently valid, set when superseded.
    pub valid_to: Option<String>,
    /// Transaction time — when WE recorded it (the reconcile run).
    pub recorded_at: String,
    /// The meeting the fact was derived from (gating + purge anchor). `None` for legacy rows, which
    /// the gated reader treats as NOT visible (fail-closed).
    pub meeting_id: Option<String>,
    pub confidence: f64,
}

/// A best-effort extracted triple about an entity, before reconcile. Subject is the entity name;
/// `entity_id` ties it to the resolved graph entity. No time axes yet — reconcile assigns them.
#[derive(Debug, Clone, PartialEq)]
pub struct FactCandidate {
    pub entity_id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub confidence: f64,
}

/// A new fact to INSERT (an Add op). `valid_to` is implicitly NULL (open); `valid_from` and
/// `recorded_at` are both the reconcile instant `at`.
#[derive(Debug, Clone, PartialEq)]
pub struct NewFact {
    pub entity_id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub valid_from: String,
    pub recorded_at: String,
    pub confidence: f64,
    pub meeting_id: Option<String>,
}

/// One reconcile decision. The deterministic output of [`reconcile_facts`], applied atomically by
/// [`crate::storage::Db::apply_fact_ops`].
#[derive(Debug, Clone, PartialEq)]
pub enum FactOp {
    /// Insert a brand-new open fact.
    Add(NewFact),
    /// Close an existing open fact at `valid_to` (it was superseded).
    Invalidate { id: String, valid_to: String },
    /// The candidate matches an open fact with the SAME object — nothing to do.
    NoOp,
}

/// Normalize a subject/predicate/object for IDENTITY comparison: trim + full-Unicode lowercase
/// (so "Status"/"status" and "Shipped"/"shipped" compare equal). The ORIGINAL casing is preserved
/// in the stored row; this is only the dedup/supersession key.
fn norm(s: &str) -> String {
    s.trim().to_lowercase()
}

/// THE DETERMINISTIC CORE (no LLM, no DB, no clock — `at` is injected). Reconcile `candidates`
/// against the `existing` facts, producing the ops that keep the bitemporal store consistent.
///
/// For each candidate, the matching OPEN fact is the one with the same
/// `(entity_id, norm(subject), norm(predicate))` and `valid_to IS NULL`:
///   * **no match** → [`FactOp::Add`] (valid_from = recorded_at = `at`, open),
///   * **match, SAME object** → [`FactOp::NoOp`],
///   * **match, DIFFERENT object** → [`FactOp::Invalidate`] the old (valid_to = `at`) **and**
///     [`FactOp::Add`] the new (valid_from = `at`, open) — the old fact STAYS, closed, so history
///     is preserved.
///
/// Determinism + within-batch safety: a working view of the currently-open object per key is
/// threaded through the candidate loop, starting from `existing` and updated as ops are emitted, so
/// two candidates with the same key in ONE batch can't both Add an open duplicate. Malformed
/// candidates (empty entity/subject/predicate/object) are skipped (best-effort extraction can emit
/// junk). Closed (`valid_to.is_some()`) existing facts are ignored — only open facts are matchable.
pub fn reconcile_facts(existing: &[Fact], candidates: &[FactCandidate], at: &str) -> Vec<FactOp> {
    use std::collections::HashMap;
    // key -> (id-of-open-row-if-from-existing, normalized current object). `None` id means the open
    // fact was created earlier IN THIS BATCH (no row id yet) and so cannot be Invalidated.
    let mut open: HashMap<(String, String, String), (Option<String>, String)> = HashMap::new();
    for f in existing {
        if f.valid_to.is_some() {
            continue; // only OPEN facts are matchable.
        }
        let key = (f.entity_id.clone(), norm(&f.subject), norm(&f.predicate));
        open.insert(key, (Some(f.id.clone()), norm(&f.object)));
    }

    // Dedup candidates within THIS batch by key, LAST mention wins: a single note must not assert
    // two conflicting "current" values for the same (entity, subject, predicate) — without this, two
    // same-key candidates each emitted an Add and produced two simultaneously-open ("current") facts.
    // First-seen key order is preserved so the op output stays deterministic.
    let mut last_by_key: HashMap<(String, String, String), &FactCandidate> = HashMap::new();
    let mut order: Vec<(String, String, String)> = Vec::new();
    for c in candidates {
        let entity_id = c.entity_id.trim();
        let subject = c.subject.trim();
        let predicate = c.predicate.trim();
        let object = c.object.trim();
        if entity_id.is_empty() || subject.is_empty() || predicate.is_empty() || object.is_empty() {
            continue; // skip malformed candidate.
        }
        let key = (entity_id.to_string(), norm(subject), norm(predicate));
        if !last_by_key.contains_key(&key) {
            order.push(key.clone());
        }
        last_by_key.insert(key, c); // last wins
    }

    let mut ops = Vec::new();
    for key in &order {
        let c = last_by_key[key];
        let object = c.object.trim();
        let nobj = norm(object);
        let mk_new = || NewFact {
            entity_id: c.entity_id.trim().to_string(),
            subject: c.subject.trim().to_string(),
            predicate: c.predicate.trim().to_string(),
            object: object.to_string(),
            valid_from: at.to_string(),
            recorded_at: at.to_string(),
            confidence: c.confidence,
            // The pure core never knows the source meeting; the pipeline stamps it via
            // [`set_meeting_id`] before apply.
            meeting_id: None,
        };
        match open.get(key).cloned() {
            None => ops.push(FactOp::Add(mk_new())),
            Some((_, prev_obj)) if prev_obj == nobj => ops.push(FactOp::NoOp),
            Some((maybe_id, _)) => {
                if let Some(id) = maybe_id {
                    ops.push(FactOp::Invalidate {
                        id,
                        valid_to: at.to_string(),
                    });
                }
                ops.push(FactOp::Add(mk_new()));
            }
        }
    }
    ops
}

/// Stamp the source `meeting_id` onto every Add op (the gating + purge anchor). Called by the
/// pipeline after [`reconcile_facts`], so the pure core never needs the meeting id.
pub fn set_meeting_id(ops: &mut [FactOp], meeting_id: &str) {
    for op in ops.iter_mut() {
        if let FactOp::Add(nf) = op {
            nf.meeting_id = Some(meeting_id.to_string());
        }
    }
}

/// The shape the reasoner must emit. Best-effort: parse failures degrade to no facts.
#[derive(Debug, Deserialize)]
struct FactsReply {
    #[serde(default)]
    facts: Vec<RawTriple>,
}

#[derive(Debug, Deserialize)]
struct RawTriple {
    /// The entity this fact is about (matched case-insensitively to a known entity name).
    #[serde(default)]
    entity: String,
    #[serde(default)]
    predicate: String,
    #[serde(default)]
    object: String,
}

const EXTRACT_SYSTEM: &str = "You extract durable FACTS about specific entities from a meeting \
note, as entity·predicate·object triples. Output STRICT JSON ONLY (no prose, no code fences): \
{\"facts\":[{\"entity\":\"Exact Entity Name\",\"predicate\":\"short attribute\",\"object\":\"value\"}]}.\n\
- entity MUST be one of the ENTITIES listed (copy the name exactly).\n\
- predicate is a short, stable attribute (e.g. \"status\", \"owner\", \"deadline\", \"role\").\n\
- object is the current value (e.g. \"shipped\", \"Anna\", \"2026-07-01\").\n\
- Only durable state worth tracking across meetings — not one-off remarks. Empty array if none.\n\
Output ONLY the JSON.";

/// Maximum note chars fed to the extractor (bounds the prompt / leak surface, like graph.rs).
const EXTRACT_EXCERPT_CHARS: usize = 8000;

/// BEST-EFFORT extraction of fact candidates from a meeting note about the meeting's `entities`
/// (each `(entity_id, name)`). Uses the on-device reasoner's `structured` decode; on ANY failure
/// (stub reasoner / no model / decode error / parse error / no entities) returns an EMPTY vec —
/// never an error, never a panic, never a block beyond the reasoner call itself. The RECONCILE is
/// the load-bearing deterministic core; this is the soft front-end that feeds it.
pub fn extract_fact_candidates(
    reasoner: &dyn LocalReasoner,
    title: &str,
    note_markdown: &str,
    entities: &[(String, String)],
) -> Vec<FactCandidate> {
    if entities.is_empty() {
        return Vec::new();
    }
    // No real brain (the default build / no model) → no extraction. The deterministic reconcile is
    // still exercised on whatever candidates a real brain would produce; with the stub there are none.
    if reasoner.id() == "stub" {
        return Vec::new();
    }
    let excerpt: String = note_markdown.chars().take(EXTRACT_EXCERPT_CHARS).collect();
    let names = entities
        .iter()
        .map(|(_, n)| n.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let user = format!("MEETING: {title}\n\nENTITIES: {names}\n\nNOTE:\n{excerpt}");
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "facts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "entity": { "type": "string" },
                        "predicate": { "type": "string" },
                        "object": { "type": "string" }
                    },
                    "required": ["entity", "predicate", "object"]
                }
            }
        },
        "required": ["facts"]
    });

    let value = match reasoner.structured(EXTRACT_SYSTEM, &user, &schema) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(target: "facts", error = %e, "fact extraction failed; no candidates (best-effort)");
            return Vec::new();
        }
    };
    let reply: FactsReply = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(target: "facts", error = %e, "fact extraction reply unparseable; no candidates");
            return Vec::new();
        }
    };

    candidates_from_triples(reply.facts, entities)
}

/// Map raw extracted triples to [`FactCandidate`]s: resolve each `entity` name to a known
/// `entity_id` (case-insensitive), use the canonical entity name as the subject, drop unresolved or
/// empty triples. Pure + headless-testable (no reasoner needed).
fn candidates_from_triples(
    triples: Vec<RawTriple>,
    entities: &[(String, String)],
) -> Vec<FactCandidate> {
    let mut out = Vec::new();
    for t in triples {
        let ent = t.entity.trim();
        let predicate = t.predicate.trim();
        let object = t.object.trim();
        if ent.is_empty() || predicate.is_empty() || object.is_empty() {
            continue;
        }
        let Some((id, name)) = entities
            .iter()
            .find(|(_, n)| n.trim().to_lowercase() == ent.to_lowercase())
        else {
            continue; // entity not in the meeting's known set — skip (never invent).
        };
        out.push(FactCandidate {
            entity_id: id.clone(),
            subject: name.clone(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            confidence: 1.0,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(
        id: &str,
        entity: &str,
        subject: &str,
        predicate: &str,
        object: &str,
        valid_to: Option<&str>,
    ) -> Fact {
        Fact {
            id: id.to_string(),
            entity_id: entity.to_string(),
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            valid_from: "2026-06-01T00:00:00Z".to_string(),
            valid_to: valid_to.map(|s| s.to_string()),
            recorded_at: "2026-06-01T00:00:00Z".to_string(),
            meeting_id: Some("m0".to_string()),
            confidence: 1.0,
        }
    }

    fn cand(entity: &str, subject: &str, predicate: &str, object: &str) -> FactCandidate {
        FactCandidate {
            entity_id: entity.to_string(),
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            confidence: 1.0,
        }
    }

    /// Add: an entirely new (entity, subject, predicate) → one open Add at `at`.
    #[test]
    fn reconcile_adds_a_new_fact() {
        let ops = reconcile_facts(
            &[],
            &[cand("atlas", "Atlas", "status", "in-progress")],
            "2026-06-10T00:00:00Z",
        );
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            FactOp::Add(nf) => {
                assert_eq!(nf.object, "in-progress");
                assert_eq!(nf.valid_from, "2026-06-10T00:00:00Z");
                assert_eq!(nf.recorded_at, "2026-06-10T00:00:00Z");
            }
            other => panic!("expected Add, got {other:?}"),
        }
    }

    /// NoOp: an open fact with the SAME object (case/whitespace-insensitive) → nothing changes.
    #[test]
    fn reconcile_noop_on_identical() {
        let existing = vec![fact("f1", "atlas", "Atlas", "status", "in-progress", None)];
        let ops = reconcile_facts(
            &existing,
            &[cand("atlas", "Atlas", "Status", "  In-Progress ")],
            "2026-06-10T00:00:00Z",
        );
        assert_eq!(ops, vec![FactOp::NoOp]);
    }

    /// THE BITEMPORAL HISTORY TEST (RED-before-GREEN): an open fact whose object CHANGED →
    /// Invalidate-old (valid_to set to `at`) AND Add-new (open, valid_from `at`). The old fact is
    /// kept (closed), not deleted — history preserved.
    #[test]
    fn reconcile_invalidates_old_and_adds_new_on_change() {
        let existing = vec![fact("f1", "atlas", "Atlas", "status", "in-progress", None)];
        let at = "2026-06-20T00:00:00Z";
        let ops = reconcile_facts(
            &existing,
            &[cand("atlas", "Atlas", "status", "shipped")],
            at,
        );
        assert_eq!(ops.len(), 2, "a change must emit exactly Invalidate + Add");
        // Invalidate closes the OLD row at `at`.
        assert!(
            ops.iter().any(
                |o| matches!(o, FactOp::Invalidate { id, valid_to } if id == "f1" && valid_to == at)
            ),
            "old fact must be Invalidated with valid_to = at"
        );
        // Add opens the NEW row at `at`, still open (valid_to NULL by construction).
        assert!(
            ops.iter().any(
                |o| matches!(o, FactOp::Add(nf) if nf.object == "shipped" && nf.valid_from == at)
            ),
            "new fact must be Added open at valid_from = at"
        );
        // The old object must NOT be re-added.
        assert!(
            !ops.iter()
                .any(|o| matches!(o, FactOp::Add(nf) if nf.object == "in-progress")),
            "the superseded object must not be re-added"
        );
    }

    /// WITHIN-BATCH dedup (RED-before-GREEN for the data-quality fix): a single note that asserts two
    /// conflicting values for the same (entity, subject, predicate) must NOT produce two open
    /// ("current") facts — the LAST mention wins, so exactly one open Add of the final value.
    #[test]
    fn reconcile_dedups_conflicting_candidates_within_one_batch() {
        let at = "2026-06-20T00:00:00Z";
        let ops = reconcile_facts(
            &[],
            &[
                cand("atlas", "Atlas", "status", "in-progress"),
                cand("atlas", "Atlas", "status", "shipped"), // later mention in the SAME note
            ],
            at,
        );
        let adds: Vec<_> = ops.iter().filter(|o| matches!(o, FactOp::Add(_))).collect();
        assert_eq!(
            adds.len(),
            1,
            "two conflicting same-key candidates in one batch must not both become current"
        );
        assert!(
            matches!(adds[0], FactOp::Add(nf) if nf.object == "shipped"),
            "the LAST mention wins"
        );
    }

    /// Multiple entities with the SAME subject/predicate but different objects do NOT cross-
    /// contaminate: each reconciles only against its own entity's open fact.
    #[test]
    fn reconcile_does_not_cross_contaminate_entities() {
        let existing = vec![
            fact("fa", "atlas", "Atlas", "status", "in-progress", None),
            fact("fb", "borealis", "Borealis", "status", "blocked", None),
        ];
        let at = "2026-06-20T00:00:00Z";
        // Atlas → shipped (change); Borealis → blocked (same → NoOp).
        let ops = reconcile_facts(
            &existing,
            &[
                cand("atlas", "Atlas", "status", "shipped"),
                cand("borealis", "Borealis", "status", "blocked"),
            ],
            at,
        );
        // Atlas: Invalidate fa + Add shipped. Borealis: NoOp. Borealis's fb is NEVER invalidated.
        assert!(ops
            .iter()
            .any(|o| matches!(o, FactOp::Invalidate { id, .. } if id == "fa")));
        assert!(ops.iter().any(
            |o| matches!(o, FactOp::Add(nf) if nf.entity_id == "atlas" && nf.object == "shipped")
        ));
        assert!(
            !ops.iter()
                .any(|o| matches!(o, FactOp::Invalidate { id, .. } if id == "fb")),
            "another entity's open fact must never be invalidated by this entity's change"
        );
        assert!(
            ops.contains(&FactOp::NoOp),
            "the unchanged entity's fact is a NoOp"
        );
    }

    /// Malformed candidates (empty fields) are skipped — best-effort extraction can emit junk.
    #[test]
    fn reconcile_skips_malformed_candidates() {
        let ops = reconcile_facts(
            &[],
            &[
                cand("", "Atlas", "status", "shipped"),
                cand("atlas", "", "status", "shipped"),
                cand("atlas", "Atlas", "", "shipped"),
                cand("atlas", "Atlas", "status", ""),
            ],
            "2026-06-10T00:00:00Z",
        );
        assert!(ops.is_empty(), "every malformed candidate must be skipped");
    }

    /// candidates_from_triples resolves entity names case-insensitively to ids and drops unknowns.
    #[test]
    fn triples_resolve_to_known_entities_only() {
        let entities = vec![("id-atlas".to_string(), "Atlas".to_string())];
        let triples = vec![
            RawTriple {
                entity: "atlas".into(),
                predicate: "status".into(),
                object: "shipped".into(),
            },
            RawTriple {
                entity: "Unknown".into(),
                predicate: "status".into(),
                object: "x".into(),
            },
            RawTriple {
                entity: "Atlas".into(),
                predicate: "".into(),
                object: "x".into(),
            },
        ];
        let cands = candidates_from_triples(triples, &entities);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].entity_id, "id-atlas");
        assert_eq!(cands[0].subject, "Atlas"); // canonical casing
        assert_eq!(cands[0].object, "shipped");
    }

    /// set_meeting_id stamps the source meeting onto Add ops only.
    #[test]
    fn set_meeting_id_stamps_adds() {
        let mut ops = reconcile_facts(
            &[],
            &[cand("atlas", "Atlas", "status", "shipped")],
            "2026-06-10T00:00:00Z",
        );
        set_meeting_id(&mut ops, "m42");
        match &ops[0] {
            FactOp::Add(nf) => assert_eq!(nf.meeting_id.as_deref(), Some("m42")),
            other => panic!("expected Add, got {other:?}"),
        }
    }
}
