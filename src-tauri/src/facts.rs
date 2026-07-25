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

use crate::reason::{GenOptions, LocalReasoner};

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
/// in the stored row; this is only the dedup/supersession key. `pub(crate)` so the Vault Audit's
/// stale/contradiction passes key facts IDENTICALLY to this reconcile (`crate::audit`).
pub(crate) fn norm(s: &str) -> String {
    s.trim().to_lowercase()
}

/// THE DETERMINISTIC CORE (no LLM, no DB, no clock — `at` is injected). Reconcile `candidates`
/// against the `existing` facts, producing the ops that keep the bitemporal store consistent.
///
/// For each candidate, the matching OPEN fact is the one with the same
/// `(entity_id, norm(subject), norm(predicate))` and `valid_to IS NULL`:
///   * **no match** → [`FactOp::Add`] (valid_from = recorded_at = `at`, open),
///   * **match, SAME object** → [`FactOp::NoOp`],
///   * **match, DIFFERENT object, and the open fact's `valid_from` STRICTLY precedes `at`** →
///     [`FactOp::Invalidate`] the old (valid_to = `at`) **and** [`FactOp::Add`] the new
///     (valid_from = `at`, open) — the old fact STAYS, closed, so history is preserved,
///   * **match, DIFFERENT object, but the open fact's `valid_from` is NOT before `at`** (same-instant
///     re-processing of one meeting — e.g. a PL→EN translation of the same fact on the meeting's
///     stable `started_at`, or an out-of-order earlier candidate) → [`FactOp::NoOp`]: superseding
///     here would mint a zero/negative-duration closed row = false bitemporal history.
///
/// Determinism + within-batch safety: a working view of the currently-open object per key is
/// threaded through the candidate loop, starting from `existing` and updated as ops are emitted, so
/// two candidates with the same key in ONE batch can't both Add an open duplicate. Malformed
/// candidates (empty entity/subject/predicate/object) are skipped (best-effort extraction can emit
/// junk). Closed (`valid_to.is_some()`) existing facts are ignored — only open facts are matchable.
pub fn reconcile_facts(existing: &[Fact], candidates: &[FactCandidate], at: &str) -> Vec<FactOp> {
    use std::collections::HashMap;
    // key -> (id-of-open-row-if-from-existing, normalized current object, its valid_from). `None` id
    // means the open fact was created earlier IN THIS BATCH (no row id yet) and so cannot be
    // Invalidated. The valid_from is carried so the DIFFERENT-object arm can refuse a supersession
    // that would not STRICTLY post-date the open fact (a zero/negative-duration closed row = false
    // history — e.g. a PL→EN self-flip on the SAME meeting instant).
    let mut open: HashMap<(String, String, String), (Option<String>, String, String)> =
        HashMap::new();
    for f in existing {
        if f.valid_to.is_some() {
            continue; // only OPEN facts are matchable.
        }
        let key = (f.entity_id.clone(), norm(&f.subject), norm(&f.predicate));
        open.insert(
            key,
            (Some(f.id.clone()), norm(&f.object), f.valid_from.clone()),
        );
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
            Some((_, prev_obj, _)) if prev_obj == nobj => ops.push(FactOp::NoOp),
            Some((maybe_id, _, existing_vf)) => {
                // A fact can only be superseded by a LATER one. If the open fact's valid_from is not
                // STRICTLY before `at` (same-instant re-processing of ONE meeting → a PL→EN self-flip
                // on the meeting's stable started_at, or an out-of-order earlier candidate), an
                // Invalidate+Add would mint a zero/negative-duration closed row = false bitemporal
                // history. Keep the already-open fact (NoOp). Forward-only: stored rows untouched.
                if cmp_instant(&existing_vf, at) == std::cmp::Ordering::Less {
                    if let Some(id) = maybe_id {
                        ops.push(FactOp::Invalidate {
                            id,
                            valid_to: at.to_string(),
                        });
                    }
                    ops.push(FactOp::Add(mk_new()));
                } else {
                    ops.push(FactOp::NoOp);
                }
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

// ── Knowledge Diff (Brain v3 PR-6) — deterministic bitemporal AS-OF + set algebra ───────────────
//
// A PRODUCT surface over the SHIPPED bitemporal store: "what did I know AS OF `from`" vs
// "AS OF `to`", plus the chronological decision ledger (each supersession = old fact → new fact →
// source meeting). All PURE + clock-injected (the caller passes the two instants) so the whole core
// is headless-testable, exactly like [`reconcile_facts`]. The gating lives upstream: these functions
// operate on the ALREADY-visibility-gated fact slice from [`crate::storage::Db::list_facts_visible`],
// so a sealed-and-not-session-unlocked meeting's fact never enters here.

/// One END of a supersession pair in a [`KnowledgeDiff`] change (or one added/removed fact),
/// projected down to what a decision ledger needs: the human-readable value + provenance. The
/// object's ORIGINAL casing is preserved (the `norm` key is only the identity, never the display).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FactStateChange {
    /// Entity name at assertion time (the fact's `subject`).
    pub subject: String,
    /// The attribute (the fact's `predicate`), e.g. "status", "owner".
    pub predicate: String,
    /// For `added`: `None`. For `removed`: the object that was current. For `changed`: the OLD object.
    pub old_object: Option<String>,
    /// For `removed`: `None`. For `added`: the object now current. For `changed`: the NEW object.
    pub new_object: Option<String>,
    /// The valid-time start of the state now in effect at snapshot `b` (added/changed), or of the
    /// state that WAS in effect at `a` for a `removed` row — carries the ledger's date.
    pub valid_from: String,
    /// The source meeting the (new, for changed/added; removed's) fact was learned from — the
    /// gating + provenance anchor. `None` only for legacy unattributed rows (already gate-dropped
    /// upstream, so in practice always `Some`).
    pub source_meeting_id: Option<String>,
}

/// The deterministic diff of two [`snapshot_as_of`] snapshots, keyed by `(norm(subject),
/// norm(predicate))`: an attribute present in `b` but not `a` is **added**; present in `a` but not
/// `b` is **removed**; present in both with a DIFFERENT (normalized) object is **changed**
/// (old → new). Same key + same object = no entry (unchanged). Every list is sorted deterministically
/// by `(norm(subject), norm(predicate))` so the output is stable for tests + the FE.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeDiff {
    pub added: Vec<FactStateChange>,
    pub removed: Vec<FactStateChange>,
    pub changed: Vec<FactStateChange>,
}

/// DETERMINISTIC ordering of two RFC3339 timestamps as INSTANTS. Both sides are parsed to
/// `DateTime<Utc>` and compared on the timeline, so the two equally-valid RFC3339 renderings of the
/// SAME moment compare EQUAL regardless of surface form — `Z` vs `+00:00`, and fractional-second
/// digits (`...00:00Z` vs `...00:00.000000000+00:00`). A naive byte-lexical compare would NOT: `Z`
/// (0x5A) sorts above `+` (0x2B), and differing fractional digits reorder identical instants.
///
/// FALLBACK (mirrors [`crate::memory::compute_recency`]'s never-panic posture) — but as a genuine
/// TOTAL order: the comparison is two-class, `(0, instant)` for parseable timestamps then
/// `(1, bytes)` for unparseable ones. Every parseable timestamp sorts BEFORE every unparseable
/// one; parseable pairs compare on the instant, unparseable pairs compare byte-lexically. A junk/
/// corrupt timestamp still never panics and never drops a fact — it gets a defined, reproducible
/// position (after all real instants).
///
/// Why the classes (and not "fall back to bytes when EITHER side is unparseable", the pre-fix
/// posture): mixing instant-compares with byte-compares across a pair set is NOT transitive —
/// x = "2026-06-20T00:00:00Z", y = "2026-06-19T20:00:00-05:00" (the later instant, byte-lexically
/// smaller) and junk j = "2026-06-19T21:junk" gave cmp(x,y)=Less, cmp(y,j)=Less, cmp(j,x)=Less, a
/// cycle — and `sort_by` over a non-total comparator may panic or return permutation-dependent
/// output (the ledger sorts ride this comparator). Test:
/// `cmp_instant_is_total_on_mixed_parseable_and_junk`.
fn cmp_instant(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (
        chrono::DateTime::parse_from_rfc3339(a),
        chrono::DateTime::parse_from_rfc3339(b),
    ) {
        // Compare on the instant (with_timezone(&Utc) normalizes any offset to the timeline).
        (Ok(pa), Ok(pb)) => pa
            .with_timezone(&chrono::Utc)
            .cmp(&pb.with_timezone(&chrono::Utc)),
        // Cross-class: parseable sorts before unparseable — never a byte compare against an
        // instant-compared value (that mix is what broke transitivity).
        (Ok(_), Err(_)) => Ordering::Less,
        (Err(_), Ok(_)) => Ordering::Greater,
        // Both unparseable → deterministic byte-lexical order within the junk class.
        (Err(_), Err(_)) => a.cmp(b),
    }
}

/// The facts OPEN (valid) at instant `at`: `valid_from <= at AND (valid_to IS NULL OR valid_to >
/// at)`. Pure, clock-injected (`at` is the caller's instant — no `now()`). Timestamps are compared
/// as INSTANTS via [`cmp_instant`], NOT byte-lexically, so a fact stored in `+00:00` fractional form
/// classifies correctly against a caller `at` in `Z` form (and vice versa) at the same moment — the
/// FE date-range picker may pass either rendering. Boundary semantics match the bitemporal
/// convention: a fact is OPEN on its `valid_from` (inclusive) and CLOSED exactly at its `valid_to`
/// (`valid_to > at`, so `at == valid_to` excludes it — the superseding fact is the one open at that
/// instant instead).
pub fn snapshot_as_of<'a>(facts: &'a [Fact], at: &str) -> Vec<&'a Fact> {
    use std::cmp::Ordering;
    facts
        .iter()
        .filter(|f| {
            // valid_from <= at
            cmp_instant(&f.valid_from, at) != Ordering::Greater
                && match &f.valid_to {
                    None => true,
                    // valid_to > at
                    Some(vt) => cmp_instant(vt, at) == Ordering::Greater,
                }
        })
        .collect()
}

/// The identity key for diffing/ledgering a fact: `(norm(subject), norm(predicate))` — the SAME
/// normalization [`reconcile_facts`] uses, so a diff key lines up 1:1 with a supersession key.
fn diff_key(f: &Fact) -> (String, String) {
    (norm(&f.subject), norm(&f.predicate))
}

/// Project a `Fact` into a [`FactStateChange`] for one side of the diff. `old`/`new` decide which of
/// old_object/new_object carries this fact's object.
fn to_change(f: &Fact, old: Option<String>, new: Option<String>) -> FactStateChange {
    FactStateChange {
        subject: f.subject.clone(),
        predicate: f.predicate.clone(),
        old_object: old,
        new_object: new,
        valid_from: f.valid_from.clone(),
        source_meeting_id: f.meeting_id.clone(),
    }
}

/// DETERMINISTIC set algebra over two snapshots (each a slice of the SAME entity's facts, from
/// [`snapshot_as_of`] at two instants `a` < `b`). Keyed by `(norm(subject), norm(predicate))`:
///   * in `b` not `a` → **added** (new_object = b's object),
///   * in `a` not `b` → **removed** (old_object = a's object),
///   * in both, DIFFERENT normalized object → **changed** (old = a's object, new = b's object;
///     provenance from the NEW/`b` fact — its valid_from + source meeting),
///   * in both, SAME normalized object → no entry.
///
/// Within a snapshot a key can only be open once (one currently-valid fact per key, by the reconcile
/// invariant); if a malformed input repeats a key the FIRST occurrence wins deterministically. All
/// three output lists are sorted by key for stable, test-friendly output.
pub fn diff_snapshots(a: &[&Fact], b: &[&Fact]) -> KnowledgeDiff {
    use std::collections::BTreeMap;
    // BTreeMap keeps the keys sorted → deterministic output without a post-sort.
    let mut ma: BTreeMap<(String, String), &Fact> = BTreeMap::new();
    for f in a {
        ma.entry(diff_key(f)).or_insert(f);
    }
    let mut mb: BTreeMap<(String, String), &Fact> = BTreeMap::new();
    for f in b {
        mb.entry(diff_key(f)).or_insert(f);
    }

    let mut diff = KnowledgeDiff::default();
    // added / changed: iterate b's keys (sorted).
    for (key, fb) in &mb {
        match ma.get(key) {
            None => diff
                .added
                .push(to_change(fb, None, Some(fb.object.clone()))),
            Some(fa) => {
                if norm(&fa.object) != norm(&fb.object) {
                    diff.changed.push(to_change(
                        fb,
                        Some(fa.object.clone()),
                        Some(fb.object.clone()),
                    ));
                }
            }
        }
    }
    // removed: keys in a not in b (sorted).
    for (key, fa) in &ma {
        if !mb.contains_key(key) {
            diff.removed
                .push(to_change(fa, Some(fa.object.clone()), None));
        }
    }
    diff
}

/// The chronological DECISION LEDGER for one entity: every supersession (a fact CLOSED because a
/// later fact with the same key opened with a different object), oldest → newest by the NEW fact's
/// `valid_from`. Each entry carries `old_object` (the closed fact's value), `new_object` (the value
/// that replaced it), `valid_from` (when the new value took effect) and `source_meeting_id` (the
/// meeting the new value was learned from). Pure + deterministic over the visibility-gated fact
/// slice — a sealed-not-unlocked meeting's fact is already absent, so its supersession never appears.
///
/// Derivation: group by key, sort each group by `(valid_from, id)`, and emit one ledger row per
/// adjacent (older, newer) pair whose normalized objects differ. Adjacent pairs with the SAME object
/// (a re-assertion) are not decisions and are skipped. BOTH sorts compare `valid_from` as an
/// INSTANT ([`cmp_instant`], deterministic id / key tiebreaks) — a byte-lexical sort misorders a
/// fact stored in a different RFC3339 rendering (`Z` vs `+00:00` vs a foreign offset, e.g. an
/// imported/shared-meeting fact), which pairs `windows(2)` backwards and renders the decision
/// INVERTED (test: `ledger_orders_by_instant_not_bytes`).
pub fn supersession_ledger(facts: &[Fact]) -> Vec<FactStateChange> {
    use std::collections::BTreeMap;
    let mut by_key: BTreeMap<(String, String), Vec<&Fact>> = BTreeMap::new();
    for f in facts {
        by_key.entry(diff_key(f)).or_default().push(f);
    }
    let mut rows: Vec<FactStateChange> = Vec::new();
    for group in by_key.values_mut() {
        group.sort_by(|x, y| {
            cmp_instant(&x.valid_from, &y.valid_from).then_with(|| x.id.cmp(&y.id))
        });
        for pair in group.windows(2) {
            let (older, newer) = (pair[0], pair[1]);
            if norm(&older.object) != norm(&newer.object) {
                rows.push(to_change(
                    newer,
                    Some(older.object.clone()),
                    Some(newer.object.clone()),
                ));
            }
        }
    }
    // Chronological across ALL keys: oldest decision first, ties by (subject, predicate) for stability.
    rows.sort_by(|x, y| {
        cmp_instant(&x.valid_from, &y.valid_from)
            .then_with(|| norm(&x.subject).cmp(&norm(&y.subject)))
            .then_with(|| norm(&x.predicate).cmp(&norm(&y.predicate)))
    });
    rows
}

/// The IPC/MCP payload for [`build_knowledge_diff`]: the between-two-instants set diff PLUS the full
/// chronological decision ledger for one entity. All state comes from the visibility-gated fact
/// slice, so a sealed-and-not-session-unlocked meeting's fact is absent from every field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EntityKnowledgeDiff {
    pub entity_id: String,
    /// The `from` instant the caller asked to snapshot at (echoed for the FE).
    pub from: String,
    /// The `to` instant the caller asked to snapshot at (echoed for the FE).
    pub to: String,
    /// added / removed / changed between the `from` and `to` snapshots.
    pub diff: KnowledgeDiff,
    /// Every supersession for this entity, oldest → newest — the decision ledger (independent of the
    /// from/to window: it is the entity's whole history, so the FE can render the full timeline).
    pub ledger: Vec<FactStateChange>,
}

/// GATED builder for the Knowledge Diff of one ALREADY-RESOLVED entity id. Reads the entity's facts
/// through the EXISTING visibility-gated reader [`crate::storage::Db::list_facts_visible`] — a
/// sealed-and-not-session-unlocked meeting's fact never enters, so it can appear in NO snapshot, diff
/// entry, or ledger row. The interval algebra ([`snapshot_as_of`] + [`diff_snapshots`]) and the
/// [`supersession_ledger`] are the deterministic, clock-injected pure core; this function only
/// glues the gated read to them. `from`/`to` are ISO-8601 instants (the FE passes the two dates it
/// is comparing). A REVERSED range (`from` later than `to`, by instant) is normalized by SWAPPING
/// the bounds — the diff always answers "earlier vs later" and the echoed `from`/`to` reflect the
/// normalized window, the same way [`normalize_instant`] already echoes canonical renderings — so
/// a caller mixing the arguments up can never silently receive inverted added/removed/changed
/// semantics. (Unparseable bounds keep the lexical-fallback posture: compared with [`cmp_instant`],
/// never a panic.)
pub fn build_knowledge_diff(
    db: &crate::storage::Db,
    entity_id: &str,
    from: &str,
    to: &str,
    unlocked: &std::collections::HashSet<String>,
) -> crate::error::Result<EntityKnowledgeDiff> {
    // Normalize the FE-supplied range to a canonical UTC rendering so a `Z`-form date works against
    // stored `+00:00` timestamps regardless of surface form. Unparseable input passes through
    // unchanged (deterministic — the instant-aware compare in `snapshot_as_of` still degrades to a
    // lexical fallback for it, never a panic).
    let mut from_norm = normalize_instant(from);
    let mut to_norm = normalize_instant(to);
    // Reversed range ⇒ swap (see the fn doc): the diff is always earlier-vs-later, never silently
    // inverted added/removed/changed semantics.
    if cmp_instant(&from_norm, &to_norm) == std::cmp::Ordering::Greater {
        std::mem::swap(&mut from_norm, &mut to_norm);
    }
    // THE GATE: the single user-facing fact read. Every downstream projection is over THIS slice.
    let facts = db.list_facts_visible(entity_id, unlocked)?;
    let snap_from = snapshot_as_of(&facts, &from_norm);
    let snap_to = snapshot_as_of(&facts, &to_norm);
    let diff = diff_snapshots(&snap_from, &snap_to);
    let ledger = supersession_ledger(&facts);
    Ok(EntityKnowledgeDiff {
        entity_id: entity_id.to_string(),
        from: from_norm,
        to: to_norm,
        diff,
        ledger,
    })
}

/// Canonicalize an RFC3339 instant to a stable UTC (`Z`) rendering. A parseable timestamp — in ANY
/// valid RFC3339 form (`Z`, `+00:00`, an offset, with/without fractional seconds) — is re-serialized
/// to a single canonical UTC string so echoed `from`/`to` and downstream compares are consistent.
/// FALLBACK (same posture as [`cmp_instant`] / [`crate::memory::compute_recency`]): an UNPARSEABLE
/// string is returned unchanged — never a panic, never a lossy rewrite; the instant-aware compares
/// downstream still handle it via their own lexical fallback.
fn normalize_instant(ts: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(ts) {
        Ok(dt) => dt
            .with_timezone(&chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        Err(_) => ts.to_string(),
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

/// [`EXTRACT_SYSTEM`] + a LANGUAGE directive that pins the extractor's OUTPUT language so a
/// Polish-dominant note can never emit the SAME fact twice (a Polish `rola:` AND an English `role:`
/// twin — two different reconcile keys that dedup can't merge). PURE + headless-testable.
///
/// A pinned `note_language` (e.g. `"pl"` → "Polish", via [`crate::summarize::template::language_name`])
/// forces every predicate/object into that ONE language; `"auto"`/`""` instead pins to "the same
/// language as the note", still ONE consistent language. Both variants forbid a two-language twin and
/// PROTECT the entity name (kept verbatim so the case-insensitive entity match in
/// [`candidates_from_triples`] still resolves).
fn extract_system_prompt(note_language: &str) -> String {
    match crate::summarize::template::language_name(note_language) {
        Some(name) => format!(
            "{EXTRACT_SYSTEM}\n\
LANGUAGE: Write EVERY predicate and object in {name}. Use ONE language for all facts. \
NEVER output the same fact twice in two languages (never emit both a {name} and an English version \
of one attribute). Keep the ENTITY name EXACTLY as listed — do not translate it."
        ),
        None => format!(
            "{EXTRACT_SYSTEM}\n\
LANGUAGE: Write predicates and objects in the SAME language as the NOTE below; use ONE consistent \
language for all facts; never emit the same fact in two languages. Keep the ENTITY name EXACTLY as \
listed — do not translate it."
        ),
    }
}

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
    note_language: &str,
    opts: GenOptions,
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

    let value = match reasoner.structured_with(&extract_system_prompt(note_language), &user, &schema, opts)
    {
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

    /// LEVER A (RED-before-GREEN) — the extractor system prompt is LANGUAGE-PINNED. A pinned code
    /// (`"pl"`) names the concrete language AND forbids a two-language twin; `"auto"` pins to the
    /// note's own language, still one consistent language. This is the only lever that can stop the
    /// within-note PL+EN twin ("rola:"/"role:"), since a language-insensitive reconcile key is
    /// infeasible without translation/embeddings. RED before `extract_system_prompt` existed.
    #[test]
    fn extract_system_prompt_pins_output_language() {
        let pl = extract_system_prompt("pl");
        assert!(pl.contains("Polish"), "a pinned code names the language: {pl}");
        assert!(
            pl.contains("ONE language for all facts"),
            "one-language instruction present: {pl}"
        );
        assert!(
            pl.contains("NEVER output the same fact twice in two languages"),
            "no-duplicate-language instruction present: {pl}"
        );
        // The entity name must be protected so candidates_from_triples still resolves it.
        assert!(
            pl.contains("Keep the ENTITY name EXACTLY"),
            "entity-name protection present: {pl}"
        );

        let auto = extract_system_prompt("auto");
        assert!(
            auto.contains("SAME language as the NOTE"),
            "auto pins to the note's language: {auto}"
        );
        assert!(
            auto.contains("ONE consistent language"),
            "auto still forces one consistent language: {auto}"
        );
        // Empty string behaves like "auto" (no pin).
        assert!(extract_system_prompt("").contains("SAME language as the NOTE"));
    }

    /// LEVER B (RED-before-GREEN, core of symptom 2) — same-day SELF-supersession guard. An OPEN
    /// existing fact whose `valid_from` EQUALS `at` (the meeting's stable started_at, re-extracted on
    /// multiple funnels) reconciled against a DIFFERENT-language object at the SAME `at` must NOT
    /// close the open row: an Invalidate+Add there mints `valid_from == valid_to == at`, a
    /// zero-duration closed fact = false bitemporal history (the observed "deadline: was '…'
    /// (2026-07-22 -> 2026-07-22)" WHAT-CHANGED row). The guard keeps the open fact (NoOp). RED on
    /// the pre-guard code (it emitted Invalidate + Add).
    #[test]
    fn reconcile_does_not_self_supersede_at_the_same_instant() {
        let at = "2026-07-22T09:00:00Z";
        let existing = vec![dated_fact(
            "f1",
            "Klaudia",
            "deadline",
            "skończyć projekt w tym tygodniu",
            at,
            None,
            "m1",
        )];
        let ops = reconcile_facts(
            &existing,
            &[cand("atlas", "Klaudia", "deadline", "finish project this week")],
            at,
        );
        assert_eq!(
            ops,
            vec![FactOp::NoOp],
            "a same-instant object flip must not mint a zero-duration closed row"
        );
    }

    /// LEVER B — the guard must ALSO block a supersession whose open fact's valid_from is AFTER `at`
    /// (an out-of-order earlier candidate) — that would be a NEGATIVE-duration closed row.
    #[test]
    fn reconcile_does_not_supersede_when_open_fact_postdates_at() {
        let existing = vec![dated_fact(
            "f1",
            "Klaudia",
            "deadline",
            "later value",
            "2026-07-25T00:00:00Z", // AFTER `at`
            None,
            "m1",
        )];
        let ops = reconcile_facts(
            &existing,
            &[cand("atlas", "Klaudia", "deadline", "earlier value")],
            "2026-07-22T09:00:00Z",
        );
        assert_eq!(ops, vec![FactOp::NoOp], "a negative-duration close is refused");
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

    // ── Knowledge Diff (PR-6) ───────────────────────────────────────────────────

    /// A closed fact with explicit valid_from/valid_to, for as-of interval tests.
    fn dated_fact(
        id: &str,
        subject: &str,
        predicate: &str,
        object: &str,
        valid_from: &str,
        valid_to: Option<&str>,
        meeting: &str,
    ) -> Fact {
        Fact {
            id: id.to_string(),
            entity_id: "atlas".to_string(),
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            valid_from: valid_from.to_string(),
            valid_to: valid_to.map(|s| s.to_string()),
            recorded_at: valid_from.to_string(),
            meeting_id: Some(meeting.to_string()),
            confidence: 1.0,
        }
    }

    fn ids(fs: &[&Fact]) -> Vec<String> {
        let mut v: Vec<String> = fs.iter().map(|f| f.id.clone()).collect();
        v.sort();
        v
    }

    /// snapshot_as_of: `valid_from <= at`. A fact whose valid_from is AFTER `at` is not yet open.
    #[test]
    fn snapshot_excludes_facts_not_yet_valid() {
        let facts = vec![dated_fact(
            "f1",
            "Atlas",
            "status",
            "shipped",
            "2026-06-20T00:00:00Z",
            None,
            "m1",
        )];
        // Before it became true.
        let open = snapshot_as_of(&facts, "2026-06-10T00:00:00Z");
        assert!(open.is_empty(), "a fact is not open before its valid_from");
        // On its valid_from (inclusive) it IS open.
        let open2 = snapshot_as_of(&facts, "2026-06-20T00:00:00Z");
        assert_eq!(ids(&open2), vec!["f1".to_string()]);
    }

    /// snapshot_as_of compares INSTANTS, not bytes: a fact stored in `Z` form is OPEN when the
    /// caller's `at` is the SAME instant rendered in `+00:00` form. This is the lexical-timestamp
    /// bug (the FE date-range picker may pass either surface form): a naive `valid_from.as_str() <=
    /// at` byte-compare puts `Z` (0x5A) ABOVE `+` (0x2B), so it would read `valid_from > at` for two
    /// equal moments and WRONGLY exclude the fact — RED on the old code, GREEN on the instant compare.
    #[test]
    fn snapshot_matches_same_instant_across_z_and_offset_forms() {
        // valid_from in Z form...
        let facts = vec![dated_fact(
            "f1",
            "Atlas",
            "status",
            "shipped",
            "2026-06-20T00:00:00Z",
            None,
            "m1",
        )];
        // ...and `at` the SAME instant in +00:00 form. Naive lexical: "…Z" > "…+00:00" → excluded.
        let at_offset = "2026-06-20T00:00:00+00:00";
        // Sanity: the two renderings really ARE the same instant.
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339("2026-06-20T00:00:00Z").unwrap(),
            chrono::DateTime::parse_from_rfc3339(at_offset).unwrap(),
            "test fixture must be the same instant in two RFC3339 forms"
        );
        let open = snapshot_as_of(&facts, at_offset);
        assert_eq!(
            ids(&open),
            vec!["f1".to_string()],
            "a fact open AT this instant must be included regardless of Z vs +00:00 rendering"
        );

        // The valid_to boundary must ALSO be instant-exact: a closed fact whose valid_to is the same
        // instant as `at` (different surface form) is EXCLUDED (valid_to is exclusive).
        let closed = vec![dated_fact(
            "f_closed",
            "Atlas",
            "status",
            "in-progress",
            "2026-06-01T00:00:00Z",
            Some("2026-06-20T00:00:00Z"),
            "m1",
        )];
        assert!(
            snapshot_as_of(&closed, at_offset).is_empty(),
            "at == valid_to (same instant, different form) must exclude the closed fact"
        );
    }

    /// A corrupt/unparseable stored timestamp must NOT panic and must be deterministic — the
    /// instant-compare falls back to a byte-lexical ordering for that pair (mirrors compute_recency's
    /// neutral-default posture). Here a junk `valid_from` simply can't be greater-than the numeric
    /// `at` lexically, so it stays included — the point is: no crash, a defined outcome.
    #[test]
    fn snapshot_tolerates_unparseable_timestamp_without_panic() {
        let facts = vec![dated_fact(
            "junk",
            "Atlas",
            "status",
            "shipped",
            "not-a-timestamp",
            None,
            "m1",
        )];
        // Must return deterministically (no panic) for a well-formed `at`.
        let _ = snapshot_as_of(&facts, "2026-06-20T00:00:00Z");
    }

    /// snapshot_as_of: an OPEN fact (valid_to == None) is open at any instant >= valid_from.
    #[test]
    fn snapshot_includes_open_fact_forever_after() {
        let facts = vec![dated_fact(
            "f1",
            "Atlas",
            "status",
            "shipped",
            "2026-06-01T00:00:00Z",
            None,
            "m1",
        )];
        assert_eq!(
            ids(&snapshot_as_of(&facts, "2030-01-01T00:00:00Z")),
            vec!["f1".to_string()]
        );
    }

    /// snapshot_as_of BOUNDARY: at == valid_to the closed fact is EXCLUDED (valid_to is exclusive),
    /// and its successor (open at that instant) is the one returned. This is the load-bearing
    /// half-open-interval semantics — RED-before-GREEN on the boundary.
    #[test]
    fn snapshot_boundary_at_equals_valid_to_excludes_closed_includes_successor() {
        let cut = "2026-06-20T00:00:00Z";
        let facts = vec![
            // in-progress until the cut...
            dated_fact(
                "f_old",
                "Atlas",
                "status",
                "in-progress",
                "2026-06-01T00:00:00Z",
                Some(cut),
                "m1",
            ),
            // ...shipped from the cut on.
            dated_fact("f_new", "Atlas", "status", "shipped", cut, None, "m2"),
        ];
        // Exactly AT the cut: the closed fact is gone (valid_to == at → excluded), the new one open.
        let at_cut = snapshot_as_of(&facts, cut);
        assert_eq!(
            ids(&at_cut),
            vec!["f_new".to_string()],
            "at == valid_to must exclude the closed fact and include its successor"
        );
        // Just BEFORE the cut: the closed fact is still open, the new one not yet valid.
        let before = snapshot_as_of(&facts, "2026-06-19T23:59:59Z");
        assert_eq!(ids(&before), vec!["f_old".to_string()]);
    }

    /// diff_snapshots ADDED / REMOVED / CHANGED, keyed by (norm(subject), norm(predicate)).
    #[test]
    fn diff_classifies_added_removed_changed() {
        // Snapshot A (earlier): Atlas.status=in-progress, Atlas.owner=Anna.
        let a_facts = [
            dated_fact(
                "a1",
                "Atlas",
                "status",
                "in-progress",
                "2026-06-01T00:00:00Z",
                None,
                "m1",
            ),
            dated_fact(
                "a2",
                "Atlas",
                "owner",
                "Anna",
                "2026-06-01T00:00:00Z",
                None,
                "m1",
            ),
        ];
        // Snapshot B (later): Atlas.status=shipped (CHANGED), Atlas.owner gone (REMOVED),
        // Atlas.deadline=2026-07-01 (ADDED). Casing/whitespace differs on the key → still same key.
        let b_facts = [
            dated_fact(
                "b1",
                "Atlas",
                "Status", // different casing, same normalized key
                "shipped",
                "2026-06-20T00:00:00Z",
                None,
                "m2",
            ),
            dated_fact(
                "b2",
                "Atlas",
                "deadline",
                "2026-07-01",
                "2026-06-20T00:00:00Z",
                None,
                "m2",
            ),
        ];
        let a: Vec<&Fact> = a_facts.iter().collect();
        let b: Vec<&Fact> = b_facts.iter().collect();
        let diff = diff_snapshots(&a, &b);

        assert_eq!(diff.changed.len(), 1, "status changed");
        assert_eq!(diff.changed[0].predicate, "Status"); // display casing preserved from b
        assert_eq!(diff.changed[0].old_object.as_deref(), Some("in-progress"));
        assert_eq!(diff.changed[0].new_object.as_deref(), Some("shipped"));
        assert_eq!(diff.changed[0].valid_from, "2026-06-20T00:00:00Z");
        assert_eq!(diff.changed[0].source_meeting_id.as_deref(), Some("m2"));

        assert_eq!(diff.added.len(), 1, "deadline added");
        assert_eq!(diff.added[0].predicate, "deadline");
        assert_eq!(diff.added[0].old_object, None);
        assert_eq!(diff.added[0].new_object.as_deref(), Some("2026-07-01"));

        assert_eq!(diff.removed.len(), 1, "owner removed");
        assert_eq!(diff.removed[0].predicate, "owner");
        assert_eq!(diff.removed[0].old_object.as_deref(), Some("Anna"));
        assert_eq!(diff.removed[0].new_object, None);
    }

    /// diff_snapshots: same key, same normalized object across both snapshots → NO entry (unchanged).
    #[test]
    fn diff_ignores_unchanged_key() {
        let a_facts = [dated_fact(
            "a1",
            "Atlas",
            "status",
            "shipped",
            "2026-06-01T00:00:00Z",
            None,
            "m1",
        )];
        // Casing-only difference in the OBJECT is not a change (norm equal).
        let b_facts = [dated_fact(
            "b1",
            "Atlas",
            "status",
            "Shipped",
            "2026-06-20T00:00:00Z",
            None,
            "m2",
        )];
        let diff = diff_snapshots(
            &a_facts.iter().collect::<Vec<_>>(),
            &b_facts.iter().collect::<Vec<_>>(),
        );
        assert!(diff.added.is_empty() && diff.removed.is_empty() && diff.changed.is_empty());
    }

    /// supersession_ledger: the chronological decision list — one row per (older→newer) object
    /// change per key, oldest first, carrying old/new/valid_from/source meeting.
    #[test]
    fn ledger_lists_supersessions_chronologically() {
        // Atlas.status: in-progress → shipped → deprecated (two decisions).
        let facts = vec![
            dated_fact(
                "f3",
                "Atlas",
                "status",
                "deprecated",
                "2026-07-05T00:00:00Z",
                None,
                "m3",
            ),
            dated_fact(
                "f1",
                "Atlas",
                "status",
                "in-progress",
                "2026-06-01T00:00:00Z",
                Some("2026-06-20T00:00:00Z"),
                "m1",
            ),
            dated_fact(
                "f2",
                "Atlas",
                "status",
                "shipped",
                "2026-06-20T00:00:00Z",
                Some("2026-07-05T00:00:00Z"),
                "m2",
            ),
        ];
        let ledger = supersession_ledger(&facts);
        assert_eq!(ledger.len(), 2, "two decisions");
        // Oldest first: in-progress → shipped (learned at m2).
        assert_eq!(ledger[0].old_object.as_deref(), Some("in-progress"));
        assert_eq!(ledger[0].new_object.as_deref(), Some("shipped"));
        assert_eq!(ledger[0].valid_from, "2026-06-20T00:00:00Z");
        assert_eq!(ledger[0].source_meeting_id.as_deref(), Some("m2"));
        // Then shipped → deprecated (learned at m3).
        assert_eq!(ledger[1].old_object.as_deref(), Some("shipped"));
        assert_eq!(ledger[1].new_object.as_deref(), Some("deprecated"));
        assert_eq!(ledger[1].source_meeting_id.as_deref(), Some("m3"));
    }

    /// supersession_ledger: a re-assertion of the SAME object (adjacent pair, same value) is not a
    /// decision and produces no ledger row.
    #[test]
    fn ledger_skips_reassertion_of_same_object() {
        let facts = vec![
            dated_fact(
                "f1",
                "Atlas",
                "status",
                "shipped",
                "2026-06-01T00:00:00Z",
                Some("2026-06-20T00:00:00Z"),
                "m1",
            ),
            dated_fact(
                "f2",
                "Atlas",
                "status",
                "Shipped", // re-asserted (norm equal) — not a decision
                "2026-06-20T00:00:00Z",
                None,
                "m2",
            ),
        ];
        assert!(
            supersession_ledger(&facts).is_empty(),
            "re-asserting the same normalized object is not a supersession"
        );
    }

    /// supersession_ledger orders by INSTANT, not bytes — the `cmp_instant` fix must cover the
    /// ledger's TWO sorts (the per-key group sort AND the final cross-key chronological sort), not
    /// just `snapshot_as_of`. A fact carrying a FOREIGN-OFFSET RFC3339 rendering (an imported /
    /// shared-meeting fact: "2026-06-19T20:00:00-05:00" = 2026-06-20T01:00:00Z) sorts byte-lexically
    /// BELOW an EARLIER Z-form instant ("2026-06-20T00:00:00Z"), so a lexical group sort pairs
    /// (newer, older) backwards and the ledger renders the decision INVERTED (shipped →
    /// in-progress); the final sort misorders rows across keys the same way. RED on byte-lexical
    /// `.cmp()` sorts, GREEN on `cmp_instant`.
    #[test]
    fn ledger_orders_by_instant_not_bytes() {
        // The Z form is the EARLIER instant yet the byte-lexically GREATER string.
        let z_form = "2026-06-20T00:00:00Z"; // instant 2026-06-20T00:00:00Z
        let offset_form = "2026-06-19T20:00:00-05:00"; // instant 2026-06-20T01:00:00Z (LATER)
        assert!(
            chrono::DateTime::parse_from_rfc3339(offset_form).unwrap()
                > chrono::DateTime::parse_from_rfc3339(z_form).unwrap(),
            "fixture: the offset form must be the LATER instant"
        );
        assert!(
            offset_form < z_form,
            "fixture: the offset form must be the byte-lexically SMALLER string"
        );

        let facts = vec![
            // status: in-progress (Z form, 00:00Z) superseded by shipped (offset form, 01:00Z).
            dated_fact(
                "s_old",
                "Atlas",
                "status",
                "in-progress",
                z_form,
                Some(offset_form),
                "m1",
            ),
            dated_fact(
                "s_new",
                "Atlas",
                "status",
                "shipped",
                offset_form,
                None,
                "m2",
            ),
            // owner: Anna → Piotr at 00:30Z — BETWEEN the status instants, so the correct
            // cross-key order is [owner @00:30Z, status @01:00Z]; the lexical order inverts it.
            dated_fact(
                "o_old",
                "Atlas",
                "owner",
                "Anna",
                "2026-06-01T00:00:00Z",
                Some("2026-06-20T00:30:00Z"),
                "m1",
            ),
            dated_fact(
                "o_new",
                "Atlas",
                "owner",
                "Piotr",
                "2026-06-20T00:30:00Z",
                None,
                "m3",
            ),
        ];
        let ledger = supersession_ledger(&facts);
        assert_eq!(ledger.len(), 2, "two decisions");
        // Chronological by INSTANT: the owner decision (00:30Z) first...
        assert_eq!(ledger[0].predicate, "owner");
        assert_eq!(ledger[0].old_object.as_deref(), Some("Anna"));
        assert_eq!(ledger[0].new_object.as_deref(), Some("Piotr"));
        // ...then status (01:00Z), paired the RIGHT way round: the foreign-offset (LATER) fact is
        // the NEW side of the decision, never the old.
        assert_eq!(ledger[1].predicate, "status");
        assert_eq!(
            ledger[1].old_object.as_deref(),
            Some("in-progress"),
            "a lexical group sort pairs the decision backwards (shipped → in-progress)"
        );
        assert_eq!(ledger[1].new_object.as_deref(), Some("shipped"));
        assert_eq!(ledger[1].valid_from, offset_form);
        assert_eq!(ledger[1].source_meeting_id.as_deref(), Some("m2"));
    }

    /// cmp_instant is a TOTAL order even on MIXED parseable/junk timestamps. The pre-fix
    /// comparator fell back to byte-lexical whenever EITHER side was unparseable, which broke
    /// transitivity: x = "2026-06-20T00:00:00Z", y = "2026-06-19T20:00:00-05:00" (the LATER
    /// instant, lexically smaller), j = junk "2026-06-19T21:junk" gave cmp(x,y)=Less (instants),
    /// cmp(y,j)=Less (bytes), cmp(j,x)=Less (bytes) — a CYCLE (x < y < j < x), and `sort_by`
    /// over a non-total comparator is allowed to panic or return permutation-dependent output
    /// (the ledger sorts ride exactly this comparator). The fix makes the order two-class —
    /// (0, instant) for parseable, then (1, bytes) for unparseable — which is genuinely total.
    /// RED on the old comparator: the pairwise compares below WERE the cycle [Less, Less, Less],
    /// and the permutations sorted to different outputs.
    #[test]
    fn cmp_instant_is_total_on_mixed_parseable_and_junk() {
        use std::cmp::Ordering;
        let x = "2026-06-20T00:00:00Z";
        let y = "2026-06-19T20:00:00-05:00"; // later instant than x, byte-lexically smaller
        let j = "2026-06-19T21:junk"; // unparseable

        // Acyclicity of the three pairwise compares (the pre-fix cycle: all three Less).
        let c = [cmp_instant(x, y), cmp_instant(y, j), cmp_instant(j, x)];
        assert_ne!(
            c,
            [Ordering::Less, Ordering::Less, Ordering::Less],
            "cyclic comparator: x < y < j < x"
        );
        assert_ne!(
            c,
            [Ordering::Greater, Ordering::Greater, Ordering::Greater],
            "cyclic comparator: x > y > j > x"
        );

        // Every input permutation must sort to the SAME deterministic order.
        let perms: [[&str; 3]; 6] = [
            [x, y, j],
            [x, j, y],
            [y, x, j],
            [y, j, x],
            [j, x, y],
            [j, y, x],
        ];
        let mut outputs: Vec<Vec<&str>> = Vec::new();
        for perm in perms {
            let mut v = perm.to_vec();
            v.sort_by(|a, b| cmp_instant(a, b));
            outputs.push(v);
        }
        for out in &outputs[1..] {
            assert_eq!(
                out, &outputs[0],
                "sort under cmp_instant must be permutation-independent (total order)"
            );
        }
        // And the total order is the documented one: parseable instants first (x = 00:00Z before
        // y = 01:00Z, by INSTANT), unparseable junk last.
        assert_eq!(outputs[0], vec![x, y, j]);
    }

    /// build_knowledge_diff NORMALIZES a reversed range: when `from` is the LATER instant the
    /// bounds are swapped, so the diff semantics (added/removed/changed direction) are those of
    /// the ordered window and the echoed payload carries the normalized bounds. RED on the old
    /// pass-through (a reversed range silently inverted the semantics: added ↔ removed, changed
    /// old ↔ new) — this pins both the echo and the direction.
    #[test]
    fn knowledge_diff_normalizes_reversed_range() {
        use crate::facts::{FactOp, NewFact};
        use crate::storage::models::{EntityKind, Meeting, MeetingStatus};
        use std::collections::HashSet;
        const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let p = crate::storage::db::unique_temp_path("murmur-facts-kdiff", "sqlite");
        let db = crate::storage::Db::open_with_key(&p, TEST_DEK).unwrap();
        // One visible meeting (no folder, no note ⇒ visible to `list_facts_visible`).
        db.insert_meeting(&Meeting {
            id: "m1".to_string(),
            started_at: "2026-06-01T09:00:00Z".to_string(),
            ended_at: None,
            title: Some("Kickoff".to_string()),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
        db.add_mention(&atlas, "m1").unwrap();
        let add = |object: &str, vf: &str| {
            FactOp::Add(NewFact {
                entity_id: atlas.clone(),
                subject: "Atlas".to_string(),
                predicate: "status".to_string(),
                object: object.to_string(),
                valid_from: vf.to_string(),
                recorded_at: vf.to_string(),
                confidence: 1.0,
                meeting_id: Some("m1".to_string()),
            })
        };
        // Atlas.status: in-progress (06-01, closed 06-20) → shipped (06-20, open).
        db.apply_fact_ops(&[add("in-progress", "2026-06-01T00:00:00Z")])
            .unwrap();
        let ip_id = db
            .facts_for_entities(std::slice::from_ref(&atlas))
            .unwrap()
            .into_iter()
            .find(|f| f.object == "in-progress")
            .expect("in-progress row exists")
            .id;
        db.apply_fact_ops(&[
            FactOp::Invalidate {
                id: ip_id,
                valid_to: "2026-06-20T00:00:00Z".to_string(),
            },
            add("shipped", "2026-06-20T00:00:00Z"),
        ])
        .unwrap();

        // REVERSED bounds: `from` is the LATER instant.
        let kd = build_knowledge_diff(
            &db,
            &atlas,
            "2026-06-25T00:00:00Z",
            "2026-06-10T00:00:00Z",
            &HashSet::new(),
        )
        .unwrap();
        // The payload echoes the NORMALIZED (swapped) window, the way normalize_instant already
        // echoes canonical forms...
        assert_eq!(
            kd.from, "2026-06-10T00:00:00Z",
            "from must echo the EARLIER bound"
        );
        assert_eq!(
            kd.to, "2026-06-25T00:00:00Z",
            "to must echo the LATER bound"
        );
        // ...and the semantics are the ordered window's: status CHANGED in-progress → shipped.
        assert_eq!(kd.diff.changed.len(), 1);
        assert_eq!(
            kd.diff.changed[0].old_object.as_deref(),
            Some("in-progress"),
            "a reversed range must not invert the change direction"
        );
        assert_eq!(kd.diff.changed[0].new_object.as_deref(), Some("shipped"));
        let _ = std::fs::remove_file(&p);
    }
}
