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
- **Directory per component** (2026-07-04, user-approved): `name/name.component.{ts,html,scss}`
  with `templateUrl`/`styleUrl` — never inline template/styles. Exemplars in
  `src/app/design-system/`.
- **Liquid Glass + tokens + catalog** (rule §6b): new views model on the macOS glass chrome;
  every design value from `src/design-tokens/*.css` (missing value → ADD a token there with its
  light override, never a raw hex/px in scss); reusable/atomic components go in
  `src/app/design-system/` under the `mur-` prefix (form controls as CVAs — check the 14-strong
  catalog + `primitives.css` before rolling a one-off).
- **Verify live**, not just by build: drive `:1420` with a mocked
  `window.__TAURI_INTERNALS__.invoke`; Playwright defaults colorScheme LIGHT — eyeball the PNG,
  don't trust a shallow shell screenshot.

## Run journal
<!-- Append-only, newest first. -->

### [2026-07-21 PR#423 meeting-detail perf] Making a data source LAZY breaks any one-shot DOM effect keyed on a DIFFERENT trigger
- **Pattern:** the meeting transcript `segments` moved from an eager DTO field to a LAZY fetch (loaded only
  when the Audio tab opens). The audio-panel's receipt-seek did its scroll-into-view + pulse-restart in a
  ONE-SHOT `afterNextRender` fired from `_applyReceiptSeek`, keyed only on `seekTarget()`. With lazy
  segments, a receipt clicked from the Note tab sets `seekTarget` BEFORE the segments (and thus the `.frag`)
  exist → the DOM lookup ran against an EMPTY transcript and silently no-opped (no scroll, no pulse). A
  green `ng build` + the old tests (which baked segments into the detail DTO) hid it entirely.
- **Fix:** SPLIT the effect. `_applyReceiptSeek` sets only panel-local flash STATE (`flashSegId`/`flashSeq`)
  + seeks; a NEW `_scrollFlashIntoView` effect keyed on BOTH `flashKey()` AND `renderedTurns()` does the DOM
  scroll+pulse, so it RE-RUNS when the lazily-fetched segments finally render. A `lastScrolledFlashKey`
  field makes it once-per-flash.
- **Caught by:** adversarial-verifier driving `receipts.spec.ts` + `transcript-cap.spec.ts` against the
  real lazy path.
- **Lesson:** when making a data source lazy/async, audit every one-shot DOM read keyed on another trigger.
  Re-key the DOM step on the rendered list with a once-per-key guard, and update tests that previously
  embedded the now-lazy data in an eager DTO.
- **Status:** journal

### [2026-07-04] Settings auto-save — a type="number" input killed the save stream
- **Pattern 1:** an `<input type="number">` bound to a STRING-typed form control commits a
  NUMBER (or null when cleared) via NumberValueAccessor — any `.trim()`/string method on
  `getRawValue()` then throws. Normalize (`raw == null ? "" : String(raw)`) before string ops.
- **Pattern 2:** a synchronous throw inside a `.subscribe()` callback KILLS the subscription —
  an auto-save wired as `valueChanges → debounce → subscribe(save)` dies silently for the whole
  session on the first bad value. Wrap the callback body in try/catch and surface the error.
- **Pattern 3:** replacing a Save button with debounced auto-save needs a destroy-flush: a
  change made <debounce before navigation is otherwise dropped (`DestroyRef.onDestroy` +
  pending flag). And legacy direct `save()` calls (select `(change)` handlers) double-save —
  retire them.
- **Caught by:** adversarial-verifier live pass (payload-capturing mock invoke); build+lint
  green throughout.
- **Status:** journaled

### [2026-07-04] Apple TV shell prototype — 2 findings the build/lint missed
- **Pattern 1:** an `(keydown.escape)` bound on an overlay's scrim/panel only fires while focus
  sits INSIDE that subtree — click any non-focusable text and focus falls to `<body>`, Esc goes
  dead. A modal's Esc-to-close belongs on the shell's `(document:keydown)` host listener.
- **Pattern 2:** a host class derived from a persisted mode (`[class.pill-mode]="pillMode()"`)
  leaks its layout side-effects onto routes that hide the chrome — gate the binding on the same
  route condition that hides the chrome (`pillMode() && !inDrilldown()`), not on the raw pref.
- **Caught by:** adversarial-verifier live Playwright pass (RED observed pre-fix, GREEN post-fix);
  `ng build`/`ng lint` were green the whole time.
- **Status:** journaled
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
