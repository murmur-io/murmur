# Learnings — angular-zoneless-dev

## Recurring patterns
<!-- Curated, binding. Prepended to every dispatch. Keep ≤ ~20 bullets. -->

- **NG0600 (T1):** an `effect()` that writes a signal it might also read throws. Prefer a
  `computed()`; only when the effect genuinely orchestrates an async IPC fetch, pass
  `{ allowSignalWrites: true }`. Mirrors: `entity-detail.component.ts`, `graph.component.ts`.
- **Import-cycle `ɵcmp` (T2):** mutually-recursive standalone components (tree ↔ row) must use
  `forwardRef(() => Other)` in BOTH `imports:` arrays, or the first `@for` throws
  `getComponentDef(undefined)`.
- **Opaque overlays (T3):** anything floating OVER content (menu, popover, modal, dropdown) uses
  `var(--surface-overlay)` + `backdrop-filter: none`, NOT the frosted `.card` (content bleeds
  through). Rationale: `move-to-menu.component.ts`.
- **CSP style-src nonce (T4):** never add a nonce/hash to `style-src`; keep
  `dangerousDisableAssetCspModification: ["style-src"]` in `tauri.conf.json`. A nonce makes WKWebView
  drop every Angular runtime `<style>` → prod renders unstyled while `ng serve` is fine. A green
  `ng build` proves nothing here — verify in the real packaged WKWebView.
- **State is signals, not fields.** Zoneless: CD runs only on signal change / template event. A
  stale view almost always = state in a plain field that should be `signal()`/`computed()`.
- **IPC lands in a signal.** `await ipc.x()` inside an `effect()` (with a stale-result guard) or
  `toSignal()` — never `.subscribe()`/`.then()` into a field, never `async` pipe. One typed
  `IpcService` method per Tauri command; never `invoke(...)` from a component.
- **Templates:** `@if`/`@for`/`@switch`/`@let` only; `@for` must `track item.id` (never `$index`
  for keyed data). `input()`/`output()`/`viewChild()`, not the decorators. `inject()`, not ctor.
- **Side effects:** `afterNextRender(fn, { injector })` for focus/scroll/measure — never
  `setTimeout`/`rAF` in a component. Observers live in a directive with `DestroyRef.onDestroy`.
- **No new npm packages** without explicit user approval.
- **Verify live**, not just by build: drive `:1420` with a mocked
  `window.__TAURI_INTERNALS__.invoke`; Playwright defaults colorScheme LIGHT — eyeball the PNG,
  don't trust a shallow shell screenshot.

## Run journal
<!-- Append-only, newest first. -->

### [2026-07-05 detail redesign — #194] a botched multi-agent component split
- **Pattern:** Splitting a 4600-line `detail.component` into panels across workflow phases left it
  NON-BUILDING: the Split-phase agent DIED mid-response ("API Error: Connection closed") so the panel
  components were created-but-UNWIRED (imported but never rendered), the old inline markup stayed, and
  stale `<app-move-to-menu>` / `<app-share-verify-sheet>` refs remained after their imports were
  stripped → NG8001. Separately, a panel had a HARD syntax error: BACKTICKS inside an HTML comment
  (`<!-- `createdUrl` -->`) INSIDE an inline `template: \`…\`` terminated the template literal early →
  a cascade of phantom "@Component argument" errors that made the panel look half-built.
- **Caught by:** operator (running `ng build` + grepping the actual template) — the workflow's own
  verify phase had run on the broken tree and still reported PASS.
- **Lesson:** After a big split/refactor, RUN `ng build` yourself and GREP the result: is every new
  panel actually rendered (`<app-x-panel>` present in the parent template)? are stale component refs +
  their `imports:` entries gone? A "done" report from an agent describes intent, not a building tree.
  Never put backticks inside an inline `template: \`…\`` — even inside an HTML `<!-- … -->` comment they
  silently end the literal. When a workflow phase agent dies mid-response, expect a half-applied tree.
- **Status:** journal

### [2026-07-04 PR#181 Murmur Brain] Preset command + reactive form dual-write → the stale form CLOBBERS the preset on next save (CRITICAL)
- **Pattern:** a "posture" preset was applied via a Tauri command that wrote the `role_*` / `brain_backend`
  DB config keys directly. But the Settings page's reactive form still held the STALE key values from its
  one-time `load()`, and the ordinary `save()` re-serialized `form.getRawValue()` verbatim → the next
  unrelated Save overwrote the preset's keys back to `""` → a Fully-Local (zero-egress) posture silently
  reverted to cloud egress. TWO writers of the same keys (a command + the form) that never reconcile.
  Fix: after the preset command succeeds, re-fetch fresh config and `patchValue` ONLY the preset-owned
  controls (mirror `load()`'s config→form mapping) so the form no longer clobbers; and refresh the derived
  label (`refreshPosture()`) after `save()` + per-role edits so the shown posture never lies. Also: a
  readiness `computed()` that checks "ANY model downloaded" is a FALSE POSITIVE when the backend resolves a
  SPECIFIC selected/default model — mirror the backend's exact resolution.
- **Caught by:** deep-review Workflow (wiring/FE dimension, adversarially verified) — CRITICAL.
- **Lesson:** when a backend command writes config keys that a reactive form ALSO owns, the form is a
  stale SECOND writer — after the command, re-`patchValue` the affected controls from fresh backend
  config, or the next `save()` clobbers the command's write. A derived display label (posture / status /
  badge) must be re-fetched after ANY config-affecting action, never left optimistic.
- **Status:** journal

### [2026-07-02 seed] Distilled from angular-zoneless.md traps T1–T4
- **Pattern:** the four Murmur-specific FE traps + the signals/IPC/template hard rules.
- **Caught by:** operator (seeding the loop).
- **Lesson:** the bullets above; full detail in `.claude/rules/angular-zoneless.md`.
- **Status:** distilled (2026-07-02)
