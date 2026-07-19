# Brain v3 — calibration harnesses (the program's own unrun release gates)

**Status:** harnesses BUILT and compiling; the LIGHT synthetic-math is green in `cargo test --lib`.
The actual measured numbers are **NOT YET RUN** — each gate needs a real Mac with the real embed model
and/or a real vault. This doc is the runbook + the results placeholder the operator fills in later.

Brain v3 shipped three mechanisms whose OPERATING POINTS were never measured on real models/vaults.
`cargo test --lib` pins the *logic* of each; it cannot pin the *numbers*. These three harnesses close
that gap. Each mirrors the `eval::bakeoff` shape: a **pure, headless-testable math core** (unit-tested
in the normal loop) + a **heavy `#[ignore]` + env-gated runner** the operator runs on a Mac.

- Code: `src-tauri/src/eval/calibration.rs` (pure math: `percentile`, `NullCosineSummary`,
  `coverage_fraction`; the three `#[ignore]` runners in its `#[cfg(test)]` block).
- Gated read helper: `Db::sampled_visible_item_centroids` (`src-tauri/src/storage/db.rs`, `#[cfg(test)]`,
  enumerates ONLY visibility-gated items — no new ungated read path).

All three runners are **READ-ONLY** and route every read through the same `visibility_clause`-gated
readers the app uses; a sealed-and-not-session-unlocked item is invisible to them exactly as it is to
the app. No seal, no export, no lock surface touched.

> Honesty bar: the PURE MATH is proven headless. The NUMBERS are real-Mac-only. Every runner also prints
> whether the embedder is the REAL model or the deterministic STUB and states the numbers are meaningless
> under the stub.

---

## Gate 1 — Semantic-link threshold calibration

### (a) What it measures + why it's the release gate

The semantic auto-linker keeps a candidate iff `cos >= SEMANTIC_LINK_FLOOR` (0.80) and (`mutual` OR
`cos >= SEMANTIC_LINK_STRONG` (0.88)) — `src-tauri/src/links.rs`. Those two floors were chosen as e5
**start values** and, per the brain-v3 audit, were **never calibrated against a real vault**. e5's cosine
range is compressed: unrelated pairs routinely sit at 0.75–0.85, so 0.80 may barely bind (or admit noise
as links). This is the program's OWN release gate — it decides whether a "semantic link" means anything.

The runner samples ~2000 **random** visible item pairs (presumed UNrelated), computes each pair's cosine
from the shipped `item_centroid` centroids, and builds the **NULL distribution**. It reports the null's
mean / p50 / p95 / p99 / p99.9 / max vs the shipped floors. Reading: **a floor is only meaningful ABOVE
the null's high percentiles.** If p99 of random pairs already exceeds 0.80, the floor admits ~1% of
unrelated pairs as links → raise it. If the high percentiles sit well below 0.80, the floor binds cleanly.
(This measures the false-positive floor only; the *true-positive* side — do related pairs clear 0.80 —
needs a hand-labeled positive set, a follow-up.)

### (b) How to run it

```sh
source ~/.cargo/env
export MURMUR_CALIB_DB=/path/to/a/copy/of/meetnotes.sqlite     # a WAL-checkpointed copy of the dev DB
export MURMUR_CALIB_DEK=<the 64-hex DEK for that DB>           # the dev DEK for a dev DB
export MURMUR_CALIB_PAIRS=2000                                 # optional, default 2000
export MURMUR_CALIB_MAXITEMS=5000                             # optional, default 5000
export MURMUR_CALIB_OUT=docs/research/results/semantic-link-null.md   # optional, writes the report
# The embed model MUST be on disk for real vectors (else the run prints STUB and the numbers are noise).
cargo test --lib eval::calibration::calibrate_semantic_link_threshold_from_env -- --ignored --nocapture
```

### (c) Results

**NOT YET RUN — needs a real Mac + the real embed model + a populated vault.**

| stat | cosine |
|---|---:|
| mean | _tbd_ |
| p50 | _tbd_ |
| p95 | _tbd_ |
| p99 | _tbd_ |
| p99.9 | _tbd_ |
| max | _tbd_ |

Shipped floors for comparison: `SEMANTIC_LINK_FLOOR = 0.80`, `SEMANTIC_LINK_STRONG = 0.88`.
Verdict (fill in after the run): _does 0.80 sit above the null's p99/p99.9, or in the noise?_

---

## Gate 2 — Receipts coverage measurer

### (a) What it measures + why it's the release gate

Receipts (`summarize::grounding::align_claims_to_segments`, floor `RECEIPT_MIN_OVERLAP = 0.5`) attach each
note claim-line to the transcript second it derives from. The audit flagged that coverage on real
LLM-**paraphrased** notes is plausibly **< 50%** — a low-coverage receipts feature is a weak feature. This
gate measures what FRACTION of claim-lines earn a receipt, per note + overall, over a real vault, so the
operating point is a measured number rather than a guess.

**Coverage ONLY.** This says nothing about receipt PRECISION (do the receipts point at the *right* second)
— that needs hand labels and is explicitly out of scope. The runner says so in its output.

### (b) How to run it

```sh
source ~/.cargo/env
export MURMUR_CALIB_DB=/path/to/a/copy/of/meetnotes.sqlite
export MURMUR_CALIB_DEK=<the 64-hex DEK>
export MURMUR_CALIB_LIMIT=500                                  # optional, max meetings to scan, default 500
export MURMUR_CALIB_OUT=docs/research/results/receipts-coverage.md   # optional
# No embed model needed — the receipts pass is pure token overlap.
cargo test --lib eval::calibration::measure_receipts_coverage_from_env -- --ignored --nocapture
```

### (c) Results

**NOT YET RUN — needs a real Mac + a real vault (notes with transcripts).**

- notes measured (visible, with transcript + ≥1 claim line): _tbd_
- overall coverage (Σreceipts / Σclaim-lines): _tbd_
- mean per-note coverage: _tbd_
- median per-note coverage: _tbd_

Verdict (fill in after the run): _is coverage above ~50%, or does the audit's concern hold?_

---

## Gate 3 — Large-PDF ingest bench

### (a) What it measures + why it's the release gate

The document-ingest hot path is `extract_blocks` → `chunk_document_hierarchical` → sub-batched embed
(`src-tauri/src/extract`, `src-tauri/src/embed.rs`). Brain v3 PR-4 hardened it (per-page OCR, page caps,
universal text ceiling) but its **throughput on a realistically large document was never timed**. This
gate times each stage on a big TEXT PDF and reports wall-clock + block/chunk/vector counts, so the ingest
surface's performance is a measured number (the operator can see whether a 300-page import is seconds or
minutes).

**Text-layer throughput only.** Scanned-PDF OCR **fidelity** + PDFKit behavior can only be validated on a
real signed Mac and are NOT measured here — the runner states this. (RSS/peak-memory is awkward to probe
portably on macOS from a test process, so the bench reports wall-clock + counts; a peak-RSS probe is a
possible follow-up via `task_info`/`mach` if a memory number becomes load-bearing.)

### (b) How to run it

Generate a big text PDF (macOS, no extra deps):

```sh
# a large plain-text corpus → PDF via the built-in CUPS text filter:
yes "The quick brown fox jumps over the lazy dog. " | head -n 40000 > /tmp/big.txt
cupsfilter /tmp/big.txt > /tmp/big.pdf 2>/dev/null
# (or: open /tmp/big.txt in TextEdit → File ▸ Export as PDF)
```

Then:

```sh
source ~/.cargo/env
export MURMUR_CALIB_PDF=/tmp/big.pdf
export MURMUR_CALIB_OUT=docs/research/results/pdf-ingest-bench.md   # optional
# The real embed model is preferred (the embed stage timing is only representative with it).
cargo test --lib eval::calibration::bench_large_pdf_ingest_from_env -- --ignored --nocapture
```

### (c) Results

**NOT YET RUN — needs a real Mac + a big text PDF (+ the embed model for a representative embed time).**

| stage | ms |
|---|---:|
| extract_blocks | _tbd_ |
| chunk_document_hierarchical | _tbd_ |
| embed (sub-batched, size 8) | _tbd_ |
| **total** | _tbd_ |

Counts (fill in after the run): blocks _tbd_ / chunks _tbd_ / embeddable _tbd_ / vectors _tbd_.

---

## What IS proven headless (green in `cargo test --lib eval::calibration`)

The PURE MATH each gate reports:

- `percentile` — nearest-rank percentile over a sorted copy (p0/p50/p95/p99/p99.9/p100, clamped, input
  never mutated). Pinned against a known 1..=100 and 1..=1000 vector.
- `NullCosineSummary::from_cosines` — mean + the key percentiles over a synthetic compressed-e5 null
  distribution.
- `coverage_fraction` — `receipts / claim_lines` with the vacuity + clamp conventions, AND wired against
  the REAL `align_claims_to_segments` over a synthetic (note, segments) fixture (1 of 2 claim lines
  covered — proving the numerator/denominator match the real pass).
- `chunk_document_hierarchical` wiring over synthetic blocks (the bench's chunk step, headless).

11 LIGHT tests pass; the 3 `#[ignore]` runners compile clean and are skipped in the normal loop.

## What is NOT proven headless (real-Mac-only)

- Every measured NUMBER above (all three gates) — real e5 vectors / a real vault / a real PDF.
- The stub embedder makes gate-1 cosines and gate-3 embed timing **meaningless**; the runners print a loud
  STUB warning when the model is absent.
