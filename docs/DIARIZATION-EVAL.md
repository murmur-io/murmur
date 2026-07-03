<!-- Turnkey protocol for MEASURING speaker diarization + voiceprint re-ID quality on YOUR real Mac. Decides whether the diarizer splits speakers correctly and whether the placeholder cosine threshold (0.5) is the right operating point. Mirrors docs/RAG-BAKEOFF.md. -->
# Diarization / voiceprint eval — is speaker attribution trustworthy, and is 0.5 the right cosine? (run on your Mac)

Murmur diarizes the system ("others") stream into `others-{n}` speakers and, opt-in, stores a per-cluster CAM++ **voiceprint** that a cosine matcher (`suggest_voiceprint_labels`) re-identifies across meetings — gated at the **unvalidated placeholder** `VOICEPRINT_MATCH_THRESHOLD = 0.5` (`transcribe/diarize.rs`). Whether the diarizer splits speakers correctly, and whether `0.5` over- or under-enrolls, can only be answered **on real multi-speaker audio you have hand-labeled**. This protocol produces those numbers.

> **The metric math is unit-tested and green in `cargo test --lib` — but a green build is NOT a DER or threshold claim.** It proves the harness typechecks; the real numbers below need labeled audio + the ONNX models on a Mac. `0.5` stays a documented placeholder until you run the sweep.

---

## What you get

- **DER** (Diarization Error Rate) + its `miss / false-alarm / confusion` seconds breakdown, over an optimal 1:1 reference→cluster mapping.
- **cluster purity / coverage** (how single-speaker each cluster is / how completely each speaker is captured).
- a **cosine threshold sweep** — FAR/FRR at every threshold `0.00…1.00`, the **EER** (equal-error-rate crossover), and the **best-F1** threshold — so you can see whether `0.5` is right for your voices.
- **re-ID precision/recall** of the shipped matcher at `0.5` (leave-one-out over your labeled clusters).

---

## Stage 1 — hand-label reference speaker turns (the gold set)

Nothing headless can synthesize a trustworthy DER reference — you must label who spoke when. For each recording, write a reference JSON of ground-truth speaker turns. A template lives at `src-tauri/src/eval/fixtures/diarization-sample.json`; copy it and fill in real turns:

```json
{
  "meeting": "standup-2026-07-03",
  "reference": [
    { "start": 0.0,  "end": 4.2,  "speaker": "Anna" },
    { "start": 4.2,  "end": 9.8,  "speaker": "Bartek" },
    { "start": 9.8,  "end": 14.5, "speaker": "Anna" }
  ]
}
```
- `start` / `end` are seconds into the **system-stream WAV** (the "others" side — the mic/`me` stream is never diarized).
- `speaker` is the ground-truth identity string (a real name or a stable pseudonym). Use the SAME identity string across recordings for the same person — that is what the re-ID sweep measures.
- Label the whole recording; gaps of silence are fine (they simply aren't covered by any turn).

Use a player that shows a timecode; a couple of minutes of careful labeling per recording is enough to get a real signal.

## Stage 2 — find the diarization ONNX models

The two models are already on disk after any diarized recording, in `models_dir()`:
- `~/Library/Application Support/MeetNotes/models/sherpa-pyannote-segmentation-3.0.onnx`
- `~/Library/Application Support/MeetNotes/models/wespeaker_en_voxceleb_CAM++.onnx`

(`MeetNotes-dev/models/…` for a dev build.) If they're absent, do one diarized recording first, or trigger the model download from the app. The harness passes them **explicitly** so it never triggers a network download.

## Stage 3 — run the DER runner (one recording)

The end-to-end runs are `#[ignore]`d tests driven by env vars (no recompile to change inputs). They open **no** murmur DB, need **no** DEK / Touch ID, and perform **no** egress.

```bash
source ~/.cargo/env
cd src-tauri
MURMUR_DER_WAV=/path/to/system-stream.wav \
MURMUR_DER_REF=/path/to/standup-2026-07-03.json \
MURMUR_DIARIZE_SEG_MODEL="$HOME/Library/Application Support/MeetNotes/models/sherpa-pyannote-segmentation-3.0.onnx" \
MURMUR_DIARIZE_EMB_MODEL="$HOME/Library/Application Support/MeetNotes/models/wespeaker_en_voxceleb_CAM++.onnx" \
MURMUR_DER_COLLAR=0.25 \
cargo test --lib eval::diarization::tests::run_der_over_labeled_audio_from_env -- --ignored --nocapture
```
- `MURMUR_DER_COLLAR` is optional (default `0.0`); NIST uses `0.25` to forgive boundary ambiguity around each reference turn edge.
- Output:
```
Diarization Error Rate — total reference speaker time 132.40s
DER                      0.1837
  miss (s)                  6.10
  false alarm (s)           3.20
  confusion (s)            14.99
  matched speakers             3
cluster purity           0.8721
cluster coverage         0.9013
purity/coverage F1       0.8865
```
Lower DER is better; `< ~0.25` is generally usable, `> ~0.5` means the diarizer is struggling on this audio. High **confusion** = clusters swapped/merged; high **miss** = speech the diarizer dropped; high **false alarm** = it invented speech.

## Stage 4 — run the threshold sweep + re-ID (multiple recordings)

To validate the `0.5` cosine you need several recordings sharing some speakers. Write a manifest of `{wav, reference}` pairs:

```json
[
  { "wav": "/path/to/standup.wav",  "reference": "/path/to/standup.json" },
  { "wav": "/path/to/planning.wav", "reference": "/path/to/planning.json" }
]
```
Then:
```bash
MURMUR_DER_MANIFEST=/path/to/manifest.json \
MURMUR_DIARIZE_SEG_MODEL="$HOME/Library/Application Support/MeetNotes/models/sherpa-pyannote-segmentation-3.0.onnx" \
MURMUR_DIARIZE_EMB_MODEL="$HOME/Library/Application Support/MeetNotes/models/wespeaker_en_voxceleb_CAM++.onnx" \
cargo test --lib eval::diarization::tests::run_threshold_sweep_over_manifest_from_env -- --ignored --nocapture
```
Output = a per-threshold FAR/FRR table, the EER, the best-F1 threshold, and the shipped matcher's re-ID precision/recall at `0.5`:
```
Verification threshold sweep — 51 points
  thr   tp   fp   tn   fn    prec     rec      f1     far     frr     acc
------------------------------------------------------------------------
 0.48   18    3   27    2  0.8571  0.9000  0.8780  0.1000  0.1000  0.9000
 0.50   17    2   28    3  0.8947  0.8500  0.8718  0.0667  0.1500  0.9000
 ...
EER: threshold 0.46, error 0.1000
operating point (threshold 0.50): prec 0.8947 rec 0.8500 f1 0.8718 far 0.0667 frr 0.1500
best-F1 threshold = 0.46 (F1 0.8889)
re-ID @ 0.50: precision 0.850 recall 0.810 f1 0.830 (tp 17 fp 2 fn 4 tn 0)
```

### Reading it — is 0.5 right?
- **EER threshold ≈ 0.5** → the placeholder is well-calibrated for your voices; leave it.
- **EER threshold materially < 0.5** (e.g. 0.4) → `0.5` is too STRICT (under-enrolls, high FRR / misses real matches); lower it.
- **EER threshold materially > 0.5** (e.g. 0.6) → `0.5` is too LOOSE (over-enrolls, high FAR / wrong-person matches — the privacy-worse failure); raise it.
- **best-F1** gives the accuracy-optimal operating point; **EER** gives the balanced-error point. For a voice biometric, prefer the FAR-conservative side (fewer wrong-person enrollments).

**Send me the two tables + which recordings** and I'll set the threshold from real evidence. Changing `VOICEPRINT_MATCH_THRESHOLD` is a one-line follow-up — this harness only MEASURES it.

---

## Honesty notes
- The metric math (DER decomposition + optimal mapping + purity/coverage + re-ID + sweep/EER) is pure and unit-tested (`cargo test --lib eval::diarization` — runs in the normal loop). That proves the harness is correct, **not** that diarization is accurate.
- The real numbers need your hand-labeled multi-speaker audio + the two ONNX models on a Mac. The diarizer inference is CPU-ONNX, so the runners can in principle run unsigned once you supply labeled audio — but end-to-end voiceprint fidelity and the enroll/re-label UX only truly verify on a signed build.
- Label from recordings where you KNOW who spoke, use stable identity strings across recordings, and keep the label set fixed across runs so threshold comparisons are apples-to-apples.
- The harness is READ-ONLY: it opens no murmur DB, adds no gated-read bypass, registers no command, triggers no model download, and never egresses. Identity strings come from your own reference files and are reported as aggregate counts, never logged per-person.
