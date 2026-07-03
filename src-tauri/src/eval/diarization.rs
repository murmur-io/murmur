//! DIARIZATION / VOICEPRINT eval harness — the missing reliability floor for speaker attribution.
//!
//! ## Why this exists
//!
//! Murmur ships offline speaker diarization ([`crate::transcribe::diarize`]) plus an opt-in per-
//! cluster CAM++ **voiceprint** and a cosine re-identification matcher gated at the UNVALIDATED
//! placeholder [`crate::transcribe::diarize::VOICEPRINT_MATCH_THRESHOLD`] (`0.5`). Whether the
//! diarizer splits speakers correctly, and whether `0.5` is the right cosine operating point, can
//! ONLY be answered empirically on real multi-speaker audio. This module is that measurement:
//! given hand-labeled reference speaker turns and a diarizer hypothesis it computes
//!
//! - **DER** (Diarization Error Rate = `(miss + false_alarm + confusion) / total_ref_time`, the
//!   NIST decomposition with an optimal 1:1 speaker mapping),
//! - **cluster purity / coverage** (+ their F1),
//! - **re-ID precision / recall** of the shipped voiceprint matcher, and
//! - a **cosine threshold sweep** (FAR/FRR per threshold, EER, best-F1) so the `0.5` placeholder can
//!   be TUNED on gold labels.
//!
//! ## What is / isn't testable headless
//!
//! - The METRIC MATH (this file) is pure and unit-tested with synthetic `Turn` / `ReIdCase` /
//!   `VerificationPair` fixtures — NO models, NO DB, NO FFI. It runs in `cargo test --lib` exactly
//!   like `eval::recall_at_k`.
//! - The REAL numbers (the two `#[ignore]`d runners in the test module) need hand-labeled
//!   multi-speaker audio + the pyannote-seg + CAM++ ONNX models on a Mac, and true voiceprint
//!   fidelity only verifies on a signed build. Their end-to-end tests are `#[ignore]`d and driven
//!   manually (see `docs/DIARIZATION-EVAL.md`). **A green build proves the harness typechecks
//!   against the diarizer/voiceprint APIs — NOT that diarization is accurate or that `0.5` is
//!   right.** `0.5` stays a documented placeholder until the sweep is run on gold labels.
//!
//! ## Gating / lock model (READ-ONLY, not lock-touching)
//!
//! The pure math touches NO content, NO DB, NO FFI — it is numeric only, like `recall_at_k`. The
//! `#[ignore]`d runners read a USER-pointed WAV + reference JSON + pre-existing ONNX models; they
//! open NO murmur DB, add NO gated-read bypass, register NO `#[tauri::command]`, trigger NO model
//! download, and perform NO egress. The re-ID runner does leave-one-out over the manifest's OWN
//! labeled cluster embeddings, so it never reads stored voiceprints at all; a future variant that
//! wanted stored voiceprints would have to route through the gated
//! [`crate::storage::Db::list_voiceprints_visible`] (which applies `visibility_clause`) — never a
//! raw read. NO PII is logged: identity strings come solely from the maintainer's own reference
//! files and are reported as aggregate counts, never per-person `tracing` rows.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::error::{AppError, Result};

/// A labeled speech turn (seconds) with a string speaker identity. A `String` label (not the
/// diarizer's `i32`) so reference identities (`"Anna"`) and hypothesis cluster labels (`"others-0"`)
/// live in INDEPENDENT namespaces that the DER optimal-mapping reconciles — the whole point of the
/// mapping step is that a hypothesis cluster need not share a name with the reference speaker it
/// represents.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub start: f64,
    pub end: f64,
    pub speaker: String,
}

/// Bridge the diarizer's [`crate::transcribe::diarize::SpeakerSpan`] output into [`Turn`]s, naming
/// each `i32` cluster `"{label_prefix}-{n}"` (matching the app's `others-{n}` convention). This
/// feeds a live `Diarizer::diarize` result straight into the metrics. Pure.
pub fn turns_from_spans(
    spans: &[crate::transcribe::diarize::SpeakerSpan],
    label_prefix: &str,
) -> Vec<Turn> {
    spans
        .iter()
        .map(|s| Turn {
            start: s.start,
            end: s.end,
            speaker: format!("{label_prefix}-{}", s.speaker),
        })
        .collect()
}

/// The largest speaker count for which the DER speaker mapping is solved EXACTLY (by enumerating
/// k! injective assignments of the smaller label set). Distinct speakers per meeting is tiny in
/// practice; above this we fall back to a greedy descending-weight assignment.
const MAX_EXACT_MATCH: usize = 8;

/// Upper bound on the LARGER label set for the exact enumeration. The exact matcher enumerates
/// P(max, min) injective assignments, so a small `min` alone does NOT bound cost when the hypothesis
/// is heavily over-segmented (the diarizer runs `num_clusters=-1`, uncapped — e.g. 8 hand-labeled
/// speakers vs 25 clusters ⇒ P(25,8) ≈ 4e10 leaves). Bounding the larger side too caps the worst case
/// at ~P(12,8) ≈ 2e7 (fast); above it we take the greedy fallback.
const MAX_EXACT_LABELS: usize = 12;

// ── Atom / cooccurrence primitive (shared by DER, purity, dominant-label) ───────────────────────

/// One elementary time interval between two adjacent boundary points, with the DISTINCT reference
/// and hypothesis speakers active across the whole atom.
struct Atom {
    d: f64,
    ref_active: Vec<String>,
    hyp_active: Vec<String>,
}

/// The distinct speaker labels of `turns` fully covering the atom `[t0, t1]` (a turn is active iff
/// `turn.start <= t0 && turn.end >= t1`). Sorted + deduped for determinism.
fn active_speakers(turns: &[Turn], t0: f64, t1: f64) -> Vec<String> {
    let mut s: Vec<String> = turns
        .iter()
        .filter(|t| t.start <= t0 && t.end >= t1)
        .map(|t| t.speaker.clone())
        .collect();
    s.sort();
    s.dedup();
    s
}

/// Slice the reference+hypothesis timeline into atoms at every boundary point of either side.
///
/// `collar` (NIST no-score zone, seconds): a whole atom is DROPPED when either of its endpoints lies
/// within `collar` of any REFERENCE turn boundary. `collar == 0.0` (the deterministic default used by
/// every unit test) drops nothing; NIST uses `0.25` for real runs to forgive boundary ambiguity.
fn build_atoms(reference: &[Turn], hypothesis: &[Turn], collar: f64) -> Vec<Atom> {
    let mut pts: Vec<f64> = reference
        .iter()
        .chain(hypothesis.iter())
        .flat_map(|t| [t.start, t.end])
        .collect();
    pts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    pts.dedup();

    let mut ref_bounds: Vec<f64> = reference.iter().flat_map(|t| [t.start, t.end]).collect();
    ref_bounds.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    ref_bounds.dedup();

    let mut atoms = Vec::new();
    for w in pts.windows(2) {
        let (t0, t1) = (w[0], w[1]);
        let d = t1 - t0;
        if d <= 0.0 {
            continue;
        }
        if collar > 0.0
            && ref_bounds
                .iter()
                .any(|b| (b - t0).abs() < collar || (b - t1).abs() < collar)
        {
            continue;
        }
        atoms.push(Atom {
            d,
            ref_active: active_speakers(reference, t0, t1),
            hyp_active: active_speakers(hypothesis, t0, t1),
        });
    }
    atoms
}

/// Per-(ref,hyp) overlapping seconds + per-label active seconds + totals, accumulated over atoms.
struct Overlaps {
    /// `cooc[(ref, hyp)]` = seconds both are simultaneously active.
    cooc: BTreeMap<(String, String), f64>,
    ref_labels: Vec<String>,
    hyp_labels: Vec<String>,
    /// Σ over atoms of `d * |ref_active|` — the DER denominator (total reference SPEAKER time).
    total_ref_time: f64,
    /// Σ over atoms of `d * |hyp_active|` — the purity denominator.
    total_hyp_time: f64,
}

fn accumulate_overlaps(atoms: &[Atom]) -> Overlaps {
    let mut cooc: BTreeMap<(String, String), f64> = BTreeMap::new();
    let mut ref_dur: BTreeMap<String, f64> = BTreeMap::new();
    let mut hyp_dur: BTreeMap<String, f64> = BTreeMap::new();
    for a in atoms {
        for r in &a.ref_active {
            *ref_dur.entry(r.clone()).or_insert(0.0) += a.d;
        }
        for h in &a.hyp_active {
            *hyp_dur.entry(h.clone()).or_insert(0.0) += a.d;
        }
        for r in &a.ref_active {
            for h in &a.hyp_active {
                *cooc.entry((r.clone(), h.clone())).or_insert(0.0) += a.d;
            }
        }
    }
    Overlaps {
        ref_labels: ref_dur.keys().cloned().collect(),
        hyp_labels: hyp_dur.keys().cloned().collect(),
        total_ref_time: ref_dur.values().sum(),
        total_hyp_time: hyp_dur.values().sum(),
        cooc,
    }
}

fn cooc_weight(cooc: &BTreeMap<(String, String), f64>, r: &str, h: &str) -> f64 {
    cooc.get(&(r.to_string(), h.to_string())).copied().unwrap_or(0.0)
}

// ── DER (the NIST decomposition) ────────────────────────────────────────────────────────────────

/// A full Diarization Error Rate report. `der = (miss + false_alarm + confusion) / total_ref_time`.
/// `der` MAY exceed `1.0` under heavy false alarm (NIST-standard). `mapping` is the optimal 1:1
/// reference→hypothesis speaker mapping used to score confusion.
#[derive(Clone, Debug)]
pub struct DerReport {
    pub der: f64,
    pub miss: f64,
    pub false_alarm: f64,
    pub confusion: f64,
    pub total_ref_time: f64,
    pub mapping: Vec<(String, String)>,
}

/// **Diarization Error Rate** over aligned `reference` / `hypothesis` speaker-turn timelines.
///
/// Algorithm (per NIST): slice into atoms at every boundary; accumulate ref×hyp cooccurrence; solve
/// the optimal 1:1 speaker mapping ([`optimal_speaker_mapping`]) that MAXIMIZES matched time; then
/// per atom of duration `d` with `Nref`/`Nhyp` active speakers and `Ncorrect` reference speakers
/// whose MAPPED hypothesis speaker is also active:
/// `miss += d*max(0,Nref-Nhyp)`, `false_alarm += d*max(0,Nhyp-Nref)`,
/// `confusion += d*(min(Nref,Nhyp)-Ncorrect)`.
///
/// GUARD: `total_ref_time == 0` (empty reference) ⇒ `der = 0.0` (vacuous), though `false_alarm` is
/// still reported in the components. Pure — no DB, no model, no FFI.
pub fn diarization_error_rate(reference: &[Turn], hypothesis: &[Turn], collar: f64) -> DerReport {
    let atoms = build_atoms(reference, hypothesis, collar);
    let ov = accumulate_overlaps(&atoms);
    let mapping_map = optimal_speaker_mapping(&ov.cooc, &ov.ref_labels, &ov.hyp_labels);

    let mut miss = 0.0;
    let mut false_alarm = 0.0;
    let mut confusion = 0.0;
    for a in &atoms {
        let nref = a.ref_active.len();
        let nhyp = a.hyp_active.len();
        miss += a.d * nref.saturating_sub(nhyp) as f64;
        false_alarm += a.d * nhyp.saturating_sub(nref) as f64;
        let ncorrect = a
            .ref_active
            .iter()
            .filter(|r| {
                mapping_map
                    .get(r.as_str())
                    .is_some_and(|h| a.hyp_active.contains(h))
            })
            .count();
        confusion += a.d * nref.min(nhyp).saturating_sub(ncorrect) as f64;
    }

    let total_ref_time = ov.total_ref_time;
    let der = if total_ref_time > 0.0 {
        (miss + false_alarm + confusion) / total_ref_time
    } else {
        0.0
    };
    DerReport {
        der,
        miss,
        false_alarm,
        confusion,
        total_ref_time,
        mapping: mapping_map.into_iter().collect(),
    }
}

/// Exact max-weight 1:1 speaker mapping `reference → hypothesis` maximizing
/// `Σ_r cooc[(r, map(r))]` (which equals total matched time, so it MINIMIZES confusion). Solved
/// exactly by enumerating injective assignments of the SMALLER label set (`k! ≤ 8!`, trivial);
/// above [`MAX_EXACT_MATCH`] speakers it falls back to a greedy descending-weight assignment. Only
/// `min(|ref|,|hyp|)` reference speakers get a mapping; the rest stay unmapped (they can never be
/// "correct"). Deterministic: labels are sorted and ties break to the earliest.
fn optimal_speaker_mapping(
    cooc: &BTreeMap<(String, String), f64>,
    ref_labels: &[String],
    hyp_labels: &[String],
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if ref_labels.is_empty() || hyp_labels.is_empty() {
        return out;
    }
    // Fall back to greedy when EITHER the smaller side exceeds MAX_EXACT_MATCH (balanced-large) OR the
    // larger side exceeds MAX_EXACT_LABELS (unbalanced — e.g. 8 hand-labeled speakers vs an
    // over-segmented, uncapped-`num_clusters` hypothesis). The exact path enumerates P(max, min)
    // assignments, so bounding BOTH sides keeps the worst case tractable (no factorial blow-up / hang).
    if ref_labels.len().min(hyp_labels.len()) > MAX_EXACT_MATCH
        || ref_labels.len().max(hyp_labels.len()) > MAX_EXACT_LABELS
    {
        // Non-PII note (no labels): the exact enumeration is skipped for an unusually large speaker
        // count; greedy is a documented approximation for the real `#[ignore]`d runs only.
        tracing::warn!(
            target: "eval",
            small = ref_labels.len().min(hyp_labels.len()),
            large = ref_labels.len().max(hyp_labels.len()),
            "diarization speaker mapping: too many speakers, greedy fallback"
        );
        return greedy_mapping(cooc, ref_labels, hyp_labels);
    }

    // Enumerate over the smaller side; invert if hypothesis is smaller so the returned map is always
    // reference → hypothesis.
    if ref_labels.len() <= hyp_labels.len() {
        for (ri, hi) in best_injective_assignment(ref_labels, hyp_labels, |r, h| {
            cooc_weight(cooc, r, h)
        }) {
            out.insert(ref_labels[ri].clone(), hyp_labels[hi].clone());
        }
    } else {
        for (hi, ri) in best_injective_assignment(hyp_labels, ref_labels, |h, r| {
            cooc_weight(cooc, r, h)
        }) {
            out.insert(ref_labels[ri].clone(), hyp_labels[hi].clone());
        }
    }
    out
}

/// Best injective assignment `small[i] → large[j]` (distinct `j`) maximizing `Σ weight(small,large)`.
/// Returns `(small_idx, large_idx)` pairs. Exact backtracking; deterministic (ascending `j`, strict
/// improvement) so the lexicographically-earliest max-scoring assignment wins ties.
fn best_injective_assignment<F: Fn(&str, &str) -> f64>(
    small: &[String],
    large: &[String],
    weight: F,
) -> Vec<(usize, usize)> {
    let mut used = vec![false; large.len()];
    let mut cur: Vec<usize> = Vec::with_capacity(small.len());
    let mut best_score = f64::NEG_INFINITY;
    let mut best: Vec<usize> = Vec::new();
    assign_rec(
        0,
        small,
        large,
        &weight,
        &mut used,
        &mut cur,
        0.0,
        &mut best_score,
        &mut best,
    );
    best.into_iter().enumerate().collect()
}

#[allow(clippy::too_many_arguments)]
fn assign_rec<F: Fn(&str, &str) -> f64>(
    i: usize,
    small: &[String],
    large: &[String],
    weight: &F,
    used: &mut [bool],
    cur: &mut Vec<usize>,
    acc: f64,
    best_score: &mut f64,
    best: &mut Vec<usize>,
) {
    if i == small.len() {
        if acc > *best_score {
            *best_score = acc;
            *best = cur.clone();
        }
        return;
    }
    for j in 0..large.len() {
        if used[j] {
            continue;
        }
        used[j] = true;
        cur.push(j);
        let add = weight(&small[i], &large[j]);
        assign_rec(
            i + 1,
            small,
            large,
            weight,
            used,
            cur,
            acc + add,
            best_score,
            best,
        );
        cur.pop();
        used[j] = false;
    }
}

/// Greedy descending-weight 1:1 mapping (the `> MAX_EXACT_MATCH` fallback): assign the
/// highest-overlap `(ref, hyp)` pair, skip any already-used speaker, ignore zero-overlap pairs.
/// Deterministic tie-break by label index.
fn greedy_mapping(
    cooc: &BTreeMap<(String, String), f64>,
    ref_labels: &[String],
    hyp_labels: &[String],
) -> BTreeMap<String, String> {
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for (ri, r) in ref_labels.iter().enumerate() {
        for (hi, h) in hyp_labels.iter().enumerate() {
            pairs.push((cooc_weight(cooc, r, h), ri, hi));
        }
    }
    pairs.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
    });
    let mut used_r = vec![false; ref_labels.len()];
    let mut used_h = vec![false; hyp_labels.len()];
    let mut out = BTreeMap::new();
    for (w, ri, hi) in pairs {
        if w <= 0.0 {
            continue;
        }
        if !used_r[ri] && !used_h[hi] {
            used_r[ri] = true;
            used_h[hi] = true;
            out.insert(ref_labels[ri].clone(), hyp_labels[hi].clone());
        }
    }
    out
}

// ── Cluster purity / coverage ───────────────────────────────────────────────────────────────────

/// Cluster purity + coverage (+ their harmonic-mean F1). `purity` = how single-speaker each
/// hypothesis cluster is; `coverage` = how completely each reference speaker is captured by one
/// cluster. Both in `[0, 1]`, higher = better.
#[derive(Clone, Debug)]
pub struct PurityCoverage {
    pub purity: f64,
    pub coverage: f64,
    pub f1: f64,
}

/// `purity = Σ_hyp max_ref overlap(hyp,ref) / total_hyp_time`,
/// `coverage = Σ_ref max_hyp overlap(ref,hyp) / total_ref_time`, `f1 = harmonic(purity, coverage)`.
/// Empty-side guards → `0.0`. Pure.
pub fn cluster_purity_coverage(reference: &[Turn], hypothesis: &[Turn]) -> PurityCoverage {
    let ov = accumulate_overlaps(&build_atoms(reference, hypothesis, 0.0));

    let purity_num: f64 = ov
        .hyp_labels
        .iter()
        .map(|h| {
            ov.ref_labels
                .iter()
                .map(|r| cooc_weight(&ov.cooc, r, h))
                .fold(0.0_f64, f64::max)
        })
        .sum();
    let purity = if ov.total_hyp_time > 0.0 {
        purity_num / ov.total_hyp_time
    } else {
        0.0
    };

    let cov_num: f64 = ov
        .ref_labels
        .iter()
        .map(|r| {
            ov.hyp_labels
                .iter()
                .map(|h| cooc_weight(&ov.cooc, r, h))
                .fold(0.0_f64, f64::max)
        })
        .sum();
    let coverage = if ov.total_ref_time > 0.0 {
        cov_num / ov.total_ref_time
    } else {
        0.0
    };

    PurityCoverage {
        purity,
        coverage,
        f1: harmonic(purity, coverage),
    }
}

/// For each hypothesis cluster label, the reference identity it overlaps MOST (positive overlap
/// only), sorted by hypothesis label. Used by the re-ID runner to attach a ground-truth identity to
/// each diarized cluster; also independently useful. Pure + deterministic.
pub fn dominant_reference_labels(reference: &[Turn], hypothesis: &[Turn]) -> Vec<(String, String)> {
    let ov = accumulate_overlaps(&build_atoms(reference, hypothesis, 0.0));
    let mut out = Vec::new();
    for h in &ov.hyp_labels {
        let mut best: Option<(f64, &String)> = None;
        for r in &ov.ref_labels {
            let w = cooc_weight(&ov.cooc, r, h);
            if w > 0.0 && best.map(|(bw, _)| w > bw).unwrap_or(true) {
                best = Some((w, r));
            }
        }
        if let Some((_, r)) = best {
            out.push((h.clone(), r.clone()));
        }
    }
    out
}

// ── Re-identification precision / recall ────────────────────────────────────────────────────────

/// One re-ID trial: the ground-truth identity (None = a genuine stranger not in the gallery) vs the
/// matcher's predicted label (None = the matcher offered nothing above threshold).
#[derive(Clone, Debug)]
pub struct ReIdCase {
    pub true_identity: Option<String>,
    pub predicted: Option<String>,
}

/// Precision / recall / F1 of the voiceprint matcher over a set of [`ReIdCase`]s, plus the raw
/// confusion counts. `recall` denominator is the number of cases with a KNOWN true identity.
#[derive(Clone, Debug)]
pub struct ReIdMetrics {
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
    pub tp: usize,
    pub fp: usize,
    pub fn_: usize,
    pub tn: usize,
}

/// Score re-ID cases. A correct label (`predicted == Some(x) == true`) is a **tp**; any label on a
/// stranger OR a WRONG label is a **fp** (it hurts precision AND, by not being a tp, recall); no
/// label on a known speaker is a **fn**; no label on a stranger is a **tn**. `precision =
/// tp/(tp+fp)`, `recall = tp / #known`, `f1 = harmonic`. Div-by-zero → `0.0`. Pure.
pub fn reid_metrics(cases: &[ReIdCase]) -> ReIdMetrics {
    let (mut tp, mut fp, mut fn_, mut tn) = (0usize, 0usize, 0usize, 0usize);
    for c in cases {
        match (&c.true_identity, &c.predicted) {
            (Some(t), Some(p)) if t == p => tp += 1,
            (_, Some(_)) => fp += 1,
            (Some(_), None) => fn_ += 1,
            (None, None) => tn += 1,
        }
    }
    let known = cases.iter().filter(|c| c.true_identity.is_some()).count();
    let precision = ratio(tp, tp + fp);
    let recall = ratio(tp, known);
    ReIdMetrics {
        precision,
        recall,
        f1: harmonic(precision, recall),
        tp,
        fp,
        fn_,
        tn,
    }
}

// ── Cosine verification threshold sweep (validates VOICEPRINT_MATCH_THRESHOLD) ──────────────────

/// One same/different-speaker verification pair with its cosine score (`f32` to compare directly
/// with [`crate::transcribe::diarize::cosine`]'s output).
#[derive(Clone, Debug)]
pub struct VerificationPair {
    pub score: f32,
    pub same_speaker: bool,
}

/// Confusion counts + rates at ONE cosine threshold (a pair is predicted-same iff `score >= thr`).
/// `far = fp / #impostor`, `frr = fn_ / #genuine`, `accuracy = (tp+tn)/total`.
#[derive(Clone, Debug)]
pub struct ThresholdPoint {
    pub threshold: f32,
    pub tp: usize,
    pub fp: usize,
    pub tn: usize,
    pub fn_: usize,
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
    pub far: f64,
    pub frr: f64,
    pub accuracy: f64,
}

/// Evaluate every threshold in `thresholds` over `pairs`. Pure.
pub fn sweep_thresholds(pairs: &[VerificationPair], thresholds: &[f32]) -> Vec<ThresholdPoint> {
    let genuine = pairs.iter().filter(|p| p.same_speaker).count();
    let impostor = pairs.len() - genuine;
    thresholds
        .iter()
        .map(|&threshold| {
            let (mut tp, mut fp, mut tn, mut fn_) = (0usize, 0usize, 0usize, 0usize);
            for p in pairs {
                match (p.same_speaker, p.score >= threshold) {
                    (true, true) => tp += 1,
                    (true, false) => fn_ += 1,
                    (false, true) => fp += 1,
                    (false, false) => tn += 1,
                }
            }
            let precision = ratio(tp, tp + fp);
            let recall = ratio(tp, tp + fn_);
            ThresholdPoint {
                threshold,
                tp,
                fp,
                tn,
                fn_,
                precision,
                recall,
                f1: harmonic(precision, recall),
                far: ratio(fp, impostor),
                frr: ratio(fn_, genuine),
                accuracy: ratio(tp + tn, pairs.len()),
            }
        })
        .collect()
}

/// The default sweep grid: `0.00, 0.02, … 1.00` (51 points), constructed as `i/50` so it contains
/// EXACTLY `0.50` — the point at which [`crate::transcribe::diarize::VOICEPRINT_MATCH_THRESHOLD`]
/// currently sits.
pub fn default_threshold_grid() -> Vec<f32> {
    (0..=50).map(|i| i as f32 / 50.0).collect()
}

/// Equal Error Rate: the grid threshold minimizing `|far - frr|`, returning
/// `(threshold, error = (far+frr)/2)`. `None` when there is no impostor OR no genuine pair (FAR/FRR
/// undefined). Deterministic tie-break to the lowest threshold. Pure.
pub fn equal_error_rate(pairs: &[VerificationPair]) -> Option<(f32, f64)> {
    let genuine = pairs.iter().filter(|p| p.same_speaker).count();
    let impostor = pairs.len() - genuine;
    if genuine == 0 || impostor == 0 {
        return None;
    }
    let points = sweep_thresholds(pairs, &default_threshold_grid());
    let mut best: Option<(f32, f64, f64)> = None; // (threshold, gap, error)
    for p in &points {
        let gap = (p.far - p.frr).abs();
        if best.map(|(_, bgap, _)| gap < bgap).unwrap_or(true) {
            best = Some((p.threshold, gap, (p.far + p.frr) / 2.0));
        }
    }
    best.map(|(t, _, e)| (t, e))
}

/// The sweep point with the highest F1 (ties → lowest threshold, since `points` are threshold-
/// ordered and improvement is strict). `None` for an empty slice. Pure.
pub fn best_f1(points: &[ThresholdPoint]) -> Option<ThresholdPoint> {
    let mut best: Option<&ThresholdPoint> = None;
    for p in points {
        if best.map(|b| p.f1 > b.f1).unwrap_or(true) {
            best = Some(p);
        }
    }
    best.cloned()
}

// ── Shared numeric helpers ──────────────────────────────────────────────────────────────────────

fn ratio(num: usize, den: usize) -> f64 {
    if den == 0 {
        0.0
    } else {
        num as f64 / den as f64
    }
}

fn harmonic(a: f64, b: f64) -> f64 {
    if a + b > 0.0 {
        2.0 * a * b / (a + b)
    } else {
        0.0
    }
}

// ── Dep-free stdout formatters (deterministic, mirror eval::format_report_table) ────────────────

/// Render a [`DerReport`] + [`PurityCoverage`] as a fixed-width block. Reports speaker COUNTS, never
/// the mapped identity names (no per-person rows in output). Pure string formatting.
pub fn format_der_report(der: &DerReport, pc: &PurityCoverage) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Diarization Error Rate — total reference speaker time {:.2}s\n",
        der.total_ref_time
    ));
    out.push_str(&format!("{:<20} {:>10.4}\n", "DER", der.der));
    out.push_str(&format!("{:<20} {:>10.2}\n", "  miss (s)", der.miss));
    out.push_str(&format!("{:<20} {:>10.2}\n", "  false alarm (s)", der.false_alarm));
    out.push_str(&format!("{:<20} {:>10.2}\n", "  confusion (s)", der.confusion));
    out.push_str(&format!("{:<20} {:>10}\n", "  matched speakers", der.mapping.len()));
    out.push_str(&format!("{:<20} {:>10.4}\n", "cluster purity", pc.purity));
    out.push_str(&format!("{:<20} {:>10.4}\n", "cluster coverage", pc.coverage));
    out.push_str(&format!("{:<20} {:>10.4}\n", "purity/coverage F1", pc.f1));
    out
}

/// Render a threshold sweep as a fixed-width table + an EER line + the highlighted operating point.
/// Pure string formatting, no color, no deps.
pub fn format_sweep_table(
    points: &[ThresholdPoint],
    eer: Option<(f32, f64)>,
    default_point: &ThresholdPoint,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Verification threshold sweep — {} points\n",
        points.len()
    ));
    out.push_str(&format!(
        "{:>5} {:>4} {:>4} {:>4} {:>4} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}\n",
        "thr", "tp", "fp", "tn", "fn", "prec", "rec", "f1", "far", "frr", "acc"
    ));
    out.push_str(&format!("{}\n", "-".repeat(72)));
    for p in points {
        out.push_str(&format!(
            "{:>5.2} {:>4} {:>4} {:>4} {:>4} {:>7.4} {:>7.4} {:>7.4} {:>7.4} {:>7.4} {:>7.4}\n",
            p.threshold, p.tp, p.fp, p.tn, p.fn_, p.precision, p.recall, p.f1, p.far, p.frr,
            p.accuracy
        ));
    }
    match eer {
        Some((t, e)) => out.push_str(&format!("EER: threshold {t:.2}, error {e:.4}\n")),
        None => out.push_str("EER: n/a (need both genuine and impostor pairs)\n"),
    }
    out.push_str(&format!(
        "operating point (threshold {:.2}): prec {:.4} rec {:.4} f1 {:.4} far {:.4} frr {:.4}\n",
        default_point.threshold,
        default_point.precision,
        default_point.recall,
        default_point.f1,
        default_point.far,
        default_point.frr
    ));
    out
}

// ── Reference-annotation parsing (for the `#[ignore]`d runners) ─────────────────────────────────

#[derive(Deserialize)]
struct ReferenceFile {
    #[serde(default)]
    #[allow(dead_code)] // manifest/documentation metadata; parse_reference reads only `reference`.
    meeting: String,
    reference: Vec<TurnJson>,
}

#[derive(Deserialize)]
struct TurnJson {
    start: f64,
    end: f64,
    speaker: String,
}

/// Parse a reference-annotation JSON (`{ "meeting": …, "reference": [ {start,end,speaker}, … ] }`)
/// into ground-truth [`Turn`]s. Parse failures map to [`AppError::InvalidArg`] (the same pattern as
/// `LabeledSet::from_json`). Used by the real-audio runners; also useful standalone.
pub fn parse_reference(json: &str) -> Result<Vec<Turn>> {
    let rf: ReferenceFile = serde_json::from_str(json)
        .map_err(|e| AppError::InvalidArg(format!("parse diarization reference: {e}")))?;
    Ok(rf
        .reference
        .into_iter()
        .map(|t| Turn {
            start: t.start,
            end: t.end,
            speaker: t.speaker,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turns(rows: &[(f64, f64, &str)]) -> Vec<Turn> {
        rows.iter()
            .map(|(s, e, sp)| Turn {
                start: *s,
                end: *e,
                speaker: (*sp).to_string(),
            })
            .collect()
    }

    fn reid(t: Option<&str>, p: Option<&str>) -> ReIdCase {
        ReIdCase {
            true_identity: t.map(|s| s.to_string()),
            predicted: p.map(|s| s.to_string()),
        }
    }

    // ── DER ──────────────────────────────────────────────────────────────────────────────────────

    #[test]
    fn der_identical_reference_is_zero() {
        // ref == hyp geometry but with DIFFERENT label NAMES — the optimal mapping reconciles them.
        let reference = turns(&[(0.0, 5.0, "A"), (5.0, 10.0, "B")]);
        let hypothesis = turns(&[(0.0, 5.0, "others-0"), (5.0, 10.0, "others-1")]);
        let r = diarization_error_rate(&reference, &hypothesis, 0.0);
        assert!((r.der - 0.0).abs() < 1e-9, "identical geometry ⇒ DER 0");
        assert!((r.confusion - 0.0).abs() < 1e-9);
        assert!((r.total_ref_time - 10.0).abs() < 1e-9);
    }

    #[test]
    fn der_empty_hypothesis_is_all_miss() {
        let reference = turns(&[(0.0, 4.0, "A"), (4.0, 10.0, "B")]);
        let r = diarization_error_rate(&reference, &[], 0.0);
        assert!((r.total_ref_time - 10.0).abs() < 1e-9);
        assert!((r.miss - 10.0).abs() < 1e-9, "no hypothesis ⇒ all miss");
        assert!((r.false_alarm - 0.0).abs() < 1e-9);
        assert!((r.der - 1.0).abs() < 1e-9);
    }

    #[test]
    fn der_split_cluster_is_confusion() {
        // One reference speaker across [0,10]; the diarizer splits it into two clusters at t=5.
        let reference = turns(&[(0.0, 10.0, "A")]);
        let hypothesis = turns(&[(0.0, 5.0, "others-0"), (5.0, 10.0, "others-1")]);
        let r = diarization_error_rate(&reference, &hypothesis, 0.0);
        assert!((r.miss - 0.0).abs() < 1e-9);
        assert!((r.false_alarm - 0.0).abs() < 1e-9);
        assert!((r.confusion - 5.0).abs() < 1e-9, "half the time is wrong-cluster");
        assert!((r.der - 0.5).abs() < 1e-9);
    }

    #[test]
    fn der_swapped_labels_zero_via_optimal_mapping() {
        // RED-before-GREEN for STEP 4: the correct mapping is A→others-1, B→others-0 (NOT the
        // sorted-order pairing). A name-equality / positional stub scores ~1.0 here; only the real
        // max-weight matching scores 0.0.
        let reference = turns(&[(0.0, 5.0, "A"), (5.0, 10.0, "B")]);
        let hypothesis = turns(&[(5.0, 10.0, "others-0"), (0.0, 5.0, "others-1")]);
        let r = diarization_error_rate(&reference, &hypothesis, 0.0);
        assert!(
            (r.der - 0.0).abs() < 1e-9,
            "optimal mapping reconciles the swap ⇒ DER 0 (a stub would give ~1.0)"
        );
        assert!((r.confusion - 0.0).abs() < 1e-9);
        // The mapping actually chosen: A↔others-1, B↔others-0.
        let m: std::collections::BTreeMap<_, _> = r.mapping.into_iter().collect();
        assert_eq!(m.get("A").map(String::as_str), Some("others-1"));
        assert_eq!(m.get("B").map(String::as_str), Some("others-0"));
    }

    #[test]
    fn der_false_alarm_can_exceed_ref() {
        // ref one speaker over [0,5]; hyp is correct there PLUS an extra 10s cluster [5,15] with no
        // reference at all ⇒ 10s of false alarm over 5s of reference ⇒ DER 2.0 (> 1, NIST-standard).
        let reference = turns(&[(0.0, 5.0, "A")]);
        let hypothesis = turns(&[(0.0, 5.0, "others-0"), (5.0, 15.0, "others-1")]);
        let r = diarization_error_rate(&reference, &hypothesis, 0.0);
        assert!((r.total_ref_time - 5.0).abs() < 1e-9);
        assert!((r.false_alarm - 10.0).abs() < 1e-9);
        assert!((r.der - 2.0).abs() < 1e-9, "DER may exceed 1.0 under heavy false alarm");
    }

    #[test]
    fn der_empty_reference_is_guarded_zero() {
        // No reference ⇒ total_ref_time 0 ⇒ DER guarded to 0.0, but the false-alarm seconds are
        // still reported in the components (honest).
        let hypothesis = turns(&[(0.0, 5.0, "others-0")]);
        let r = diarization_error_rate(&[], &hypothesis, 0.0);
        assert!((r.der - 0.0).abs() < 1e-9, "empty reference ⇒ vacuous DER 0");
        assert!((r.total_ref_time - 0.0).abs() < 1e-9);
        assert!((r.false_alarm - 5.0).abs() < 1e-9, "false-alarm seconds still reported");
    }

    // ── purity / coverage ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn purity_perfect_is_one() {
        let reference = turns(&[(0.0, 5.0, "A"), (5.0, 10.0, "B")]);
        let hypothesis = turns(&[(0.0, 5.0, "others-0"), (5.0, 10.0, "others-1")]);
        let pc = cluster_purity_coverage(&reference, &hypothesis);
        assert!((pc.purity - 1.0).abs() < 1e-9);
        assert!((pc.coverage - 1.0).abs() < 1e-9);
        assert!((pc.f1 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn purity_split_5050_is_half() {
        // One hypothesis cluster spanning two reference speakers 50/50 ⇒ purity 0.5, coverage 1.0.
        let reference = turns(&[(0.0, 5.0, "A"), (5.0, 10.0, "B")]);
        let hypothesis = turns(&[(0.0, 10.0, "others-0")]);
        let pc = cluster_purity_coverage(&reference, &hypothesis);
        assert!((pc.purity - 0.5).abs() < 1e-9, "cluster is half A, half B");
        assert!((pc.coverage - 1.0).abs() < 1e-9, "each reference speaker fully in the cluster");
    }

    #[test]
    fn dominant_reference_labels_maps_each_cluster_to_best_overlap() {
        // others-0 overlaps A more; others-1 overlaps B fully.
        let reference = turns(&[(0.0, 6.0, "A"), (6.0, 10.0, "B")]);
        let hypothesis = turns(&[(0.0, 5.0, "others-0"), (6.0, 10.0, "others-1")]);
        let dom = dominant_reference_labels(&reference, &hypothesis);
        assert_eq!(
            dom,
            vec![
                ("others-0".to_string(), "A".to_string()),
                ("others-1".to_string(), "B".to_string()),
            ]
        );
    }

    // ── re-ID ─────────────────────────────────────────────────────────────────────────────────────

    #[test]
    fn reid_precision_recall_known_mix() {
        let cases = vec![
            reid(Some("A"), Some("A")), // tp
            reid(Some("B"), Some("B")), // tp
            reid(Some("C"), Some("D")), // fp (wrong label)
            reid(None, Some("E")),      // fp (label on a stranger)
            reid(Some("F"), None),      // fn
            reid(None, None),           // tn
        ];
        let m = reid_metrics(&cases);
        assert_eq!((m.tp, m.fp, m.fn_, m.tn), (2, 2, 1, 1));
        assert!((m.precision - 0.5).abs() < 1e-9, "2 tp / (2 tp + 2 fp)");
        assert!((m.recall - 0.5).abs() < 1e-9, "2 tp / 4 known (A,B,C,F)");
        assert!((m.f1 - 0.5).abs() < 1e-9);
    }

    #[test]
    fn reid_wrong_label_hurts_precision_and_recall() {
        let correct = reid_metrics(&[reid(Some("A"), Some("A"))]);
        assert!((correct.precision - 1.0).abs() < 1e-9);
        assert!((correct.recall - 1.0).abs() < 1e-9);
        // Adding a WRONG prediction (Bob for Alice) is an fp AND is excluded from tp ⇒ both drop.
        let with_wrong =
            reid_metrics(&[reid(Some("A"), Some("A")), reid(Some("Alice"), Some("Bob"))]);
        assert!((with_wrong.precision - 0.5).abs() < 1e-9, "precision drops 1.0 → 0.5");
        assert!((with_wrong.recall - 0.5).abs() < 1e-9, "recall drops 1.0 → 0.5");
    }

    // ── threshold sweep / EER / best-F1 ────────────────────────────────────────────────────────────

    #[test]
    fn sweep_far_frr_at_known_thresholds() {
        // genuine (same) scores {0.8, 0.6}; impostor (diff) scores {0.4, 0.2}.
        let pairs = vec![
            VerificationPair { score: 0.8, same_speaker: true },
            VerificationPair { score: 0.6, same_speaker: true },
            VerificationPair { score: 0.4, same_speaker: false },
            VerificationPair { score: 0.2, same_speaker: false },
        ];
        let pts = sweep_thresholds(&pairs, &[0.3, 0.5, 0.7]);
        // t=0.3: genuine both accept (tp 2, fn 0); impostor 0.4 accepted (fp 1), 0.2 rejected (tn 1).
        assert_eq!((pts[0].tp, pts[0].fp, pts[0].tn, pts[0].fn_), (2, 1, 1, 0));
        assert!((pts[0].far - 0.5).abs() < 1e-9 && (pts[0].frr - 0.0).abs() < 1e-9);
        // t=0.5: perfect split (fp 0, fn 0).
        assert_eq!((pts[1].tp, pts[1].fp, pts[1].tn, pts[1].fn_), (2, 0, 2, 0));
        assert!((pts[1].far - 0.0).abs() < 1e-9 && (pts[1].frr - 0.0).abs() < 1e-9);
        // t=0.7: genuine 0.6 now rejected (fn 1); impostors both rejected.
        assert_eq!((pts[2].tp, pts[2].fp, pts[2].tn, pts[2].fn_), (1, 0, 2, 1));
        assert!((pts[2].far - 0.0).abs() < 1e-9 && (pts[2].frr - 0.5).abs() < 1e-9);
    }

    #[test]
    fn sweep_edges_accept_all_and_reject_all() {
        let pairs = vec![
            VerificationPair { score: 0.8, same_speaker: true },
            VerificationPair { score: 0.3, same_speaker: false },
        ];
        // threshold 0.0 accepts everything ⇒ far 1.0, frr 0.0.
        let lo = &sweep_thresholds(&pairs, &[0.0])[0];
        assert!((lo.far - 1.0).abs() < 1e-9 && (lo.frr - 0.0).abs() < 1e-9);
        // threshold above every score rejects everything ⇒ far 0.0, frr 1.0.
        let hi = &sweep_thresholds(&pairs, &[1.01])[0];
        assert!((hi.far - 0.0).abs() < 1e-9 && (hi.frr - 1.0).abs() < 1e-9);
    }

    #[test]
    fn equal_error_rate_finds_crossover() {
        // Overlapping (adversarial) distributions so FAR and FRR genuinely cross at a nonzero rate:
        // genuine {0.3,0.5,0.7}, impostor {0.4,0.6,0.8}. FAR==FRR==2/3 in the crossover band.
        let pairs = vec![
            VerificationPair { score: 0.3, same_speaker: true },
            VerificationPair { score: 0.5, same_speaker: true },
            VerificationPair { score: 0.7, same_speaker: true },
            VerificationPair { score: 0.4, same_speaker: false },
            VerificationPair { score: 0.6, same_speaker: false },
            VerificationPair { score: 0.8, same_speaker: false },
        ];
        let (thr, err) = equal_error_rate(&pairs).expect("both genuine and impostor present");
        assert!((err - 2.0 / 3.0).abs() < 1e-6, "EER error at the FAR/FRR crossover");
        assert!((0.5..=0.62).contains(&thr), "crossover threshold band, got {thr}");
        // No impostor ⇒ None; no genuine ⇒ None.
        let genuine_only = vec![VerificationPair { score: 0.9, same_speaker: true }];
        assert!(equal_error_rate(&genuine_only).is_none());
        let impostor_only = vec![VerificationPair { score: 0.1, same_speaker: false }];
        assert!(equal_error_rate(&impostor_only).is_none());
    }

    #[test]
    fn best_f1_picks_expected_point() {
        let pairs = vec![
            VerificationPair { score: 0.8, same_speaker: true },
            VerificationPair { score: 0.6, same_speaker: true },
            VerificationPair { score: 0.4, same_speaker: false },
            VerificationPair { score: 0.2, same_speaker: false },
        ];
        let pts = sweep_thresholds(&pairs, &[0.3, 0.5, 0.7]);
        let bf = best_f1(&pts).expect("non-empty");
        assert!((bf.threshold - 0.5).abs() < 1e-6, "0.5 separates perfectly ⇒ F1 1.0");
        assert!((bf.f1 - 1.0).abs() < 1e-9);
        assert!(best_f1(&[]).is_none());
    }

    #[test]
    fn default_threshold_grid_contains_050() {
        let grid = default_threshold_grid();
        assert_eq!(grid.len(), 51);
        assert!(grid.contains(&0.5), "the 0.50 operating point must be on the grid");
        assert!((grid[0] - 0.0).abs() < 1e-9);
        assert!((grid[50] - 1.0).abs() < 1e-9);
    }

    // ── span bridge + formatters ───────────────────────────────────────────────────────────────────

    #[test]
    fn turns_from_spans_stringifies() {
        use crate::transcribe::diarize::SpeakerSpan;
        let spans = vec![
            SpeakerSpan { start: 0.0, end: 2.0, speaker: 0 },
            SpeakerSpan { start: 2.0, end: 5.0, speaker: 2 },
        ];
        let t = turns_from_spans(&spans, "others");
        assert_eq!(t[0].speaker, "others-0");
        assert_eq!(t[1].speaker, "others-2");
        assert!((t[1].start - 2.0).abs() < 1e-9 && (t[1].end - 5.0).abs() < 1e-9);
    }

    #[test]
    fn format_der_report_and_sweep_table_smoke() {
        let reference = turns(&[(0.0, 5.0, "A"), (5.0, 10.0, "B")]);
        let hypothesis = turns(&[(0.0, 5.0, "others-0"), (5.0, 10.0, "others-1")]);
        let der = diarization_error_rate(&reference, &hypothesis, 0.0);
        let pc = cluster_purity_coverage(&reference, &hypothesis);
        let s = format_der_report(&der, &pc);
        for needle in ["DER", "miss", "false alarm", "confusion", "purity", "coverage"] {
            assert!(s.contains(needle), "DER report missing '{needle}'");
        }

        let pairs = vec![
            VerificationPair { score: 0.8, same_speaker: true },
            VerificationPair { score: 0.2, same_speaker: false },
        ];
        let grid = default_threshold_grid();
        let points = sweep_thresholds(&pairs, &grid);
        let eer = equal_error_rate(&pairs);
        let dp = points
            .iter()
            .find(|p| (p.threshold - 0.5).abs() < 1e-6)
            .cloned()
            .expect("0.50 is on the grid");
        let table = format_sweep_table(&points, eer, &dp);
        for needle in ["threshold", "EER", "far", "frr", "operating point"] {
            assert!(table.contains(needle), "sweep table missing '{needle}'");
        }
    }

    #[test]
    fn parse_reference_reads_turns_and_rejects_garbage() {
        let json = r#"{ "meeting": "demo", "reference": [
            {"start": 0.0, "end": 4.2, "speaker": "Anna"},
            {"start": 4.2, "end": 9.8, "speaker": "Bartek"}
        ]}"#;
        let t = parse_reference(json).expect("valid reference parses");
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].speaker, "Anna");
        assert!((t[1].end - 9.8).abs() < 1e-9);
        // Malformed JSON ⇒ AppError::InvalidArg, never a panic.
        assert!(matches!(
            parse_reference("{ not json"),
            Err(AppError::InvalidArg(_))
        ));
    }

    // ── `#[ignore]`d REAL-AUDIO runners (compile-checked here; run manually per docs) ──────────────
    //
    // These typecheck against the diarizer / voiceprint / wav APIs in `cargo test --lib` but do NOT
    // execute (no models, no labeled audio on CI). Real DER / EER / re-ID numbers are produced on a
    // Mac per docs/DIARIZATION-EVAL.md. They open NO murmur DB and perform NO egress; env-var driven
    // exactly like `eval::bakeoff::run_bakeoff_over_real_db_from_env` (sidesteps Touch ID / the DEK).

    use std::path::Path;

    /// STEP 10 — real DER + purity/coverage over ONE hand-labeled recording.
    ///
    /// Env: `MURMUR_DER_WAV` (system-stream WAV), `MURMUR_DER_REF` (reference JSON),
    /// `MURMUR_DIARIZE_SEG_MODEL` + `MURMUR_DIARIZE_EMB_MODEL` (the pyannote-seg + CAM++ `.onnx`
    /// already in `models_dir()` after any diarized recording), `MURMUR_DER_COLLAR` (opt, def 0.0).
    #[test]
    #[ignore = "real DER: needs MURMUR_DER_WAV + MURMUR_DER_REF + MURMUR_DIARIZE_SEG_MODEL + MURMUR_DIARIZE_EMB_MODEL on a Mac"]
    fn run_der_over_labeled_audio_from_env() {
        use crate::audio::wav::{read_wav_mono, resample_to_16k};
        use crate::transcribe::diarize::Diarizer;

        let wav = std::env::var("MURMUR_DER_WAV").expect("set MURMUR_DER_WAV to a system-stream WAV");
        let ref_path =
            std::env::var("MURMUR_DER_REF").expect("set MURMUR_DER_REF to a reference JSON path");
        let seg = std::env::var("MURMUR_DIARIZE_SEG_MODEL")
            .expect("set MURMUR_DIARIZE_SEG_MODEL to the pyannote segmentation .onnx");
        let emb = std::env::var("MURMUR_DIARIZE_EMB_MODEL")
            .expect("set MURMUR_DIARIZE_EMB_MODEL to the CAM++ embedding .onnx");
        let collar: f64 = std::env::var("MURMUR_DER_COLLAR")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);

        let (samples, sr) = read_wav_mono(Path::new(&wav)).expect("read wav");
        let diarizer = Diarizer::load(Path::new(&seg), Path::new(&emb)).expect("load diarizer");
        if diarizer.sample_rate() != 16_000 {
            eprintln!(
                "WARNING: diarizer sample_rate = {} (resample_to_16k targets 16 kHz)",
                diarizer.sample_rate()
            );
        }
        let samples16 = resample_to_16k(&samples, sr).expect("resample to 16k");
        let spans = diarizer.diarize(&samples16).expect("diarize");
        let hypothesis = turns_from_spans(&spans, "others");
        let reference =
            parse_reference(&std::fs::read_to_string(&ref_path).expect("read reference json"))
                .expect("parse reference json");

        let der = diarization_error_rate(&reference, &hypothesis, collar);
        let pc = cluster_purity_coverage(&reference, &hypothesis);
        println!("\n{}", format_der_report(&der, &pc));
    }

    /// Manifest of `{wav, reference}` pairs for the multi-recording sweep/re-ID runner.
    #[derive(Deserialize)]
    struct ManifestEntry {
        wav: String,
        reference: String,
    }

    /// Diarize each manifest recording, compute per-cluster CAM++ voiceprints, and attach each
    /// cluster's ground-truth identity via `dominant_reference_labels`. Returns `(identity, embedding)`
    /// — the leave-one-out gallery for the sweep + re-ID. No DB, no egress.
    fn gather_labeled_cluster_embeddings(
        manifest: &[ManifestEntry],
        seg: &Path,
        emb: &Path,
    ) -> Vec<(String, Vec<f32>)> {
        use crate::audio::wav::{read_wav_mono, resample_to_16k};
        use crate::transcribe::diarize::{compute_cluster_voiceprints, Diarizer};

        let diarizer = Diarizer::load(seg, emb).expect("load diarizer");
        let sr = diarizer.sample_rate();
        let mut out = Vec::new();
        for entry in manifest {
            let (samples, wsr) = read_wav_mono(Path::new(&entry.wav)).expect("read wav");
            let samples16 = resample_to_16k(&samples, wsr).expect("resample to 16k");
            let spans = diarizer.diarize(&samples16).expect("diarize");
            let reference = parse_reference(
                &std::fs::read_to_string(&entry.reference).expect("read reference json"),
            )
            .expect("parse reference json");
            let hypothesis = turns_from_spans(&spans, "others");
            let dominant = dominant_reference_labels(&reference, &hypothesis);
            for vp in compute_cluster_voiceprints(emb, &samples16, &spans, sr) {
                let hyp_label = format!("others-{}", vp.cluster_index);
                if let Some((_, identity)) = dominant.iter().find(|(h, _)| *h == hyp_label) {
                    out.push((identity.clone(), vp.embedding));
                }
            }
        }
        out
    }

    /// STEP 11 — real cosine threshold sweep (validates VOICEPRINT_MATCH_THRESHOLD) + leave-one-out
    /// re-ID accuracy of the SHIPPED matcher (`suggest_voiceprint_labels`).
    ///
    /// Env: `MURMUR_DER_MANIFEST` (JSON list of `{wav, reference}`) + the two model paths above.
    #[test]
    #[ignore = "real sweep + re-ID: needs MURMUR_DER_MANIFEST + MURMUR_DIARIZE_SEG_MODEL + MURMUR_DIARIZE_EMB_MODEL on a Mac"]
    fn run_threshold_sweep_over_manifest_from_env() {
        use crate::transcribe::diarize::{
            cosine, suggest_voiceprint_labels, ClusterEmbeddingRef, LabeledEmbeddingRef,
            VOICEPRINT_MATCH_THRESHOLD,
        };

        let manifest_path =
            std::env::var("MURMUR_DER_MANIFEST").expect("set MURMUR_DER_MANIFEST to a manifest JSON");
        let seg = std::env::var("MURMUR_DIARIZE_SEG_MODEL").expect("set MURMUR_DIARIZE_SEG_MODEL");
        let emb = std::env::var("MURMUR_DIARIZE_EMB_MODEL").expect("set MURMUR_DIARIZE_EMB_MODEL");
        let manifest: Vec<ManifestEntry> = serde_json::from_str(
            &std::fs::read_to_string(&manifest_path).expect("read manifest json"),
        )
        .expect("parse manifest json");

        let labeled = gather_labeled_cluster_embeddings(&manifest, Path::new(&seg), Path::new(&emb));
        if labeled.len() < 2 {
            eprintln!("WARNING: fewer than 2 labeled clusters gathered — nothing to sweep");
            return;
        }

        // (a) all within-identity (same) + cross-identity (diff) cosine pairs → sweep + EER + best-F1.
        let mut pairs = Vec::new();
        for i in 0..labeled.len() {
            for j in (i + 1)..labeled.len() {
                pairs.push(VerificationPair {
                    score: cosine(&labeled[i].1, &labeled[j].1),
                    same_speaker: labeled[i].0 == labeled[j].0,
                });
            }
        }
        let points = sweep_thresholds(&pairs, &default_threshold_grid());
        let eer = equal_error_rate(&pairs);
        let default_point = points
            .iter()
            .find(|p| (p.threshold - VOICEPRINT_MATCH_THRESHOLD).abs() < 1e-6)
            .cloned()
            .unwrap_or_else(|| {
                sweep_thresholds(&pairs, &[VOICEPRINT_MATCH_THRESHOLD])
                    .into_iter()
                    .next()
                    .expect("one point")
            });
        println!("\n{}", format_sweep_table(&points, eer, &default_point));
        if let Some(bf) = best_f1(&points) {
            println!("best-F1 threshold = {:.2} (F1 {:.4})", bf.threshold, bf.f1);
        }

        // (b) leave-one-out re-ID through the SAME matcher the app ships (at the placeholder 0.5).
        let mut cases = Vec::new();
        for (idx, (identity, emb_i)) in labeled.iter().enumerate() {
            let gallery: Vec<LabeledEmbeddingRef> = labeled
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != idx)
                .map(|(_, (lab, e))| LabeledEmbeddingRef {
                    label: lab.as_str(),
                    embedding: e.as_slice(),
                })
                .collect();
            let query = [ClusterEmbeddingRef {
                cluster_index: idx as i32,
                embedding: emb_i.as_slice(),
            }];
            let predicted = suggest_voiceprint_labels(&query, &gallery, VOICEPRINT_MATCH_THRESHOLD)
                .into_iter()
                .next()
                .map(|s| s.label);
            cases.push(ReIdCase {
                true_identity: Some(identity.clone()),
                predicted,
            });
        }
        let m = reid_metrics(&cases);
        println!(
            "re-ID @ {:.2}: precision {:.3} recall {:.3} f1 {:.3} (tp {} fp {} fn {} tn {})",
            VOICEPRINT_MATCH_THRESHOLD, m.precision, m.recall, m.f1, m.tp, m.fp, m.fn_, m.tn
        );
    }
}
