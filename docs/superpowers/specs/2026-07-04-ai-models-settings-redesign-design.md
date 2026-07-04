<!-- Design brainstormed 2026-07-04 via /brainstorming. Follow-up FE redesign after the Murmur Brain merge (#184). -->
# Design: AI & Models settings — posture-first redesign

## Problem

The Murmur-Brain work (#184) added a **Murmur Brain Posture** chooser (Cloud / Hybrid / Fully local)
*on top of* the pre-existing controls instead of reconciling them. The `AI & Models` section now stacks
3+ overlapping ways to configure the same thing, concentrated in one 963-line component
(`ai-defaults-block.component.ts`). User-reported symptoms:

- Pick **Fully local** and the **Default AI: Claude** picker still stares back — "I clicked local but see Claude."
- No clear answer to *which model runs with what, when, and when to download a local one.*
- A separate "Enable Murmur Brain Live" card duplicates what picking Hybrid already means.
- The flat list of 6 local models is always visible, regardless of relevance.

**Goal:** one clear mental model — the *posture* is the primary control; provider/model/per-feature
machinery is contextual or collapsed; on-device model download surfaces contextually.

## The one mental model

The only question a normal user answers: **"Where does Murmur's AI run?"** — that is the posture.
Everything else is a *consequence*, not a separate decision.

- **Cloud** — your Default AI (Claude Code) does everything.
- **Hybrid ⭐** — Claude writes notes; your Mac runs realtime reactions + keeps fact-extraction on-device (needs one small local model).
- **Fully local** — your Mac does everything; nothing leaves (needs a notes model + a reactions model).

## Information architecture (top → bottom)

| # | Block | Who touches it |
|---|-------|----------------|
| 1 | **What Murmur uses** — 3 posture cards + a live "right now" state line + auto-download progress when a posture needs models | ~90% of users, only this |
| 2 | **⚙ Advanced** (collapsed) — *Connections* (Ollama/Claude Code/Anthropic/Kong setup) + *Default AI / model* + *Per-feature override* (Notes/Ask/Live) + *Local models* (the full 6-model registry) | power users only |
| 3 | **During meetings** — voice assistant + proactive hints toggles | unchanged |
| 4 | **Always on this Mac** — transcription / embeddings / name-redaction / semantic search | unchanged |
| 5 | **Where your text goes** — the egress receipt | unchanged |

The core fix: the **Default AI picker leaves the default view**. When you pick Fully local you never see
a cloud provider picker; the posture card itself states what runs where.

## Block 1 — "What Murmur uses" (the default view)

Three selectable posture cards (one active) + a **contextual state area beneath them** that changes per
posture and always tells the truth about what runs where.

**Cloud (selected):**
```
Right now: Claude Code writes everything — notes, answers, briefs.
Only transcription runs on this Mac.
```

**Hybrid — light model present:**
```
Right now: Claude writes notes; your Mac runs realtime reactions
and keeps fact-extraction on-device — Bielik 1.5B ✓
```
**Hybrid — light model absent → auto-download:**
```
Hybrid — downloading the on-device reactions model…
Bielik 1.5B  ▰▰▰▰▱▱▱▱  620 MB / 1.0 GB     [ Cancel ]
Staying on Cloud until it's ready.
```

**Fully local — models present:**
```
Right now: everything runs on this Mac. Nothing leaves.
Notes & Ask: Qwen3 4B ✓   ·   Reactions: Bielik 1.5B ✓
@brain answers run on-device (private, a little slower live).
```
**Fully local — models absent → auto-download:**
```
Fully local — downloading on-device models…
Notes model  Qwen3 4B     ▰▰▱▱▱▱  0.8 / 2.3 GB   [ Cancel ]
Reactions    Bielik 1.5B  ✓ ready
Staying on your current setup until both are ready.
```

**Three rules of this block:**

1. **Model auto-picked** — the **smallest model of the needed class that fits the Mac's RAM**
   (family-agnostic: today that is Bielik 1.5B for the light/reactions class and Qwen3 4B for the
   heavy/notes class). Shown by name (not a mystery), and overridable in Advanced — e.g. a Polish user can
   switch the heavy to the Bielik family there for Polish-native note quality. No language auto-detection
   in the default path (the app UI is English; keying on locale is a deferred nicety, not a requirement).
2. **RAM guard (non-blocking)** — if even the smallest needed model does not fit RAM, the card shows a
   soft warning ("Your Mac has 8 GB — Fully local needs ~⩾8 GB; may be slow") but still lets the user
   proceed; it warns, it never hard-blocks. Backed by the existing `brain_live_ram_ok` command.
3. **The standalone "Enable Murmur Brain Live" card is removed** — picking Hybrid/Fully local *is* the
   enablement. One mechanism, not two.

**Auto-download flow (the chosen behavior):** selecting a posture that needs an absent model starts the
download **immediately** with a progress bar + **Cancel**. The posture **activates only when the
download completes**; until then Murmur stays on the previous working posture. On failure or cancel, it
reverts to the prior posture and shows a recoverable error — the user is never stranded in a broken,
half-local state, and no multi-GB download is a silent surprise (size + progress are always on screen).

## Block 2 — ⚙ Advanced (collapsed by default)

One expander row: **"⚙ Advanced — connections, models, per-feature."** Inside:

1. **Connections** (the old "Providers") — Ollama / Claude Code / Anthropic (BYO key) / Kong Gateway, with
   status + setup. Rarely touched (Claude Code is ready by default).
2. **Default AI + Default model** — which cloud provider + model powers "everything Murmur writes."
   Relevant only for Cloud/Hybrid; greyed with "not used — Fully local" under the Fully-local posture.
3. **Per-feature override** (the old `ai-role-rows`: Notes / Ask / Live) — the escape hatch that produces
   the **Custom** posture.
4. **Local models** — the full 6-model registry (download / select / in-use). Only here; the posture
   block's auto-pick chooses from it, and Advanced lets you override which light/heavy.

**Invariant preserved:** Advanced is the *source* of the **Custom** posture. Any hand-edit (a per-feature
role, or a non-default model) makes `derive_posture` render **Custom** — honest, never a lying "Fully
local" over an egressing role (the exact deep-review invariant). Opening Advanced inside a posture shows
controls consistent with it (Fully local → rows all `local`); editing them → Custom. This relies on the
already-shipped `syncPostureFormFromBackend` form-resync.

## Block 3-5 — unchanged

**During meetings** (voice assistant + proactive hints), **Always on this Mac** (transcription /
embeddings / redaction / semantic search), and **Where your text goes** (egress receipt) keep their
current behavior and move below Advanced. The egress strip already reflects the posture.

## Component decomposition

`ai-defaults-block.component.ts` (963 L — doing too much) is split so each unit has one purpose,
is independently testable, and none exceeds ~350 L:

| Component | Single purpose | Status |
|-----------|----------------|--------|
| `brain-posture-block.component.ts` | posture cards + "right now" state + auto-download (Block 1) | **new** |
| `ai-advanced-block.component.ts` | the collapsed Advanced expander wrapping Connections + Default AI/model + per-feature + local-models (Block 2) | **new** |
| `ai-connection-cards` / `ai-connection-card` | Connections | existing, moved under Advanced |
| `ai-role-rows` | per-feature override | existing, moved under Advanced |
| `local-models-list.component.ts` | the 6-model registry list (download/select/in-use) | **new** (extracted from the 963 L block) |
| `ai-privacy-strip` | egress receipt | existing, stays at bottom |

The old `ai-defaults-block` is deleted once its parts are extracted.

## Backend

Minimal — the seam already exists: `brain_posture` / `set_brain_posture` / `brain_live_ram_ok` /
model download+select / `list_models` all shipped in #184. Likely needed:

- a small read the FE uses to render "what the current posture needs + each model's download state"
  (may be derivable FE-side from config + `list_models`; add a thin command only if not);
- a **download-progress event** for the auto-download bar (confirm during planning whether the existing
  model-download path already emits progress; if not, add one typed event).

No new lock/crypto/at-rest surface → a lock-security review is *not* expected. But posture egress-label
correctness (`derive_posture`) is privacy-adjacent, so the **adversarial-verifier owns the verdict**.

## Verification

- `npx ng lint` + `npx ng build` green (16 kB per-component style budget respected by the split).
- Live-repro on `:1420` with a mocked Tauri `invoke`: switch each posture (Cloud/Hybrid/Fully local),
  the auto-download progress + Cancel + revert-on-failure, Advanced → hand-edit → Custom, the RAM-guard
  warning, and the egress strip tracking the posture.
- The zoneless traps actively checked: NG0600 on the posture/download effects, no import cycle across the
  new split components, opaque overlays for any menu/popover, no `setTimeout` in components.
- Adversarial-verifier owns PASS/FAIL. Implementation ships as a **separate follow-up PR** (branch
  `feat/ai-models-redesign`; #184 already merged).

## Out of scope (honest)

- Registry model **URLs / sha256 pins** remain unverified (a separate Murmur-Brain follow-up); this
  redesign does not make Fully-local a *default* — it stays user-chosen until the pins land.
- No new local model families or engines; the auto-pick chooses among the existing registry entries.
- Real-Mac verification of the actual download + on-device inference latency stays the user's step on a
  signed build (headless proves the wiring/UX, not the GB download or Metal latency).
