//! CALIBRATION harnesses — brain-v3 audit-fix PR-10: the program's OWN unrun release gates.
//!
//! The brain-v3 program shipped three mechanisms whose OPERATING POINTS were never measured on a real
//! Mac against real models/vaults — `cargo test` pins the LOGIC but not the NUMBERS. This module closes
//! that gap with three calibration harnesses, each in the same shape as the [`crate::eval::bakeoff`]
//! harness: a small PURE, headless-testable math core (unit-tested in the normal loop) plus a heavy
//! `#[ignore]` + env-gated RUNNER that a human runs on a real Mac to produce the actual numbers.
//!
//! The three gates:
//!
//! 1. **Semantic-link threshold** ([`percentile`]) — the [`crate::links::SEMANTIC_LINK_FLOOR`] (0.80) /
//!    [`crate::links::SEMANTIC_LINK_STRONG`] (0.88) floors were chosen as e5 START VALUES and never
//!    calibrated. The runner samples RANDOM visible item pairs, builds the NULL cosine distribution, and
//!    reports its high percentiles vs the floors — so a human can see whether 0.80 sits ABOVE the noise
//!    floor (a link means something) or is buried in it (e5's compressed cosine range routinely puts
//!    unrelated pairs at 0.75–0.85, so 0.80 may barely bind).
//!
//! 2. **Receipts coverage** ([`coverage_fraction`]) — what FRACTION of a note's claim lines earn a
//!    receipt (via [`crate::summarize::grounding::align_claims_to_segments`] at the shipped overlap
//!    floor). The audit flagged this plausibly <50% on real LLM-paraphrased notes. The runner MEASURES
//!    coverage only; PRECISION (are the receipts CORRECT) needs hand labels and is out of scope.
//!
//! 3. **Large-PDF ingest bench** (runner only) — times `extract_blocks` → `chunk_document_hierarchical`
//!    → sub-batched embed over a big text PDF and reports wall-clock + block/chunk/vector counts, so the
//!    ingest surface's throughput on a realistic large document is a measured number, not a guess.
//!
//! ## What is / isn't testable headless
//!
//! - The PURE MATH here ([`percentile`], [`coverage_fraction`]) is unit-tested with synthetic inputs in
//!   `cargo test --lib` — no model, no DB, no clock.
//! - The RUNNERS are `#[ignore]`d and env-gated. They load real e5 vectors / generate a real PDF / OCR,
//!   so they MUST run on a real Mac (the embed model on disk, a populated vault). A green build proves
//!   the harness typechecks against the real APIs, NOT that any threshold is right or any note is
//!   well-covered. Each runner prints whether the embedder is REAL or the STUB and says the numbers are
//!   meaningless under the stub.
//!
//! ## Gating (read the lock-model note in [`crate::eval`])
//!
//! READ-ONLY. The semantic-link runner enumerates items ONLY through the visibility-gated
//! `Db::sampled_visible_item_centroids` (same `visibility_clause` the app uses); the receipts runner
//! reads notes + segments through the visibility-gated readers. No raw connection, no ungated read, no
//! new seal, no export. A sealed-and-not-session-unlocked item is invisible to every runner exactly as
//! it is to the app.

/// The `p`-th PERCENTILE (`p` in `[0, 100]`) of `values`, by the standard "nearest-rank on a sorted
/// copy" method (a stable, dependency-free estimator — no interpolation): sort ascending, then pick the
/// element at 0-based rank `ceil(p/100 · n) − 1`, clamped to `[0, n−1]`. `p = 0` ⇒ the min, `p = 100` ⇒
/// the max. `None` for an empty slice (no percentile of nothing). Pure: no DB, no clock; a fresh sorted
/// copy each call, so the caller's slice is never mutated. NaNs sort to the end (partial_cmp fallback),
/// which for the cosine distributions this measures never occurs (cosines are finite).
pub fn percentile(values: &[f32], p: f32) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    let p = p.clamp(0.0, 100.0);
    let mut sorted: Vec<f32> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Greater));
    let n = sorted.len();
    // Nearest-rank: rank = ceil(p/100 · n), 1-based; convert to a 0-based index and clamp.
    let rank_1based = ((p / 100.0) * n as f32).ceil() as usize;
    let idx = rank_1based.saturating_sub(1).min(n - 1);
    Some(sorted[idx])
}

/// A summary of a NULL cosine distribution: the count of pairs, the key high percentiles, and the
/// mean — everything a human needs to judge the semantic-link floors against the noise floor. Pure data.
#[derive(Debug, Clone, Copy)]
pub struct NullCosineSummary {
    /// Number of random pairs sampled.
    pub pairs: usize,
    /// Mean cosine over the sample.
    pub mean: f32,
    /// 50th percentile (median).
    pub p50: f32,
    /// 95th percentile.
    pub p95: f32,
    /// 99th percentile.
    pub p99: f32,
    /// 99.9th percentile.
    pub p999: f32,
    /// The single max cosine seen (the worst-case false-positive a floor at that value would admit).
    pub max: f32,
}

impl NullCosineSummary {
    /// Summarize a slice of pairwise cosines. Returns all-zero (pairs=0) for an empty slice — the
    /// caller renders that as "not enough items to sample". Pure.
    pub fn from_cosines(cosines: &[f32]) -> Self {
        if cosines.is_empty() {
            return NullCosineSummary {
                pairs: 0,
                mean: 0.0,
                p50: 0.0,
                p95: 0.0,
                p99: 0.0,
                p999: 0.0,
                max: 0.0,
            };
        }
        let mean = cosines.iter().sum::<f32>() / cosines.len() as f32;
        NullCosineSummary {
            pairs: cosines.len(),
            mean,
            p50: percentile(cosines, 50.0).unwrap_or(0.0),
            p95: percentile(cosines, 95.0).unwrap_or(0.0),
            p99: percentile(cosines, 99.0).unwrap_or(0.0),
            p999: percentile(cosines, 99.9).unwrap_or(0.0),
            max: percentile(cosines, 100.0).unwrap_or(0.0),
        }
    }
}

/// The COVERAGE FRACTION of one note: `receipts / claim_lines` (both computed the SAME way the receipts
/// pass does). `receipts` = the number of [`crate::summarize::grounding::ClaimAlignment`]s produced;
/// `claim_lines` = the number of note lines that ARE claims (cleared the same skip filters the pass
/// applies — front-matter / headings / code / blockquote / wikilink-only / too-short lines are NOT
/// claims and do not count against coverage). A note with ZERO claim lines has coverage `1.0` (vacuously
/// fully covered — nothing to receipt), matching the recall-vacuity convention in [`crate::eval`]. Pure:
/// takes the two already-computed counts, no DB. `receipts` is clamped to `claim_lines` (a receipt can
/// only attach to a counted claim line, but the clamp keeps the ratio in `[0, 1]` even if a caller
/// miscounts).
pub fn coverage_fraction(receipts: usize, claim_lines: usize) -> f32 {
    if claim_lines == 0 {
        return 1.0;
    }
    (receipts.min(claim_lines) as f32) / claim_lines as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── harness 1: percentile / null-cosine summary (LIGHT synthetic math) ───────────────────────

    #[test]
    fn percentile_empty_is_none() {
        assert_eq!(percentile(&[], 95.0), None);
    }

    /// RED-before-GREEN: the nearest-rank percentile over a known synthetic vector returns the exact
    /// element at the correct rank — this is the load-bearing math the semantic-link gate reports.
    #[test]
    fn percentile_nearest_rank_on_known_vector() {
        // 1..=100 in shuffled order; nearest-rank p95 = element at ceil(0.95·100)=95th (1-based) = 95.0,
        // p99 = 99.0, p50 = 50.0, p100 = 100.0, p0 = 1.0.
        let mut v: Vec<f32> = (1..=100).map(|x| x as f32).collect();
        // Shuffle deterministically (reverse) to prove the fn sorts internally.
        v.reverse();
        assert_eq!(percentile(&v, 0.0), Some(1.0));
        assert_eq!(percentile(&v, 50.0), Some(50.0));
        assert_eq!(percentile(&v, 95.0), Some(95.0));
        assert_eq!(percentile(&v, 99.0), Some(99.0));
        assert_eq!(percentile(&v, 100.0), Some(100.0));
    }

    #[test]
    fn percentile_p999_picks_top_of_a_thousand() {
        // 1..=1000; nearest-rank p99.9 = ceil(0.999·1000)=999th (1-based) = 999.0.
        let v: Vec<f32> = (1..=1000).map(|x| x as f32).collect();
        assert_eq!(percentile(&v, 99.9), Some(999.0));
        // Out-of-range p is clamped: p > 100 behaves as p100 (the max), p < 0 as p0 (the min).
        assert_eq!(percentile(&v, 150.0), Some(1000.0));
        assert_eq!(percentile(&v, -5.0), Some(1.0));
    }

    #[test]
    fn percentile_does_not_mutate_input() {
        let v = vec![3.0f32, 1.0, 2.0];
        let _ = percentile(&v, 50.0);
        assert_eq!(v, vec![3.0, 1.0, 2.0], "input slice must be untouched");
    }

    #[test]
    fn null_cosine_summary_over_synthetic_distribution() {
        // A synthetic "compressed e5" null distribution: unrelated pairs clustered 0.70–0.85.
        let cosines: Vec<f32> = (0..1000).map(|i| 0.70 + (i as f32) * 0.00015).collect();
        let s = NullCosineSummary::from_cosines(&cosines);
        assert_eq!(s.pairs, 1000);
        // p95 = element at rank 950 = 0.70 + 949*0.00015 ≈ 0.84235.
        let want_p95 = 0.70 + 949.0 * 0.00015;
        assert!(
            (s.p95 - want_p95).abs() < 1e-4,
            "p95 {} vs {}",
            s.p95,
            want_p95
        );
        // The max is the last element.
        let want_max = 0.70 + 999.0 * 0.00015;
        assert!((s.max - want_max).abs() < 1e-4);
        // Mean is well below the floor here — the point of the measurement.
        assert!(s.mean > 0.70 && s.mean < 0.85);
    }

    #[test]
    fn null_cosine_summary_empty_is_zeroed() {
        let s = NullCosineSummary::from_cosines(&[]);
        assert_eq!(s.pairs, 0);
        assert_eq!(s.mean, 0.0);
        assert_eq!(s.p95, 0.0);
        assert_eq!(s.max, 0.0);
    }

    // ── harness 2: receipts coverage fraction (LIGHT synthetic math) ─────────────────────────────

    /// RED-before-GREEN: the coverage fraction over known counts returns the exact ratio.
    #[test]
    fn coverage_fraction_basic_ratios() {
        assert!((coverage_fraction(3, 6) - 0.5).abs() < 1e-6);
        assert!((coverage_fraction(6, 6) - 1.0).abs() < 1e-6);
        assert!((coverage_fraction(0, 6) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn coverage_fraction_zero_claims_is_vacuously_one() {
        // No claim lines ⇒ nothing to receipt ⇒ 1.0 (matches the eval recall-vacuity convention).
        assert!((coverage_fraction(0, 0) - 1.0).abs() < 1e-6);
        assert!((coverage_fraction(5, 0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn coverage_fraction_clamps_receipts_to_claims() {
        // A miscount (more receipts than claim lines) can't push the ratio above 1.0.
        assert!((coverage_fraction(9, 6) - 1.0).abs() < 1e-6);
    }

    /// The pure coverage fraction wired against the REAL receipts pass over a synthetic (note,
    /// segments) fixture — proves the numerator/denominator this harness reports match what
    /// `align_claims_to_segments` actually emits (no model, deterministic). One note line quotes a
    /// segment nearly verbatim (earns a receipt); one is a pure paraphrase sharing no content tokens
    /// (no receipt) — so 1 of 2 claim lines is covered.
    #[test]
    fn coverage_fraction_matches_real_receipts_pass_on_fixture() {
        use crate::summarize::grounding::align_claims_to_segments;
        use crate::transcribe::types::Segment;

        let seg = |idx: i64, text: &str| Segment {
            idx,
            start_s: idx as f64 * 10.0,
            end_s: idx as f64 * 10.0 + 5.0,
            text: text.to_string(),
            speaker: Some("others".to_string()),
            confidence: Some(0.9),
        };
        let segments = vec![
            seg(
                0,
                "We decided to migrate the storage layer to SQLCipher next quarter.",
            ),
            seg(1, "The budget review is scheduled for Friday afternoon."),
        ];
        // Line 0: near-verbatim of segment 0 (high token overlap → receipt).
        // Line 1: unrelated paraphrase, no shared content tokens with any segment (→ no receipt).
        let note_lines = vec![
            "We decided to migrate the storage layer to SQLCipher next quarter.",
            "Everyone felt optimistic about the roadmap overall.",
        ];
        let alignments = align_claims_to_segments(&note_lines, &segments);
        // Count claim lines the SAME way this harness's runner does — via the pass's own emission for
        // the covered ones plus the lines that qualify as claims. Here both lines are long enough to be
        // claims, so claim_lines = 2 and receipts = 1.
        let receipts = alignments.len();
        let claim_lines = 2usize;
        let cov = coverage_fraction(receipts, claim_lines);
        assert_eq!(receipts, 1, "exactly the verbatim line earns a receipt");
        assert!((cov - 0.5).abs() < 1e-6, "1 of 2 claim lines covered");
    }

    // ── harness 3: PDF-bench synthetic chunk-count assertion (compile + tiny math) ────────────────

    /// The PDF bench's chunk step is `chunk_document_hierarchical` — proven headless over synthetic
    /// blocks so the runner's chunk-count reporting is wired correctly (no PDF, no model). A section
    /// with a heading and several paragraphs yields at least an L1 parent + one L0 leaf.
    #[test]
    fn pdf_bench_chunker_wires_over_synthetic_blocks() {
        use crate::extract::ExtractedBlock;
        let blocks = vec![
            ExtractedBlock {
                text: "The first paragraph of the design section with enough words to chunk."
                    .to_string(),
                page: Some(1),
                heading_path: Some("Design".to_string()),
            },
            ExtractedBlock {
                text: "A second paragraph under the same heading, also with sufficient content."
                    .to_string(),
                page: Some(1),
                heading_path: Some("Design".to_string()),
            },
        ];
        let chunks = crate::embed::chunk_document_hierarchical("bench-doc", &blocks);
        assert!(
            !chunks.is_empty(),
            "a non-empty document must produce chunks"
        );
        // At least one chunk is embeddable (L0/L2) — the vector count the bench reports is these.
        assert!(
            chunks.iter().any(|c| c.embed),
            "at least one chunk must be embeddable"
        );
    }

    // ══════════════════════════════════════════════════════════════════════════════════════════════
    //  HEAVY, ENV-GATED, #[ignore] RUNNERS — run MANUALLY on a real Mac. See
    //  docs/research/2026-07-18-brain-v3-calibration.md for the exact commands. NEVER run these in the
    //  normal loop: they load the real e5 model, generate/read a real PDF, or scan a whole vault.
    // ══════════════════════════════════════════════════════════════════════════════════════════════

    use std::collections::HashSet;

    /// Open a real murmur SQLCipher DB from `MURMUR_CALIB_DB` (path) + `MURMUR_CALIB_DEK` (64-hex),
    /// mirroring the bake-off env-open. Test-only; panics (with a clear message) if the env is unset or
    /// the DB won't open — this is a manual, human-driven runner, so a panic IS the error surface.
    fn open_calib_db() -> crate::storage::Db {
        let db_path = std::env::var("MURMUR_CALIB_DB")
            .expect("set MURMUR_CALIB_DB to a murmur SQLCipher DB path");
        let dek = std::env::var("MURMUR_CALIB_DEK")
            .expect("set MURMUR_CALIB_DEK to that DB's 64-hex DEK");
        crate::storage::Db::open_with_key(std::path::Path::new(&db_path), &dek)
            .expect("open calibration DB (check the DEK)")
    }

    /// Write a report to `$MURMUR_CALIB_OUT` when set (creating parent dirs); always also printed to
    /// stdout by the caller. Shared by the runners. NO PII (ids + numbers only).
    fn write_calib_report(text: &str) {
        let Ok(path) = std::env::var("MURMUR_CALIB_OUT") else {
            return;
        };
        if path.trim().is_empty() {
            return;
        }
        let p = std::path::Path::new(&path);
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(p, text).expect("write MURMUR_CALIB_OUT report");
        println!("calibration report written to MURMUR_CALIB_OUT");
    }

    /// ══ GATE 1 RUNNER: semantic-link threshold calibration ══
    ///
    /// Samples up to `MURMUR_CALIB_PAIRS` (default 2000) RANDOM pairs of VISIBLE, EMBEDDED items,
    /// computes each pair's cosine (over the shipped `item_centroid` centroids, via
    /// `Db::sampled_visible_item_centroids`), builds the NULL distribution, and reports its high
    /// percentiles vs `SEMANTIC_LINK_FLOOR`/`_STRONG`. HONEST: prints whether the embedder is REAL or
    /// the STUB (stub vectors are noise → the numbers are meaningless).
    ///
    /// Env:
    ///   MURMUR_CALIB_DB    — murmur SQLCipher DB path (a copy of the dev DB is fine),
    ///   MURMUR_CALIB_DEK   — that DB's 64-hex DEK,
    ///   MURMUR_CALIB_PAIRS — (optional) number of random pairs to sample, default 2000,
    ///   MURMUR_CALIB_MAXITEMS — (optional) max items to enumerate before pairing, default 5000,
    ///   MURMUR_CALIB_OUT   — (optional) write the markdown report here.
    #[test]
    #[ignore = "calibration: needs MURMUR_CALIB_DB/DEK + the embed model on a Mac (real vectors)"]
    fn calibrate_semantic_link_threshold_from_env() {
        let db = open_calib_db();
        let real = crate::embed::embed_model_present();
        let max_items: usize = std::env::var("MURMUR_CALIB_MAXITEMS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5000);
        let want_pairs: usize = std::env::var("MURMUR_CALIB_PAIRS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2000);

        // Empty unlocked set = OPEN content only (a sealed folder is invisible — the gate the app uses).
        let items = db
            .sampled_visible_item_centroids(max_items, &HashSet::new())
            .expect("sample visible item centroids");

        // Pair sampling: a deterministic LCG walk over distinct (i, j) index pairs so the run is
        // reproducible (no rand dep, matches the harness's dependency-free convention). If there are
        // fewer than 2 items, there are no pairs to sample.
        let n = items.len();
        let mut cosines: Vec<f32> = Vec::new();
        if n >= 2 {
            // A simple xorshift-ish deterministic index generator seeded from the item count so the
            // walk is stable for a fixed vault. We reject i==j and cap at `want_pairs` samples.
            let mut state: u64 = 0x9E3779B97F4A7C15 ^ (n as u64);
            let mut next = || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state
            };
            let mut sampled = 0usize;
            // Bound the loop so a tiny corpus (few possible pairs) can't spin forever chasing want_pairs.
            let max_attempts = want_pairs.saturating_mul(8).max(64);
            let mut attempts = 0usize;
            while sampled < want_pairs && attempts < max_attempts {
                attempts += 1;
                let i = (next() as usize) % n;
                let j = (next() as usize) % n;
                if i == j {
                    continue;
                }
                let cos = crate::transcribe::diarize::cosine(&items[i].2, &items[j].2);
                cosines.push(cos);
                sampled += 1;
            }
        }

        let summary = NullCosineSummary::from_cosines(&cosines);
        let report = format!(
            "# Semantic-link threshold calibration\n\n\
             - embedder: {embedder}\n\
             - visible embedded items sampled: {items}\n\
             - random pairs: {pairs}\n\
             - SHIPPED floors: SEMANTIC_LINK_FLOOR={floor}, SEMANTIC_LINK_STRONG={strong}\n\n\
             ## Null cosine distribution (random, presumed-UNrelated pairs)\n\n\
             | stat | cosine |\n|---|---:|\n\
             | mean | {mean:.4} |\n| p50 | {p50:.4} |\n| p95 | {p95:.4} |\n\
             | p99 | {p99:.4} |\n| p99.9 | {p999:.4} |\n| max | {max:.4} |\n\n\
             ## Reading\n\n\
             A floor is only meaningful ABOVE the null distribution's high percentiles. If p99 (or p99.9) \
             of RANDOM pairs already exceeds {floor}, then {floor} admits ~1% (or ~0.1%) of unrelated \
             pairs as links — raise it. If the high percentiles sit well below {floor}, the floor binds \
             cleanly.\n{stub_note}",
            embedder = if real {
                format!(
                    "REAL model ({})",
                    crate::embed::selected_embed_model().id
                )
            } else {
                "STUB (hash-bag) — NUMBERS ARE MEANINGLESS, download the embed model and re-run".to_string()
            },
            items = n,
            pairs = summary.pairs,
            floor = crate::links::SEMANTIC_LINK_FLOOR,
            strong = crate::links::SEMANTIC_LINK_STRONG,
            mean = summary.mean,
            p50 = summary.p50,
            p95 = summary.p95,
            p99 = summary.p99,
            p999 = summary.p999,
            max = summary.max,
            stub_note = if real {
                String::new()
            } else {
                "\n> **WARNING — STUB EMBEDDER.** The cosines above are from the deterministic hash-bag \
                 stub, not a real embedding model. They are NOT a calibration signal. Download the embed \
                 model and re-run.\n"
                    .to_string()
            },
        );
        println!("\n{report}");
        write_calib_report(&report);
    }

    /// ══ GATE 2 RUNNER: receipts coverage measurer ══
    ///
    /// Over a real vault, computes for each VISIBLE meeting-note the fraction of its claim lines that
    /// earn a receipt (`align_claims_to_segments` at the shipped overlap floor), and reports per-note +
    /// overall coverage. Coverage ONLY — receipt PRECISION (are they correct) needs hand labels and is
    /// explicitly out of scope. No embed model needed (the receipts pass is pure token overlap).
    ///
    /// Env:
    ///   MURMUR_CALIB_DB   — murmur SQLCipher DB path,
    ///   MURMUR_CALIB_DEK  — that DB's 64-hex DEK,
    ///   MURMUR_CALIB_LIMIT — (optional) max meetings to scan, default 500,
    ///   MURMUR_CALIB_OUT  — (optional) write the markdown report here.
    #[test]
    #[ignore = "calibration: needs MURMUR_CALIB_DB/DEK on a Mac (reads a real vault's notes + segments)"]
    fn measure_receipts_coverage_from_env() {
        use crate::summarize::grounding::align_claims_to_segments;

        let db = open_calib_db();
        let limit: i64 = std::env::var("MURMUR_CALIB_LIMIT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(500);
        let unlocked = HashSet::new(); // OPEN content only — the app's gate.

        let meetings = db
            .list_meetings_visible(limit, &unlocked, None)
            .expect("list visible meetings");

        let mut total_receipts = 0usize;
        let mut total_claim_lines = 0usize;
        let mut notes_measured = 0usize;
        let mut per_note_covs: Vec<f32> = Vec::new();
        // Per-note lines (id + coverage) — ids only, NO note/transcript text (no PII).
        let mut rows: Vec<String> = Vec::new();

        for m in &meetings {
            // GATE: the AI note is read ONLY if its folder is visible (`get_note_if_visible` applies
            // `visibility_clause` — a sealed-not-unlocked meeting returns None and is skipped here). The
            // segment read below is gated by the SAME folder's `meeting_is_visible` check, so no sealed
            // content is ever read (defense-in-depth on top of the seal-time text blanking).
            let Some(note) = db
                .get_note_if_visible(&m.id, &unlocked)
                .expect("note if visible")
            else {
                continue;
            };
            if !db
                .meeting_is_visible(&m.id, &unlocked)
                .expect("meeting visibility gate")
            {
                continue; // belt-and-braces: never read segments for a non-visible meeting.
            }
            let segments = db
                .get_segments(&m.id)
                .expect("segments for visible meeting");
            if segments.is_empty() {
                continue; // no transcript ⇒ receipts are undefined; skip (not counted).
            }
            let note_md = note.markdown;
            let lines: Vec<&str> = note_md.lines().collect();
            let alignments = align_claims_to_segments(&lines, &segments);
            // Claim-line count = the receipts pass's OWN notion of a claim line. We approximate it by
            // re-running the same skip filters the pass applies — but the pass doesn't expose that
            // count, so we use the number of lines that COULD be claims via the shared predicate.
            let claim_lines = count_claim_lines(&lines);
            if claim_lines == 0 {
                continue; // a note with no claims is vacuous coverage; skip from the aggregate.
            }
            let receipts = alignments.len();
            total_receipts += receipts;
            total_claim_lines += claim_lines;
            notes_measured += 1;
            let cov = coverage_fraction(receipts, claim_lines);
            per_note_covs.push(cov);
            rows.push(format!(
                "| {id} | {receipts} | {claim_lines} | {cov:.3} |",
                id = m.id
            ));
        }

        let overall = coverage_fraction(total_receipts, total_claim_lines);
        let mean_per_note = if per_note_covs.is_empty() {
            0.0
        } else {
            per_note_covs.iter().sum::<f32>() / per_note_covs.len() as f32
        };
        let median_per_note = percentile(&per_note_covs, 50.0).unwrap_or(0.0);

        let mut report = format!(
            "# Receipts coverage\n\n\
             - notes measured (visible, with transcript + ≥1 claim line): {notes}\n\
             - overall coverage (Σreceipts / Σclaim-lines): {overall:.3}\n\
             - mean per-note coverage: {mean:.3}\n\
             - median per-note coverage: {median:.3}\n\n\
             > Coverage ONLY. This measures what FRACTION of claim lines earn a receipt, NOT whether the \
             receipts point at the RIGHT second of audio (precision needs hand labels — out of scope).\n\n\
             ## Per-note (ids only, no content)\n\n\
             | meeting_id | receipts | claim_lines | coverage |\n|---|---:|---:|---:|\n",
            notes = notes_measured,
            overall = overall,
            mean = mean_per_note,
            median = median_per_note,
        );
        for r in &rows {
            report.push_str(r);
            report.push('\n');
        }
        println!("\n{report}");
        write_calib_report(&report);
    }

    /// Count the note lines that QUALIFY as claim lines, applying the SAME skip filters
    /// `align_claims_to_segments` uses (front-matter / headings / code fences / blockquotes /
    /// wikilink-only / too-short). This mirrors the pass's internal predicate so the coverage
    /// denominator matches its numerator. Test-only (the runner's denominator helper). Kept
    /// intentionally close to the pass's own line loop; if that loop's filters change, update here too.
    fn count_claim_lines(note_lines: &[&str]) -> usize {
        // A leading YAML front-matter block is skipped.
        let mut in_fm = false;
        let mut fm_seen = false;
        let mut in_code = false;
        let mut count = 0usize;
        for (i, &line) in note_lines.iter().enumerate() {
            let t = line.trim();
            // Front-matter: a leading `---` on line 0 opens it; the next `---` closes it.
            if i == 0 && t == "---" {
                in_fm = true;
                fm_seen = true;
                continue;
            }
            if in_fm {
                if t == "---" {
                    in_fm = false;
                }
                continue;
            }
            let _ = fm_seen;
            if t.starts_with("```") || t.starts_with("~~~") {
                in_code = !in_code;
                continue;
            }
            if in_code {
                continue;
            }
            if t.is_empty() || t.starts_with('#') || t.starts_with('>') {
                continue;
            }
            // Strip a leading list/checkbox marker for the token count (mirror `unit_content`).
            let unit = t
                .trim_start_matches(['-', '*', '+'])
                .trim_start()
                .trim_start_matches("[ ]")
                .trim_start_matches("[x]")
                .trim_start();
            // Wikilink-only lines are citations, not claims.
            let stripped = unit.trim_start_matches("[[").trim_end_matches("]]");
            if unit.starts_with("[[") && unit.ends_with("]]") && !stripped.contains(' ') {
                continue;
            }
            // Too-short lines (fewer than a few word-ish tokens) are not claims. A cheap whitespace
            // token count is a conservative proxy for the pass's content-token floor.
            if unit.split_whitespace().count() < 3 {
                continue;
            }
            count += 1;
        }
        count
    }

    /// ══ GATE 3 RUNNER: large-PDF ingest bench ══
    ///
    /// Times the ingest hot path — `extract_blocks` → `chunk_document_hierarchical` → sub-batched
    /// embed — over a big TEXT PDF and reports wall-clock per stage plus block/chunk/vector counts. Uses
    /// the same sub-batch size (8) as the shipped `embed_in_sub_batches`. HONEST: scanned-PDF OCR
    /// FIDELITY + PDFKit behavior can only be validated on a real signed Mac; this bench targets a
    /// TEXT-layer PDF (throughput), not OCR quality.
    ///
    /// Generate a big text PDF (macOS, no extra deps) — e.g. from a large text file:
    ///   ```sh
    ///   # a ~large plain-text corpus → PDF via the built-in CUPS text filter:
    ///   yes "The quick brown fox jumps over the lazy dog. " | head -n 40000 > /tmp/big.txt
    ///   cupsfilter /tmp/big.txt > /tmp/big.pdf 2>/dev/null
    ///   # (or: open /tmp/big.txt in TextEdit → File ▸ Export as PDF)
    ///   ```
    ///
    /// Env:
    ///   MURMUR_CALIB_PDF — path to the big text PDF to bench,
    ///   MURMUR_CALIB_OUT — (optional) write the markdown report here.
    #[test]
    #[ignore = "calibration: needs MURMUR_CALIB_PDF (a big text PDF) on a Mac + the embed model"]
    fn bench_large_pdf_ingest_from_env() {
        use std::time::Instant;

        let pdf_path =
            std::env::var("MURMUR_CALIB_PDF").expect("set MURMUR_CALIB_PDF to a big text PDF path");
        let real = crate::embed::embed_model_present();
        // The real embedder wants the Metal forward-pass opt-in (same as the bake-off runners).
        std::env::set_var("MURMUR_TEST_REAL_EMBED", "1");
        let embedder = crate::embed::active_embedder();

        let path = std::path::Path::new(&pdf_path);
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "bench-doc".to_string());

        // 1) Extract.
        let t0 = Instant::now();
        let blocks = crate::extract::extract_blocks(path, "pdf", &crate::extract::no_progress)
            .expect("extract_blocks over the bench PDF");
        let extract_ms = t0.elapsed().as_millis();

        // 2) Chunk.
        let t1 = Instant::now();
        let chunks = crate::embed::chunk_document_hierarchical(&name, &blocks);
        let chunk_ms = t1.elapsed().as_millis();

        // 3) Embed the embeddable chunks in sub-batches of 8 (the shipped `embed_in_sub_batches` size).
        const SUB_BATCH: usize = 8;
        let embed_texts: Vec<String> = chunks
            .iter()
            .filter(|c| c.embed)
            .map(|c| c.embed_text.clone())
            .collect();
        let t2 = Instant::now();
        let mut vectors = 0usize;
        for batch in embed_texts.chunks(SUB_BATCH) {
            let vecs = embedder
                .embed_passage(batch)
                .expect("embed a sub-batch of the bench PDF");
            vectors += vecs.len();
        }
        let embed_ms = t2.elapsed().as_millis();

        let total_ms = extract_ms + chunk_ms + embed_ms;
        let report = format!(
            "# Large-PDF ingest bench\n\n\
             - embedder: {embedder}\n\
             - pdf: {name} ({file_bytes} bytes on disk)\n\
             - blocks extracted: {blocks}\n\
             - chunks (all levels): {chunks}\n\
             - embeddable chunks (L0/L2): {embeddable}\n\
             - vectors produced: {vectors}\n\n\
             ## Wall-clock (single run, warm process)\n\n\
             | stage | ms |\n|---|---:|\n\
             | extract_blocks | {extract_ms} |\n| chunk_document_hierarchical | {chunk_ms} |\n\
             | embed (sub-batched, size {sub}) | {embed_ms} |\n| **total** | **{total_ms}** |\n\n\
             > Text-layer throughput only. Scanned-PDF OCR fidelity + PDFKit behavior need a real signed \
             Mac and are NOT measured here.\n{stub_note}",
            embedder = if real {
                format!("REAL model ({})", crate::embed::selected_embed_model().id)
            } else {
                "STUB (hash-bag) — embed timing is not representative of the real model".to_string()
            },
            name = name,
            file_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            blocks = blocks.len(),
            chunks = chunks.len(),
            embeddable = embed_texts.len(),
            vectors = vectors,
            extract_ms = extract_ms,
            chunk_ms = chunk_ms,
            embed_ms = embed_ms,
            sub = SUB_BATCH,
            total_ms = total_ms,
            stub_note = if real {
                String::new()
            } else {
                "\n> **NOTE — STUB EMBEDDER.** The embed stage timing reflects the hash-bag stub, not the \
                 real e5 model. Download the model and re-run for a representative embed time.\n"
                    .to_string()
            },
        );
        println!("\n{report}");
        write_calib_report(&report);
    }
}
