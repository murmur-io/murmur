<!-- Generated 2026-07-03 at the end of the /goal loop that built out the mega-analysis roadmap. Point-in-time. -->
# Brain program — shipped (2026-07-03)

Execution record for the "PEŁNOPRAWNY BRAIN + default-flip" program derived from `docs/research/2026-07-03-murmur-full-analysis-strengths-gaps-killer-features.md`. Built via ship-feature Workflows (rust-tauri-dev / angular-zoneless-dev builders → **adversarial-verifier** + **lock-security-reviewer** gates → full `scripts/ci.sh`). Every increment: commit as QueaT, stacked PR to `murmur-io/murmur`, no Claude trailers.

## What shipped (3 stacked PRs, each triple-verified)

### PR #153 — `feat/brain-hardening` (A + B + C)
- **A — defaults (make the local-first moat visible; drop an unproven default-ON path).** `post_aec_enabled` ON→OFF (unproven v0.1 AEC3 crate, opt-in until real-Mac-verified); whisper `model_size` large-v3→small (RAM-safe, matches onboarding, all sizes still selectable); NER name-redaction optional-download plumbing verified; onboarding **explicit "Fully local / Cloud via redaction firewall" posture** (Cloud stays default) + expose system-audio/diarization.
- **B — semantic RAG (configurable + measurable, still default-OFF).** Embedder registry (`EMBED_MODELS`) with **mmlw-e5-small** added as a web-verified **BERT/384 zero-migration** option (same `query:`/`passage:` prefixes as e5, no new dep); `select_embed_model` + reindex trigger. **Bake-off harness** (`src-tauri/src/eval/`): recall@k / nDCG@k / MRR over FTS vs semantic vs hybrid, deterministic metric unit tests + `#[ignore]` real run + fixture + `RAG-BAKEOFF.md`.
- **C — cross-meeting user memory (lock-touching).** Additive bitemporal `user_facts` table (provenance-anchored); gated `list_user_facts_visible`; purge-on-seal in the seal tx + at-rest reconcile; deterministic memory brief injected into the `@brain` agentic loop like `live_transcript` (rides the existing RedactingProvider + consent gate, **no new egress**); `get/forget/clear_user_memory`; Brain-page Memory view.

### PR #154 — `feat/brain-completion` (D) — stacked on #153
- **D5** — user facts also extracted from the meeting's persisted `@brain` **thread turns** (highest-signal "zapamiętaj, że…"), gated to the meeting's own visible interactions.
- **Ask/detail injection** — memory brief injected into `ask_vault` + `chat_meeting` (parity with the loop), same gate; empty brief ⇒ byte-identical prompt.
- **`user_memory_enabled` flag** (default true) — OFF fully suppresses extraction + injection + the audit read.
- **/people CRM** — gated `list_people` reader (name, last-talked, open-commitment + fact counts) built entirely from existing visibility-gated readers; People FE view + route.

### PR #E — `feat/voiceprint` (E) — stacked on #154
- **On-device cross-meeting speaker voiceprint identity** (the analysis's #1 "planned-for-later" killer feature; feasibility proven in `2026-07-03-voiceprint-feasibility-spike.md` — sherpa-onnx's standalone `SpeakerEmbeddingExtractor`, **no new dep**).
- Per-cluster CAM++ embedding computed from the system stream at diarization → additive `speaker_voiceprints` table; gated `list_voiceprints_visible`; **purge-on-seal** in the seal tx + at-rest reconcile + FK cascade (a voiceprint is a biometric of a **sealed** meeting → dropped on seal → re-identification only works across **visible** meetings; privacy-correct). Pure cosine matcher + **enroll-on-`rename_speaker`**; raw embedding **never crosses IPC / never egresses**. Default **OFF**, AND-gated with `diarize_others`. FE: "Looks like [[Anna]]?" suggestion chip + enable toggle + manage list.

## Verification (each increment)
adversarial-verifier **PASS** + lock-security-reviewer **PASS** + full `scripts/ci.sh` **GREEN** (clippy `-D warnings` + `cargo test --lib` [771→804] + cargo audit + cargo deny + build + ng lint + ng build + headless E2E incl. mic+system mixing). RED-before-GREEN independently reproduced for every lock invariant (sealed-source exclusion from audit/brief/Ask/people/voiceprint-match; purge-on-seal). Lesson banked: the builders' `cargo test --lib` loop skips clippy — `cargo clippy --lib` before every PR (caught 3 `doc_lazy_continuation` errors in #153).

## Honest @Mac / counsel gates (NOT verifiable headless — flagged, not hidden)
- **Semantic default-on decision** — needs the bake-off RUN on a real Polish+English vault (the harness is shipped; the run is a user task). Fix-the-embedder-then-measure; e5-small stays default until measured.
- **Retrieval quality** of mmlw-e5-small on real PL data; **AEC efficacy**; **diarization DER**; **memory usefulness** (headless stub reasoner → extraction returns empty by design).
- **Voiceprint** — re-identification accuracy / cluster purity / the 0.5 cosine threshold are a real-Mac bake-off; and capturing a non-consenting remote participant's voice biometric (even on-device, encrypted, never-egressed) is **untested under BIPA/CIPA** → ships opt-in, default-OFF, with an in-code + UI note; get counsel before any default-on.
- Touch-ID / lock-at-rest / screen-share auto-relock — only truly verify on a signed build (unchanged by this program).

## Explicitly deferred to a next phase (documented, not built — to keep the loop bounded)
- **⌘K "talk to your brain" command bar + de-sprawl** (merge `/graph` into `/brain`, unify the 3 chat surfaces) — the highest perceived-magic UX lever (K3); a sizeable new-window + hotkey build, its own increment.
- **Provenance-linked notes** ("Anna said this, at 12:04") — synergizes with voiceprint; a summary-attribution feature.
- **Proactive post-meeting fact-deltas** (spec P3) — builds on the shipped in-meeting proactive recall.
- **Semantic default-on + first-run model download + backfill** — gated on the bake-off outcome above.
- **`Memory.md` vault export** (user-memory spec P4) — a cross-meeting artifact whose sources can be individually sealed; needs a lock/product decision.

## Coordination note
PR #153 heavily edits `embed.rs`; a **concurrent `embed` rework** was in flight in another session — rebase/coordinate `embed.rs` at merge. GitHub branch protection was paused at the time, so the local green `ci.sh` was the authority.
