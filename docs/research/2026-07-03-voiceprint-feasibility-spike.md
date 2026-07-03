<!-- Generated 2026-07-03 during the goal-loop follow-up (after the brain-hardening program). Point-in-time. -->
# Spike: on-device cross-meeting speaker voiceprint identity — is it buildable on our stack?

## Verdict: YES — buildable with the EXISTING dependency, no new crate. Accuracy is @Mac-gated.

The single gating unknown from the analysis ("does the sherpa-onnx Rust binding expose the per-span CAM++ embedding VECTOR standalone, or only run clustering internally?") is resolved.

## Findings

- Today's diarizer (`src-tauri/src/transcribe/diarize.rs`) uses `sherpa_onnx::OfflineSpeakerDiarization`, which runs the full pipeline (pyannote segmentation → CAM++ embedding → fast clustering) and returns only per-meeting cluster **labels** (`SpeakerSpan { speaker: usize }`) — NOT the embedding vectors. So cross-meeting identity is impossible with the current call alone.
- The **same crate** (`sherpa-onnx = "1.13"`, already compiled) exposes, separately from diarization:
  - **`SpeakerEmbeddingExtractor`** — computes a speaker embedding vector (`Vec<f32>`) from audio (consumed via an `OnlineStream`). This is the standalone extractor we need.
  - **`SpeakerEmbeddingManager`** — an in-memory index of named speaker embeddings with `register` + `get_best_matches` (cosine search) + `SpeakerEmbeddingMatch`.
  - `SpeakerEmbeddingExtractorConfig` — already imported by `diarize.rs` (it configures the embedding leg of the diarizer), so the CAM++ model (`wespeaker_en_voxceleb_CAM++.onnx`) is already downloaded + wired.
- Sources: docs.rs/sherpa-onnx, github.com/thewh1teagle/sherpa-rs, github.com/k2-fsa/sherpa-onnx (C-API `SpeakerEmbeddingExtractor` + `SpeakerEmbeddingManager`, mirrored across Go/C#/Rust bindings).

## Implementation shape (prod-ready plumbing; accuracy @Mac-gated)

1. **Extract** — after diarization relabels `others-N`, run `SpeakerEmbeddingExtractor::compute` over each span's system-stream samples → one CAM++ vector per per-meeting speaker cluster. (No new model download — the CAM++ file is already present when diarization is enabled.)
2. **Persist (CK-sealed + gated)** — additive `speaker_voiceprints` table: `{ id, label (e.g. [[Anna]]), embedding BLOB, source_meeting_id, valid_from/valid_to }`. Voiceprints are PII-at-rest → gate reads by visibility + purge-on-seal like `user_facts`/`facts`; lock-security review mandatory.
3. **Match** — on a new meeting, cosine-match each `others-N` cluster embedding against enrolled voiceprints (`SpeakerEmbeddingManager` or a manual cosine over the gated table) → surface a "This looks like Anna?" suggestion (confirm-to-bind).
4. **Enroll-on-rename** — when the user renames a speaker (`rename_speaker`), store that span's embedding as the enrolled voiceprint bound to the [[Person]] entity. This feeds the self-assembling Person graph + fact/commitment attribution.
5. **Flag** — default OFF (like `diarize_others` / `semantic_search_enabled`); depends on `diarize_others` being on.

## Honesty bar (why "prod-ready plumbing" ≠ "proven")

- **Re-identification accuracy** across different mics/meetings, cluster purity, and the cosine threshold can ONLY be validated on a signed build on a real Mac with real multi-speaker audio + the models downloaded. Headless `cargo test` proves the extract→persist→match→gate plumbing + lock invariants, NOT the biometric accuracy. Same pattern as the e5 embedder / AEC / diarization already in the tree.
- **Legal:** capturing a non-consenting remote participant's voice biometric — even on-device, never-egressed, encrypted-at-rest — is UNTESTED under BIPA/CIPA (the theory the Otter/Fireflies cloud suits turn on is *collection*, largely regardless of storage location). Almost certainly far safer than cloud retention, but not "immune." Ship behind an explicit opt-in + a consent/disclosure note; get counsel before any default-on.

## Recommendation

Implementable as a focused, lock-reviewed increment (M) once the brain-completion increment (thread-turn memory extraction + Ask injection + /people CRM) lands. Plumbing is prod-ready-verifiable headless; the accuracy + legal posture are the honest @Mac / counsel gates — flag them, don't hide them.
