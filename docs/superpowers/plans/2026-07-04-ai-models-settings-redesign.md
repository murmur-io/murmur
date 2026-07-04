# Posture-first AI & Models Settings — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the *posture* (Cloud / Hybrid / Fully local) the single primary control of Settings → AI & Models; move provider/model/per-feature into a collapsed ⚙ Advanced; auto-download the on-device model a posture needs (progress + Cancel, activate-on-complete); and decompose the 963-line `ai-defaults-block` into focused components.

**Architecture:** Angular 18 **zoneless** standalone/signals FE only. All state lives in the shell-provided `SettingsStore` (`src/app/features/settings/settings.store.ts`); components are thin signal-consumers. The backend seam already shipped in #184 — no new Rust commands are required (`set_brain_posture`, `brain_posture`, `brain_live_ram_ok`, `download_brain_model`, `select_brain_model`, `list_models`, and the `EVENT_BRAIN_DOWNLOAD` progress stream all exist). Work is: (1) grow the store's posture logic, (2) split the mega-component, (3) rewire the section.

**Tech Stack:** Angular 18.2 zoneless (signals/computed/effect, standalone, OnPush, `@if`/`@for`), `IpcService` over Tauri `invoke`/`listen`, inline template+styles, `var(--token)` CSS. Verified with `npx ng lint` + `npx ng build` + Playwright live-repro on `:1420` against a mocked `window.__TAURI_INTERNALS__.invoke`.

## Global Constraints

- **Zoneless (binding):** `standalone: true` + `ChangeDetectionStrategy.OnPush`; state is `signal()`/`computed()`; `inject()` (no constructor DI); `input()`/`output()`/`viewChild()` (never the decorators); `@if`/`@for` only (`*ngIf`/`*ngFor` BANNED); `@for` tracks a stable id; inline `template` + `styles:[\`…\`]`; **≤16 kB per-component style budget**; `var(--token)` for every color/space/radius/shadow (no hex/px design values); inline SVG icons; **no new npm packages**.
- **NG0600:** any `effect()` that writes a signal it also reads needs `{ allowSignalWrites: true }`.
- **Overlays:** any floating popover/menu uses `var(--surface-overlay)` + `backdrop-filter:none` (never the frosted `.card`). The Advanced expander is IN-FLOW, so a normal surface is fine — no overlay needed.
- **IPC (binding):** one typed method per Tauri command in `ipc.service.ts`; NEVER `invoke(...)` from a component; every IPC result lands in a `signal` (no `.subscribe`-into-field, no `async` pipe). All commands this plan needs already exist on `IpcService`.
- **Privacy invariant:** `derive_posture` must never mislabel egress — a hand-edited role (in Advanced) renders the posture chip **Custom**. Rely on the existing `syncPostureFormFromBackend()` so the form never clobbers a posture on the next save.
- **Identity:** commits authored **QueaT `<kgm004a@gmail.com>`**, no Claude trailers; conventional-commit style; ships as a PR to `murmur` (never a direct push). `gh` account `JakubGawr`.
- **Verdict:** the implementer never self-certifies — the **adversarial-verifier** owns PASS/FAIL after the gates.

---

## File structure

**Target `settings-ai-section` composition** (replaces the current 3-child layout):
```
<app-brain-posture-block />          NEW — the default view (posture + right-now + auto-download)
<app-ai-advanced-block />            NEW — collapsed: connections + default-AI/model + per-feature + local-models
<app-during-meetings-block />        NEW — extracted: voice assistant + proactive hints (+ consent warning)
<app-on-device-intelligence-block /> NEW — extracted: always-on badges + semantic search/reindex
<app-ai-privacy-strip />             UNCHANGED — egress receipt
```

| File | Responsibility | Action |
|------|----------------|--------|
| `settings.store.ts` | posture-driven state: auto-pick, needed-models, "right now" line, auto-download-on-select | **modify** |
| `sections/ai/brain-posture-block.component.ts` | 3 posture cards + contextual state area + auto-download progress/Cancel; the retirement nudge | **create** |
| `sections/ai/local-models-list.component.ts` | the 6-model registry list (download/select/in-use), extracted from `ai-role-rows` | **create** |
| `sections/ai/ai-advanced-block.component.ts` | collapsed expander wrapping connections + Default-AI/model + per-feature + local-models | **create** |
| `sections/ai/during-meetings-block.component.ts` | voice-assistant + proactive-hints toggles + the cloud-consent warning | **create** |
| `sections/ai/on-device-intelligence-block.component.ts` | always-on badges + semantic search + reindex | **create** |
| `sections/settings-ai-section.component.ts` | compose the new blocks in order | **modify** |
| `sections/ai/ai-defaults-block.component.ts` | the 963-line mishmash | **delete** (after its parts are extracted) |
| `sections/ai/ai-connection-cards.component.ts` | Connections | unchanged, rendered inside `ai-advanced-block` |
| `sections/ai/ai-role-rows.component.ts` | per-feature rows; loses the GGUF-registry list (→ `local-models-list`) | **modify** |

**Testing note (Murmur reality):** this repo's FE gate is `ng lint` + `ng build` + a **Playwright live-repro** against `:1420` with a mocked Tauri `invoke` (the `scripts/screenshots/mock-tauri.js` pattern) — there is no component unit-test runner. Each task's test cycle is therefore a Playwright spec that boots the settings page with a scripted `invoke` mock, asserts the behavior RED-before-GREEN, then GREEN after implementation. Put specs under `e2e/settings-ai/` (create the folder). Where a task adds a **pure function** (the auto-pick), test it directly inside the same spec via `page.evaluate` importing nothing — assert through the rendered UI it drives.

---

## Task 1: Store — posture-driven state (auto-pick, needed-models, "right now" line, auto-download-on-select)

**Files:**
- Modify: `src/app/features/settings/settings.store.ts`
- Test: `e2e/settings-ai/posture-auto-download.spec.ts`

**Interfaces:**
- Consumes (already in the store/ipc/models): `Posture`, `BrainModelDto` (`{ id, name, class, approxSizeBytes, downloaded, selected, fitsRam, … }` — confirm exact field names in `models.ts:269` before use), `this.ipc.listModels`, `this.ipc.setBrainPosture`, `this.ipc.downloadBrainModel(id)`, `this.ipc.selectBrainModel(id)`, `this.ipc.brainPosture`, `this.ipc.brainLiveRamOk`, existing `_brainDownloadingId`/`_brainDownloadFrac` + `onBrainDownload` wiring, `refreshPosture()`, `syncPostureFormFromBackend()`.
- Produces (later tasks rely on these exact names):
  - `readonly brainModels: Signal<BrainModelDto[]>` — the registry (add if not already exposed; `list_models`-backed).
  - `autoPickForClass(cls: ModelClass): BrainModelDto | null` — pure over `brainModels()`.
  - `readonly neededModels: Signal<{ role: "notes" | "reactions"; model: BrainModelDto | null }[]>` — what the SELECTED posture needs.
  - `readonly postureStateLine: Signal<string>` — the "right now" sentence for the active posture.
  - `readonly pendingPosture: Signal<Posture | null>` — the posture mid-download (drives the target-card progress).
  - `setPosture(p: Posture): Promise<void>` — auto-downloads absent needed models first, commits on success, reverts on fail/cancel.
  - `cancelPostureDownload(): void`.

- [ ] **Step 1: Write the failing Playwright spec (RED)**

`e2e/settings-ai/posture-auto-download.spec.ts` — boot settings with a mock `invoke` where `list_models` returns a light model `downloaded:true` and a heavy `downloaded:false`, `brain_posture` returns `"cloud"`. Assert the (not-yet-existing) behavior:

```ts
import { test, expect } from "@playwright/test";
import { mockTauri } from "./mock-invoke"; // thin wrapper over scripts/screenshots/mock-tauri.js

test("picking Fully local auto-downloads the absent heavy model then commits", async ({ page }) => {
  const calls: string[] = [];
  await mockTauri(page, {
    list_models: () => [
      { id: "bielik-1.5b", name: "Bielik 1.5B", class: "light", approxSizeBytes: 1_050_000_000, downloaded: true, selected: false, fitsRam: true },
      { id: "qwen3-4b",   name: "Qwen3 4B",    class: "heavy", approxSizeBytes: 2_300_000_000, downloaded: false, selected: false, fitsRam: true },
    ],
    brain_posture: () => "cloud",
    brain_live_ram_ok: () => true,
    download_brain_model: (a: any) => { calls.push("download:" + a.id); return null; },
    select_brain_model: (a: any) => { calls.push("select:" + a.id); return null; },
    set_brain_posture: (a: any) => { calls.push("posture:" + a.posture); return null; },
  });
  await page.goto("http://localhost:1420/#/settings");
  await page.getByRole("button", { name: /Fully local/ }).click();
  // downloads the absent heavy BEFORE committing the posture, then selects + commits:
  await expect.poll(() => calls).toEqual([
    "download:qwen3-4b", "select:qwen3-4b", "posture:fully_local",
  ]);
});
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `npx playwright test e2e/settings-ai/posture-auto-download.spec.ts` (dev app running on `:1420`).
Expected: FAIL — either the button drives the OLD flow (calls `set_brain_posture` before downloading) or `mock-invoke` helper missing.

- [ ] **Step 3: Add the auto-pick pure helper + registry signal**

In `settings.store.ts`, expose the registry and a family-agnostic smallest-fits-RAM picker:

```ts
readonly brainModels = this._brainModels.asReadonly(); // populated from ipc.listModels() where the store already loads models

/** Smallest model of `cls` that fits RAM (family-agnostic). Prefers an already-downloaded one. */
autoPickForClass(cls: ModelClass): BrainModelDto | null {
  const of = this.brainModels().filter((m) => m.class === cls && m.fitsRam);
  if (of.length === 0) return null;
  const downloaded = of.filter((m) => m.downloaded);
  const pool = downloaded.length ? downloaded : of;
  return pool.reduce((a, b) => (b.approxSizeBytes < a.approxSizeBytes ? b : a));
}
```

- [ ] **Step 4: Add `neededModels` + `postureStateLine` computeds**

```ts
/** What the ACTIVE posture needs on-device. Cloud → none; Hybrid → reactions(light); Fully local → notes(heavy)+reactions(light). */
readonly neededModels = computed(() => {
  const p = this.posture();
  const light = this.autoPickForClass("light");
  const heavy = this.autoPickForClass("heavy");
  if (p === "hybrid") return [{ role: "reactions" as const, model: light }];
  if (p === "fully_local")
    return [{ role: "notes" as const, model: heavy }, { role: "reactions" as const, model: light }];
  return [];
});

readonly postureStateLine = computed(() => {
  switch (this.posture()) {
    case "cloud": return "Claude Code writes everything — notes, answers, briefs. Only transcription runs on this Mac.";
    case "hybrid": return "Claude writes notes; your Mac runs realtime reactions and keeps fact-extraction on-device.";
    case "fully_local": return "Everything runs on this Mac. Nothing leaves. @brain answers run on-device (private, a little slower live).";
    default: return "Custom — some features run on-device, some in the cloud.";
  }
});
```

- [ ] **Step 5: Rewrite `setPosture` to auto-download-then-commit**

Replace the current `setPosture` body with: compute the needed-absent models for the *target* posture; if none, commit immediately (today's path); else download each (reusing `_brainDownloadingId`/`_brainDownloadFrac`), then `select_brain_model` each, then `set_brain_posture`, then sync+refresh. On any failure/cancel, clear pending + surface `postureError`, DO NOT commit (stay on the prior posture).

```ts
private readonly _pendingPosture = signal<Posture | null>(null);
readonly pendingPosture = this._pendingPosture.asReadonly();
private _cancelDownload = false;

async setPosture(p: Posture): Promise<void> {
  this._postureError.set(null);
  const needed = this.neededModelsFor(p);                    // pure: same logic as neededModels but for an arbitrary p
  const absent = needed.map((n) => n.model).filter((m): m is BrainModelDto => !!m && !m.downloaded);
  if (absent.length === 0) { await this.commitPosture(p); return; }
  this._pendingPosture.set(p); this._cancelDownload = false; this._postureBusy.set(true);
  try {
    for (const m of absent) {
      if (this._cancelDownload) throw new Error("cancelled");
      this._brainDownloadFrac.set(0); this._brainDownloadingId.set(m.id);
      try { await this.ipc.downloadBrainModel(m.id); } finally { this._brainDownloadingId.set(null); }
    }
    for (const n of needed) if (n.model) await this.ipc.selectBrainModel(n.model.id);
    await this.commitPosture(p);
  } catch (e) {
    this._postureError.set(this._cancelDownload ? null : String(e));  // cancel is silent; a real failure shows
    await this.refreshPosture();                                       // reflect the unchanged backend posture
  } finally {
    this._pendingPosture.set(null); this._postureBusy.set(false); this._cancelDownload = false;
    await this.reloadModels();                                         // refresh downloaded flags
  }
}

cancelPostureDownload(): void { this._cancelDownload = true; }

private async commitPosture(p: Posture): Promise<void> {
  await this.ipc.setBrainPosture(p);
  await this.syncPostureFormFromBackend();
  await this.refreshPosture();
  this._brainLiveRamOk.set(await this.ipc.brainLiveRamOk().catch(() => true));
}
```

*(Note for the implementer: `neededModelsFor(p)` factors the switch out of the `neededModels` computed so both share it; `reloadModels()` is the store's existing `list_models` loader — reuse it, don't duplicate. Confirm the exact names of the private model signal + loader in the store before wiring.)*

- [ ] **Step 6: Run the spec to verify GREEN**

Run: `npx playwright test e2e/settings-ai/posture-auto-download.spec.ts`
Expected: PASS — order is `download:qwen3-4b`, `select:qwen3-4b`, `posture:fully_local`.

- [ ] **Step 7: `ng lint` + `ng build`**

Run: `npx ng lint && npx ng build`
Expected: clean (only the pre-existing library/detail budget warnings).

- [ ] **Step 8: Commit**

```bash
git add src/app/features/settings/settings.store.ts e2e/settings-ai/
git commit --author="QueaT <kgm004a@gmail.com>" -m "feat(settings): posture-driven auto-download-then-commit + right-now state"
```

---

## Task 2: `brain-posture-block` component (default view)

**Files:**
- Create: `src/app/features/settings/sections/ai/brain-posture-block.component.ts`
- Test: `e2e/settings-ai/brain-posture-block.spec.ts`

**Interfaces:**
- Consumes from the store: `posture`, `postureBusy`, `postureError`, `pendingPosture`, `postureStateLine`, `neededModels`, `brainDownloadingId`, `brainDownloadFrac`, `brainPct`, `brainLiveRamOk`, `retirementNudge`, `applyingRetirement`, `setPosture(p)`, `cancelPostureDownload()`, `applyRetirementReplacement()`.
- Produces: the `<app-brain-posture-block>` selector consumed by `settings-ai-section` (Task 6).

- [ ] **Step 1: Write the failing spec (RED)** — `brain-posture-block.spec.ts`: with the mock from Task 1 (heavy absent), assert (a) three posture cards render, (b) NO element with text "Enable Murmur Brain Live" exists, (c) clicking Fully local shows a progress bar with text matching `/Qwen3 4B/` and a `Cancel` button, (d) the "right now" line for Cloud reads the `postureStateLine` cloud copy.

- [ ] **Step 2: Run → FAIL** (`Cannot find selector app-brain-posture-block`). Run: `npx playwright test e2e/settings-ai/brain-posture-block.spec.ts`.

- [ ] **Step 3: Create the component.** New standalone/OnPush component. **Move verbatim** the retirement banner (`ai-defaults-block.component.ts:38-59`) and the posture segment (`:66-120`) and their CSS (`.posture*`, `.retirement*`, `.brain-live-ram-warn`, `.brain-error` — `:573-703`). **Delete** the Brain-Live enablement card (`:122-189`) entirely. **Replace** the static field-help (`:114-117`) with the contextual state area:

```html
<div class="posture-state">
  @if (pendingPosture(); as pend) {
    <p class="text-secondary">{{ pendingLabel(pend) }} — downloading on-device models…</p>
    @for (n of neededModels(); track n.role) {
      @if (n.model && !n.model.downloaded) {
        <div class="semantic-progress" role="status">
          <div class="semantic-progress-track"><div class="semantic-progress-fill" [style.width.%]="brainDownloadFrac() * 100"></div></div>
          <span class="semantic-progress-label text-muted">{{ n.model.name }} · {{ brainPct() }}</span>
        </div>
      }
    }
    <button type="button" class="btn btn-ghost" (click)="cancelPostureDownload()">Cancel</button>
    <span class="field-help text-muted">Staying on your current setup until it's ready.</span>
  } @else {
    <p class="text-secondary posture-now">{{ postureStateLine() }}</p>
    @for (n of neededModels(); track n.role) {
      <span class="pill" [class.is-success]="n.model?.downloaded">
        <span class="pill-dot"></span>{{ n.role === "notes" ? "Notes & Ask" : "Reactions" }}: {{ n.model?.name ?? "—" }}{{ n.model?.downloaded ? " ✓" : "" }}
      </span>
    }
    @if (!brainLiveRamOk()) {
      <span class="brain-live-ram-warn">⚠ Your Mac may not have enough RAM to run this smoothly alongside recording.</span>
    }
  }
  @if (postureError(); as perr) { <p class="text-danger brain-error">{{ perr }}</p> }
</div>
```

Reuse the existing `.semantic-progress*` CSS (move from `:765-786`) and `.pill` global. Class-signal fields mirror `AiDefaultsBlockComponent:889-899` (posture group) — copy those `readonly x = this.store.x` lines; add `pendingPosture`, `postureStateLine`, `neededModels`. Add `pendingLabel(p: Posture)` returning "Hybrid"/"Fully local".

- [ ] **Step 4: Run → GREEN.** Run: `npx playwright test e2e/settings-ai/brain-posture-block.spec.ts`.

- [ ] **Step 5: `ng lint` + `ng build`** — clean, style budget respected.

- [ ] **Step 6: Commit**

```bash
git add src/app/features/settings/sections/ai/brain-posture-block.component.ts e2e/settings-ai/brain-posture-block.spec.ts
git commit --author="QueaT <kgm004a@gmail.com>" -m "feat(settings): brain-posture-block — posture cards + right-now state + auto-download"
```

---

## Task 3: `local-models-list` component (extracted)

**Files:**
- Create: `src/app/features/settings/sections/ai/local-models-list.component.ts`
- Modify: `src/app/features/settings/sections/ai/ai-role-rows.component.ts` (remove the GGUF-registry list block + its now-unused CSS; keep the per-role rows)
- Test: `e2e/settings-ai/local-models-list.spec.ts`

**Interfaces:**
- Consumes from the store: the registry signal `brainModels`, per-model `downloadBrainModel(id)`/`selectBrainModel(id)`, `brainDownloadingId`, `brainDownloadFrac`, `brainPct` (the same download signals Task 1 uses). Confirm the model-list markup + handlers currently in `ai-role-rows.component.ts` (grep `list_models` / model registry) and MOVE them verbatim.
- Produces: `<app-local-models-list>` used inside `ai-advanced-block` (Task 4).

- [ ] **Step 1: Failing spec (RED)** — assert `<app-local-models-list>` renders one row per `brainModels()` entry, an "In use" badge on the `selected` one, and a Download button on a `downloaded:false` one.
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Create the component** by moving the model-registry list markup + handlers + CSS out of `ai-role-rows.component.ts` into it (verbatim; adjust the store-field wiring). In `ai-role-rows.component.ts`, delete the moved block and its dead CSS; leave the Notes/Ask/Live rows intact.
- [ ] **Step 4: Run → GREEN.**
- [ ] **Step 5: `ng lint` + `ng build`** — clean.
- [ ] **Step 6: Commit** (`feat(settings): extract local-models-list from ai-role-rows`).

---

## Task 4: `ai-advanced-block` component (collapsed)

**Files:**
- Create: `src/app/features/settings/sections/ai/ai-advanced-block.component.ts`
- Test: `e2e/settings-ai/ai-advanced-block.spec.ts`

**Interfaces:**
- Consumes: `AiConnectionCardsComponent`, `AiRoleRowsComponent`, `LocalModelsListComponent` (imports), plus the store's Default-AI/model signals + handlers currently on `AiDefaultsBlockComponent` (`form`, `defaultModelCatalog`, `defaultModelsLoading`, `defaultModelIsCustom`, `onDefaultAiChanged`, `refreshDefaultModels`, `ensureModels`, `posture`).
- Produces: `<app-ai-advanced-block>` used by `settings-ai-section` (Task 6).

- [ ] **Step 1: Failing spec (RED)** — assert: Advanced is **collapsed by default** (the Default-AI select is not visible); clicking the "⚙ Advanced" toggle reveals Connections + Default AI + per-feature + local-models; when `posture()==="fully_local"` the Default-AI field shows a "not used — Fully local" note (greyed).
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Create the component.** A `expanded = signal(false)` drives an in-flow disclosure (a plain button toggling a `@if (expanded())` region — NOT an overlay). Inside, in order: `<app-ai-connection-cards />` (Connections), the Default-AI select + `brain-tuning` model/effort block (**move verbatim** from `ai-defaults-block.component.ts:192-300` + its CSS `:705-720`, `.brain-tuning` `:567-571`), `<app-ai-role-rows />`, `<app-local-models-list />`. When `posture()==="fully_local"`, render the Default-AI select disabled with a "not used — Fully local" `field-help`. Mirror the class-field wiring from `AiDefaultsBlockComponent:900-902` + the handlers `:944-954`.
- [ ] **Step 4: Run → GREEN.**
- [ ] **Step 5: `ng lint` + `ng build`** — clean, budget respected.
- [ ] **Step 6: Commit** (`feat(settings): ai-advanced-block — collapsed connections + default-AI + per-feature + models`).

---

## Task 5: `during-meetings-block` + `on-device-intelligence-block` (extracted)

**Files:**
- Create: `src/app/features/settings/sections/ai/during-meetings-block.component.ts`
- Create: `src/app/features/settings/sections/ai/on-device-intelligence-block.component.ts`
- Test: `e2e/settings-ai/live-and-ondevice.spec.ts`

**Interfaces:**
- `during-meetings-block` consumes: `form` (`realtimeReactions`, `proactiveHintsEnabled`), `liveTargetIsCloud`, `cloudConsented`, `consenting`, `consentError`, `allowCloudProcessing()`.
- `on-device-intelligence-block` consumes: `form` (`semanticSearchEnabled`), `embedModelPresent`, `downloadingEmbedModel`, `embedDownloadFrac`, `embedPct`, `embedDownloadError`, `reindexing`, `reindexFrac`, `reindexPct`, `reindexResult`, `reindexError`, `downloadEmbedModel()`, `reindexEmbeddings()`.
- Produces: `<app-during-meetings-block>` and `<app-on-device-intelligence-block>` for Task 6.

- [ ] **Step 1: Failing spec (RED)** — assert both selectors render, the assistant/proactive toggles bind to the form, the semantic-search toggle + "Re-index notes" button render, and the cloud-consent warning appears when `realtimeReactions` on + `liveTargetIsCloud` + not consented.
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Create both components** by **moving verbatim**: the "Live during meetings" group (`ai-defaults-block.component.ts:310-385`) → `during-meetings-block`; the "On-device intelligence" group (`:387-524`) → `on-device-intelligence-block`; and their CSS (`.use-group*`, `.toggle-*`, `.realtime-consent*`, `.cloud-consent*`, `.ondevice-*`, `.semantic*`, `.privacy-note`, `.spin-ring`). Each `[formGroup]="form"` wraps its own `.card`. Mirror the class-field wiring from `AiDefaultsBlockComponent:884-887, 903-912`.
- [ ] **Step 4: Run → GREEN.**
- [ ] **Step 5: `ng lint` + `ng build`** — clean, each component under the 16 kB budget.
- [ ] **Step 6: Commit** (`feat(settings): extract during-meetings + on-device-intelligence blocks`).

---

## Task 6: Rewire the section + delete `ai-defaults-block`

**Files:**
- Modify: `src/app/features/settings/sections/settings-ai-section.component.ts`
- Delete: `src/app/features/settings/sections/ai/ai-defaults-block.component.ts`
- Test: `e2e/settings-ai/section-order.spec.ts`

**Interfaces:**
- Consumes all five block components; produces the assembled section.

- [ ] **Step 1: Failing spec (RED)** — assert the AI & Models section renders, top-to-bottom: posture block → Advanced (collapsed) → During meetings → On-device intelligence → privacy strip; and that no "Enable Murmur Brain Live" text and no standalone always-visible "Default AI" select exist at load.

- [ ] **Step 2: Run → FAIL** (still the old `app-ai-defaults-block`).

- [ ] **Step 3: Rewire** `settings-ai-section.component.ts`:

```ts
imports: [
  BrainPostureBlockComponent,
  AiAdvancedBlockComponent,
  DuringMeetingsBlockComponent,
  OnDeviceIntelligenceBlockComponent,
  AiPrivacyStripComponent,
],
template: `
  <app-brain-posture-block />
  <app-ai-advanced-block />
  <app-during-meetings-block />
  <app-on-device-intelligence-block />
  <app-ai-privacy-strip />
`,
```

Then `git rm src/app/features/settings/sections/ai/ai-defaults-block.component.ts` and grep the repo for any remaining `AiDefaultsBlockComponent` / `app-ai-defaults-block` reference — there must be none.

- [ ] **Step 4: Run → GREEN.** Run: `npx playwright test e2e/settings-ai/section-order.spec.ts`.

- [ ] **Step 5: Full gate.**

Run: `npx ng lint && npx ng build`
Expected: clean; `ai-defaults-block` gone; no orphaned imports.

- [ ] **Step 6: Commit** (`refactor(settings): rewire AI & Models section, delete ai-defaults-block`).

---

## Task 7: Adversarial verification + PR

**Files:** none (verification + PR).

- [ ] **Step 1: Live-repro sweep** on `:1420` with the mocked `invoke`: switch Cloud↔Hybrid↔Fully local; confirm auto-download progress + Cancel (revert on cancel, no posture commit); confirm a present-models posture shows the "right now" line + green model pills; open Advanced, hand-edit the Ask role to a cloud provider, save, confirm the posture chip flips to **Custom** (the `derive_posture` never-lies invariant) and a page reload keeps it Custom (no form-clobber); confirm the egress strip tracks the posture.
- [ ] **Step 2: Zoneless-trap audit** — NG0600 (any effect writing a signal it reads has `allowSignalWrites`), no import cycle across the new components (`forwardRef` only if a pair is mutually recursive — none expected here), no `setTimeout`/`ResizeObserver` in a component, no `*ngIf`/`*ngFor`, `@for` tracks a stable id, ≤16 kB style budget per component.
- [ ] **Step 3: Dispatch the `adversarial-verifier` agent** over the full diff — it owns PASS/FAIL. A finding sends the offending task back. (No lock/crypto surface here, so no lock-security review; but the verifier confirms the egress-label invariant and the auto-download failure/cancel paths.)
- [ ] **Step 4: `bash scripts/ci.sh`** once, at the end — must end `✅ CI: all gates green`.
- [ ] **Step 5: Open the PR** to `murmur`:

```bash
git push -u origin feat/ai-models-redesign
gh pr create -R murmur-io/murmur --base murmur --head feat/ai-models-redesign \
  --title "refactor(settings): posture-first AI & Models redesign" \
  --body "<what + how verified; links the design spec>"
```

---

## Self-review (author checklist — done)

- **Spec coverage:** posture-primary (Task 2/6) · auto-download-on-select with progress+Cancel+activate-on-complete+revert (Task 1/2) · Advanced collapsed w/ Connections+Default-AI+per-feature+local-models (Task 3/4) · Enable-Brain-Live card removed (Task 2) · auto-pick smallest-fits-RAM (Task 1) · RAM guard non-blocking warning (Task 2) · derive_posture→Custom on hand-edit (Task 7 verify, relies on shipped `syncPostureFormFromBackend`) · During-meetings + On-device blocks preserved (Task 5) · egress strip unchanged (Task 6) · 963-line block decomposed + deleted (Tasks 2-6). All covered.
- **Placeholders:** the NEW behavior (auto-pick, `neededModels`, `postureStateLine`, `setPosture` rewrite, the state-area template) is given as real code; RELOCATED markup/CSS is cited by exact source line-range to move verbatim (a faithful refactor, not a placeholder). The one explicit "confirm exact field/loader name in the store/models before wiring" notes are real pre-flight checks, not deferred work.
- **Type consistency:** `Posture`, `ModelClass`, `BrainModelDto` used consistently; `autoPickForClass`/`neededModels`/`postureStateLine`/`pendingPosture`/`setPosture`/`cancelPostureDownload`/`commitPosture` names match across Tasks 1-2; `brainModels` registry name reused in Tasks 1/3/4.

## Out of scope (from the spec)

Registry model URLs/sha256 pins stay unverified (separate Murmur-Brain follow-up; Fully-local stays user-chosen, not a default). No new model families/engines. Real-Mac verification of the actual GB download + on-device inference latency is the user's step on a signed build — headless proves the wiring/UX only.
