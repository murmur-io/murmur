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

### [2026-07-02 seed] Distilled from angular-zoneless.md traps T1–T4
- **Pattern:** the four Murmur-specific FE traps + the signals/IPC/template hard rules.
- **Caught by:** operator (seeding the loop).
- **Lesson:** the bullets above; full detail in `.claude/rules/angular-zoneless.md`.
- **Status:** distilled (2026-07-02)
