# AI & Models — posture-driven redesign

**Date:** 2026-07-05
**Status:** design approved (clickable prototype signed off) → implementation
**Prototype:** `docs/superpowers/prototypes/2026-07-05-ai-models-redesign.html`
**Scope:** Angular frontend only — `src/app/features/settings/sections/ai/*`. **Backend untouched.**

## Problem (user report, with screenshots)

The current AI & Models settings screen is "still heavy to understand". Three concrete failures:

1. **"Change" button does nothing.** `AiResolvedMapComponent.change()` only calls
   `store.expandAdvanced()`. The Advanced disclosure expands far below the fold with no
   scroll and no targeting, so from the user's seat nothing happens.
2. **"Pick Cloud, but Murmur Brain is suddenly local + some models."** The posture is a
   preset over *roles*; 4 of the 7 jobs (Transcription, Realtime reactions, Search index,
   Name redaction) are **always on-device regardless of posture**. The flat "What runs where"
   list renders these next to the cloud rows with no grouping, so on-device rows read as a
   contradiction ("I picked Cloud — why is it local?"). Separately, the Advanced → Engines
   list shows **all 5 engines + the full local-model download aparat regardless of posture**,
   so even in Cloud the user faces the entire local machinery.
3. **No organization by posture.** The user's ask: Cloud → cloud models, Hybrid → hybrid,
   Local → local models. Today everything is shown at once.

## Ground truth of the engine (do not "fix" the backend — it is correct)

- **Posture is DERIVED, never stored** (`settings/postures.rs::derive_posture`). Picking a
  posture writes several config keys (`apply_posture`): Cloud = `brain_live=false`, clear all
  `role_*`; Hybrid = `brain_live=true`, clear `role_*`; Fully local = `brain_live=true`,
  `role_notes/ask=local+heavy`, `role_live=local+light`.
- **7 jobs** (`settings/ai_map.rs::ai_map_rows`): 3 **routable** (Notes, Ask, Live) + 4 fixed
  on-device (Realtime reactions [gated by `brain_live`], Transcription [Whisper], Search index
  [embedder, gated by `semantic_search_enabled`], Name redaction [NER]).
- **Engines:** on-device = built-in models ("Murmur Brain") + Ollama; cloud (redacted first) =
  Claude Code, Anthropic API, Kong AI Gateway. Local model registry (`reason::BRAIN_MODELS`):
  light (Qwen3 1.7B default, Bielik 1.5B) + heavy (Qwen3 4B default, Bielik 4.5B/11B, Qwen3 14B).

## Target structure — 4 sections (replaces today's flat pile)

Posture chooses the lane; the detail area shows **only what that lane needs to configure.**

1. **Where your AI runs** — the posture hero (Cloud / Hybrid ⭐ / Fully local) + a one-line
   plain-language meaning of the selected posture. Keeps the existing confirm-before-download +
   progress flow. *(evolves `brain-posture-block.component`)*
2. **Your setup** — NEW adaptive block, renders by `posture()`:
   - **Cloud** → one "Your cloud engine" card: engine select (`providerId`: Claude Code /
     Anthropic / Kong) + model + Test + "Cloud — redacted first".
   - **Hybrid** → cloud engine card **+** "On-device model — realtime reactions" (light-model
     picker + download state).
   - **Fully local** → "Your on-device models": heavy picker (Notes & Ask) + light picker
     (Live & realtime) + download + RAM warning. No cloud engine shown.
3. **What runs where** — the same honest resolver mirror, but **grouped into two blocks**:
   *☁ Goes to the cloud — redacted first* and *🖥 Stays on your Mac — always private*. Fully
   local shows "✓ Nothing leaves this Mac" in the cloud group. **"Change" on a routable row
   opens Advanced AND scrolls to + highlights that role's override row.** *(modifies
   `ai-resolved-map.component`)*
4. **▸ Advanced** — collapsed power path: **per-feature overrides** (Notes/Ask/Live) +
   **all engines** (connection cards). The **Default engine** select moves OUT of Advanced up
   into "Your setup" (§2); Advanced no longer duplicates it. *(evolves `ai-advanced-block`)*

`during-meetings-block`, `on-device-intelligence-block`, `ai-privacy-strip` stay as-is below.

## Component plan

| Component | Change |
| --- | --- |
| `settings-ai-section.component` | Reorder/compose: posture → **new setup block** → map → advanced → (unchanged tail). |
| `brain-posture-block.component` | Heading "What Murmur uses" → **"Where your AI runs"**; add clearer per-posture meaning line; keep download/confirm flow. |
| **`ai-setup-block.component` (NEW)** | Adaptive per-posture setup (§2). Cloud/Hybrid reuse the `providerId`/`providerModel` FormControls (same controls as old Advanced Default engine — one control, one source of truth). Local/Hybrid model pickers reuse the existing local-models logic (`local-models-list` / brain model store signals). |
| `ai-resolved-map.component` | Group rows cloud vs on-device (§3); `change(job)` sets a store "highlight role" signal + expands Advanced + scroll-into-view of the role row. |
| `ai-advanced-block.component` | Remove the Default-engine block (moved to setup); keep connection cards + role rows; accept the highlight target so the map's Change lands on the right row. |
| `settings.store.ts` | Add `highlightRole` signal (or reuse `advancedExpanded` + a scroll target) for the Change→scroll+flash; expose whatever the setup block needs (light/heavy model catalogs, download actions) that today lives in the brain-engine/local-models path. |

## Naming (approved — simplify)

- "Murmur Brain" (as an engine) → **"On this Mac — built-in models"**.
- posture heading → **"Where your AI runs"**.
- keep "Cloud — redacted first", posture labels Cloud / Hybrid ⭐ / Fully local.

## Constraints & Definition of Done

- **Backend untouched.** No new config keys required (setup writes the *existing* keys:
  `providerId`, `providerModel`, `brain_light_model_id`, `brain_heavy_model_id`, and the
  posture presets already do the rest). If a store convenience signal is missing it is added
  FE-side over existing IPC.
- **Zoneless/signals rules** (angular-zoneless.md): standalone, OnPush, signals, `@if`/`@for`,
  `afterNextRender` for the scroll (with `{ injector }`), opaque overlays only where floating,
  no `setTimeout` in a component, no new deps.
- **No lock/crypto/visibility path touched** → lock-security review not required; the map reads
  the already-gated `resolved_ai_map` (engine names, not content). **adversarial-verifier owns
  the PASS/FAIL** (live-repro against `:1420` with mocked invoke; NG0600 / import-cycle / opacity
  hunts).
- Gates green: `npx ng lint`, `npx ng build`, `cargo test --lib` (unchanged, sanity), then
  `scripts/ci.sh` once at the end.
- Ship: QueaT commit, PR to `murmur` (never direct push), no Claude trailers.
