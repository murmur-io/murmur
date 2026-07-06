# Angular 22 Zoneless — Murmur `src/app` (binding ruleset)

> The canonical FE ruleset for Murmur's Angular frontend. BINDING: every rule
> here is enforced. Murmur runs **Angular `^22.0.5`** on the **`@angular/build`**
> builder (the Angular CLI needs **Node ≥ 24.15** — on this machine use nvm's
> v24.18.0). The frontend is **zoneless** (`provideZonelessChangeDetection()`
> in `src/app/app.config.ts`; `zone.js` is NOT a dependency and `polyfills` is
> empty — never reintroduce it) — change detection only runs
> when a **signal** changes, a template event fires, or an explicit
> `markForCheck`-equivalent happens. Plain mutable fields do not trigger renders.
> If your view is stale, the cause is almost always state living in a field that
> should be a `signal()`.
>
> Murmur talks to the Rust/Tauri core through `src/app/core/ipc.service.ts` —
> **there is no NgRx, no facade layer, no HTTP data layer.** Every screen is a
> standalone signal component that calls IPC methods and stores the result in
> signals. Do not introduce NgRx, a "store" abstraction beyond the existing
> `*.store.ts`/`*.service.ts` singletons, or `async` pipes.

---

## 1. Component shape (HARD)

- **Standalone only.** No `NgModule`. Standalone is the DEFAULT since v19 —
  do NOT write `standalone: true` (redundant; removed repo-wide in the v22
  migration) and never `standalone: false`.
- **`OnPush` always.** `changeDetection: ChangeDetectionStrategy.OnPush`. Under
  zoneless this is effectively mandatory; omitting it does not buy you anything.
- **Directory per component, split files** (convention CHANGED 2026-07-04 with
  explicit user approval — supersedes the former inline-single-file rule):
  every component lives in its own directory as `name/name.component.ts` +
  `name.component.html` (`templateUrl`) + `name.component.scss` (`styleUrl`).
  Exemplars: `src/app/design-system/nav-icon/`, `src/app/design-system/quick-search/`.
  ESLint lints external templates via the `**/*.html` block in
  `eslint.config.js` (templateRecommended + templateAccessibility).
  EXCEPTION: the app-shell CHROME CSS stays GLOBAL in `styles.css` (the
  WKWebView cold-launch FOUC fix — see app-shell.component.ts) — never move it
  into component styles.
- **Design system & tokens:** reusable UI primitives live in
  `src/app/design-system/` (components in their own directories + the shared
  `primitives.css` control language); every design variable lives in
  `src/design-tokens/*.css` (imported at the top of `styles.css`) — components
  consume `var(--token)` only, never a raw value.
- **`inject()` only.** No constructor injection. `private readonly ipc = inject(IpcService);`.
- **Selectors are `app-` kebab-case** for components and **`app` camelCase** for
  directives — enforced by `@angular-eslint/component-selector` /
  `directive-selector` in `eslint.config.js` (e.g. `app-folder-tree`,
  `appFolderDrop`).
- **Keep the `Component` suffix on the class name** (`RecordComponent`,
  `FolderTreeComponent`, `EntityDetailComponent`). Murmur uses it everywhere —
  do NOT strip it. Stores/services use `Store`/`Service` suffixes
  (`RecorderStore`, `FoldersService`).

## 2. State = signals (HARD)

- All component/store state is `signal()` / `computed()` / `effect()`. No plain
  mutable fields for anything the template reads. Reference store:
  `src/app/core/recorder.store.ts` (private writable `_stage` + public
  `.asReadonly()` + `computed` derivations), and `src/app/services/folders.service.ts`.
- Expose writable signals to the outside as **read-only**: keep a private
  `_x = signal(...)` and publish `readonly x = this._x.asReadonly();`.
- **Derive, don't recompute in the template.** Any value computed from other
  signals is a `computed()`, never a method/getter called from the template
  (a getter re-runs every CD pass; a `computed` is cached + dependency-tracked).
- `input()` / `output()` / `viewChild()` signal APIs only — never the
  `@Input`/`@Output`/`@ViewChild` decorators. Examples:
  `MeetingTimelineComponent` (`input<MeetingTimelineData | null>(null)`,
  `output<number>()`), `pre-meeting-brief.component.ts`
  (`viewChild<ElementRef<HTMLInputElement>>("input")`).

## 3. IPC: Promises and event streams — NEVER subscribe-for-state (HARD)

`IpcService` is a thin wrapper over `@tauri-apps/api` `invoke`/`listen`. Two shapes:

- **One-shot commands** return a `Promise` (`listMeetings()`, `getGraph()`,
  `getMeetingDetail(id)`, `unlockMeeting(id)`, …). Consume them by `await`-ing
  inside an `effect()` that tracks the inputs that should re-fetch, then writing
  the result into signals — with a **stale-result guard** when an input can
  change mid-flight. Canonical: `entity-detail.component.ts` `_load` effect
  (re-fetch on `entityId()` change, drop late responses) and
  `graph.component.ts` `_refetchOnLock` effect (re-fetch when
  `folders.tree()` changes).
- **Event streams** (`onStatus`, `onVoiceStart`, `onToggleRecord`,
  `onLiveCaption`) return `Promise<UnlistenFn>`. Subscribe **once** in a store's
  `init()`, push payloads into signals, and keep the `UnlistenFn` to release on
  teardown. Canonical: `RecorderStore.init()`.
- **`toSignal()` is for rxjs-bridged streams** (the polled `level` / `elapsed`
  in `RecorderStore` wrap an rxjs `interval` via `toObservable` + `switchMap` +
  `toSignal` so the subscription lifecycle is owned by the framework). Use this
  shape — never a hand-rolled `setInterval`/`subscribe` that writes a field.
- **BANNED: `.subscribe()` to populate state.** No
  `this.ipc.x().then(v => this.field = v)` into a plain field, no `subscribe`
  whose callback assigns a non-signal, no `async` pipe. The result of an IPC
  call must land in a `signal`.
- **One IPC method per Tauri command.** New backend command → add ONE typed
  method to `ipc.service.ts` returning `Promise<T>` with `T` declared in
  `src/app/core/models.ts`. Do not call `invoke(...)` directly from a component.

## 4. Templates: built-in control flow only (HARD)

- `@if` / `@for` / `@switch` / `@let` ONLY. The structural directives
  `*ngIf` / `*ngFor` / `*ngSwitch` are BANNED.
- `@for` MUST `track` a stable identity (`track item.id`, `track f.id`), never
  `track $index` for keyed data.
- Use `@empty {}` on `@for` for empty states (see `move-to-menu.component.ts`,
  `folder-tree.component.ts`).
- Bind to signals by calling them: `@if (loading()) { … }`, `{{ title() }}`.

## 5. Side-effect timing — NEVER `setTimeout`/`rAF` in a component (HARD)

- DOM-after-render work (focus, scroll-into-view, measure) uses
  **`afterNextRender(fn)`** — a zoneless-safe one-shot, auto-torn-down on
  destroy. Examples: `folder-tree.component.ts:457` (focus the new-folder
  field), `detail.component.ts:1830` (focus the Unlock button),
  `ask.component.ts:767` (scroll the transcript), `meeting-recipes.component.ts:611`.
- When you register `afterNextRender` **outside** the constructor/field-init
  injection context (e.g. inside a click handler), pass the injector:
  `afterNextRender(fn, { injector: this.injector })` — Murmur injects
  `private readonly injector = inject(Injector)` for exactly this (see
  `folder-tree` / `detail`). Forgetting it throws "afterNextRender() can only be
  used within an injection context".
- **Service timers are the only sanctioned `setTimeout`.** A `providedIn:"root"`
  service may use tracked `setTimeout` handles, stored in a `Map`, all cleared
  in `DestroyRef.onDestroy(...)`. Reference: `src/app/services/toast.service.ts`.
  A bare `setTimeout`/`requestAnimationFrame` in a component is BANNED.
- DOM observers (`ResizeObserver`/`MutationObserver`/`IntersectionObserver`)
  belong in a directive with `DestroyRef.onDestroy()` cleanup, never ad-hoc in a
  component (Murmur's DOM-side concerns live in directives like
  `folder-drop.directive.ts`).

## 6. Styling — tokens, opaque overlays, budget (HARD)

- **`var(--token)` for every color, radius, spacing, shadow, motion value.** The
  full token set lives in **`src/design-tokens/*.css`** (`typography` / `colors` /
  `layout` / `glass` / `theme-light`, imported at the top of `styles.css`):
  `--surface-*`, `--accent*`, `--live*`, `--text-*`, `--space-1..8`,
  `--radius-*`, `--shadow-*`, `--transition*`, `--glass-*`, `--shell-*`.
  Never hardcode a hex color or a spacing/radius value a token
  already names. (One-off structural pixel sizes — a `min-width` — are fine; the
  rule is about the design language.)
- **Overlays must be OPAQUE — not the frosted `.card`.** A popover / menu /
  modal / dropdown that floats OVER other content uses
  `background: var(--surface-overlay)` (opaque `#1b1b24`) with
  `backdrop-filter: none`, `border: 1px solid var(--border-strong)`,
  `box-shadow: var(--shadow-lg)`. The global `.card` is a **translucent**
  frosted surface (`--surface-raised` + `backdrop-filter: blur(...)`); using it
  for a floating popover bleeds the content behind it through (a broken-looking
  modal). Reference + rationale comment: `move-to-menu.component.ts:140-148`.
  `<select>` options likewise paint `var(--surface-overlay)`.
- **16 kB per-component style budget** (`anyComponentStyle`: error 16 kB, warn
  12 kB in `angular.json`). Inline `styles` over budget fail `ng build`. Lean on
  the global primitives in `src/styles.css` (`.btn`, `.btn-primary`,
  `.btn-ghost`, `.card`, `.pill`, `.banner`, `.count`, `.empty-state`,
  `.state-card`) instead of re-declaring them per component.
- **Icons are inline SVG** in the template (no icon-font, no icon package).
- **`@media (prefers-reduced-motion: reduce)`** is honored globally — don't
  fight it with `!important` animations.

## 6b. Liquid Glass — the design model for every NEW view (HARD)

New screens/views model on **macOS Liquid Glass** (HIG "Materials"; the shell
prototype is the reference implementation). The concrete contract:

- **Tokens are the ONLY source of design values.** Every color/radius/spacing/
  shadow/blur comes from `src/design-tokens/*.css` (`typography` / `colors` /
  `layout` / `glass` / `theme-light`). A value with no matching token means you
  ADD a token there — always with its **light-theme override** in
  `theme-light.css` (and the `prefers-color-scheme` system block) — never a raw
  hex/px/rgba in component scss. `--glass-user-alpha` (the Settings
  transparency slider + `prefers-reduced-transparency`) must keep working:
  translucent chrome surfaces ride the `--shell-glass-veil` layer.
- **Glass is CHROME, not content.** Floating rails/bars use the shared panel
  (`<mur-sidebar>` / global `.drill-rail`, `--shell-glass-*` tokens: gradient
  fill + lensing rim + 44px blur @ 210% saturation over the aurora field).
  In-flow content panels use the frosted `.card` / `<mur-card>`. Floating
  overlays stay OPAQUE per T3 — glass never stacks on glass.
- **Neutral chrome, restrained accent.** Active/selected chrome items are the
  neutral glass-on-glass pill (`--shell-active-bg/-text/-shadow`) — the accent
  colors only the glyph/label. Native selections (menus, list rows) are the
  flat `--accent` fill + `--text-on-accent` content.
- **Reusable/atomic components live in `src/app/design-system/`** under the
  `mur-` selector prefix, each in its own directory. Form controls implement
  `ControlValueAccessor` (so `formControlName` binds directly — see
  `mur-toggle`); number inputs stay native (`NumberValueAccessor` commits
  numbers — the storage-limit lesson). Before writing ANY new control, check
  the catalog: icon, sidebar, quick-search, toggle, input, select, slider,
  segmented, kbd, spinner, banner, pill, card, empty-state, **button**
  (`<mur-button variant="…" [busy]="…" [disabled]="…">` — wraps a native
  `<button>`; variants ride the `.btn` primitives; link-shaped `<a routerLink>`
  and bespoke icon-square buttons stay on legacy `.btn` classes until it grows
  href/iconOnly support) — and
  `primitives.css` for class-based primitives (`.btn`, `.seg`, `.menu`,
  `.panel-card`, `.tabbar`…). Extending the catalog beats re-rolling a one-off.

## 7. No new dependencies (HARD)

- No new npm packages without explicit user approval. The FE runs on
  `@angular/*`, `@tauri-apps/api`, and `rxjs` (used narrowly, only to bridge
  event/interval streams into signals as in `recorder.store.ts`). Adding a UI
  kit, icon library, state library, charting lib, etc. is forbidden.

## Banned → replacement

| Banned | Use instead |
| --- | --- |
| `*ngIf` / `*ngFor` / `*ngSwitch` | `@if` / `@for` / `@switch` |
| `track $index` (keyed lists) | `track item.id` |
| `@Input()` | `input()` |
| `@Output() EventEmitter` | `output()` |
| `@ViewChild()` | `viewChild()` |
| Constructor injection | `inject()` |
| Plain mutable field as state | `signal()` |
| Getter / method called from template | `computed()` |
| `markForCheck()` / `detectChanges()` | a `signal` write (delete the call) |
| `BehaviorSubject` as component/store state | `signal()` |
| `.subscribe()` to populate state | `effect()` + `await ipc.x()` → signal, or `toSignal()` |
| `async` pipe | signal call `x()` in the template |
| `setTimeout` / `requestAnimationFrame` in a component | `afterNextRender(fn, { injector })` |
| Ad-hoc `ResizeObserver`/`MutationObserver` in a component | a directive with `DestroyRef.onDestroy()` |
| `invoke('cmd', …)` from a component | a typed method on `IpcService` |
| single-file component (inline `template`/`styles`) | a component DIRECTORY: `name/name.component.{ts,html,scss}` (changed 2026-07-04, user-approved) |
| Hardcoded hex / spacing / radius / shadow | `var(--token)` from `src/styles.css` |
| New npm package (FE) | ask the user; reuse `@angular/*` / `rxjs` / `@tauri-apps/api` |
| `NgModule`, NgRx, a new facade/store abstraction | standalone component + `IpcService` + signals |
| `standalone: true` in a decorator | omit it — standalone is the v19+ default |
| raw `class="btn btn-*"` in NEW templates | `<mur-button variant="…">` (legacy call sites migrate in waves; links/icon-squares exempt until covered) |
| `{ allowSignalWrites: true }` on `effect()` | delete it — writes are allowed since v19 (deprecated no-op) |
| `provideExperimentalZonelessChangeDetection` | `provideZonelessChangeDetection` (stable) |
| `@angular-devkit/build-angular` builders | `@angular/build:application` / `@angular/build:dev-server` |
| `zone.js` (dependency or polyfill) | nothing — the app is zoneless |

## CRITICAL Murmur-specific traps

### T1 — signal writes inside a tracked `effect()` (Angular 22 semantics)
Signal writes in effects are **allowed by default since v19** — NG0600 is gone
and the old `allowSignalWrites` flag is a **deprecated no-op** (it still
typechecks in v22 but does nothing). Never add it (it was removed repo-wide in
the v22 migration; an AI trained on Angular 18 code will try to reintroduce
it — refuse):

```ts
private readonly _load = effect(() => {
  const id = this.entityId();
  this.loading.set(true);          // fine in v22 — no flag needed
  void this.fetch(id);
});
```

The DISCIPLINE the flag used to enforce still binds: prefer **deriving with
`computed()`**; an effect that writes signals is legitimate ONLY when it
genuinely orchestrates an async IPC fetch (set loading → await → write result,
with a stale-result guard). Live examples: `entity-detail.component.ts`,
`graph.component.ts` (grep the `_load` / `_refetchOnLock` effects).

### T2 — mutually-recursive standalone components need `forwardRef`
`FolderTreeComponent` ↔ `FolderRowComponent` render each other (a row renders
its children through another tree), so their ES modules form an **import cycle**.
A direct class reference in `imports:` is `undefined` at metadata-evaluation time
and Angular then throws `getComponentDef(undefined)` (reading `'ɵcmp'`) the first
time the `@for` instantiates the child — the infamous "view breaks after adding
the first folder" bug. Defer the lookup:

```ts
imports: [FolderDropDirective, forwardRef(() => FolderRowComponent)],
```

Live mirrors: `folder-tree.component.ts:39-46`, `folder-row.component.ts:48-55`.
Any new self-referential / mutually-recursive standalone pair MUST use
`forwardRef(() => Other)` in both directions.

### T3 — floating popovers/modals must be OPAQUE, not the frosted `.card`
See §6. The frosted `.card` (`--surface-raised`, blurred) is for in-flow panels.
Anything that floats OVER content (menus, the move-to-folder popover, dialogs,
dropdowns, the lock gate's chrome) must use `var(--surface-overlay)` +
`backdrop-filter: none` or the underlying list bleeds through. Rationale comment
lives at `move-to-menu.component.ts:140-148`.

### T4 — prod WKWebView drops ALL component styles: Tauri's `style-src` nonce kills `'unsafe-inline'`
**Symptom (shipped in 0.5.0, see screenshot in PR history):** the packaged/notarized
`.dmg` renders with the GLOBAL stylesheet working (nav/header/`.btn`/`.card` — anything
in `styles.css`, loaded via `<link>`) but EVERY component's encapsulated styles missing —
folder chips bare, meeting rows run title+date together, lists show raw `•` bullets.
**`ng serve` (dev) is always fine**, so a green `ng build` proves nothing here.

**Root cause (proven 3 ways — empirical WebKit repro + Tauri source + CSP3 spec):**
Angular emulated encapsulation injects each component's styles at RUNTIME as
`document.createElement("style")` `<style>` nodes (there are NO per-component `.css` files —
the prod `dist` has exactly ONE css file, the global `styles.css`). At build time Tauri
(`tauri-utils/html.rs` `inject_nonce_token` → `inject_nonce(document, "style", STYLE_NONCE_TOKEN)`,
then `tauri/src/manager/mod.rs` `replace_csp_nonce` for `"style-src"`) stamps a nonce on every
inline `<style>` in `index.html` and appends `'nonce-<perload>'` to the served `style-src`.
**Per CSP3 §6.7.3.2, once `style-src` contains a nonce/hash source, `'unsafe-inline'` is
IGNORED** — so Angular's runtime `<style>` nodes (which carry NO nonce, because `<app-root>`
has no `ngCspNonce`) are refused. The `<link>` global sheet survives (it's `'self'`, not inline).
Console shows: *"Refused to apply a stylesheet because its hash, its nonce, or 'unsafe-inline'
does not appear in the style-src directive."* This is independent of `inlineCritical` and of
the window/`visible:false` reveal.

**THE FIX (one line, maintainer-recommended, applied 2026-06-29):** in
`src-tauri/tauri.conf.json` `app.security`, tell Tauri NOT to touch `style-src`:
```json
"security": {
  "csp": "… style-src 'self' 'unsafe-inline'; …",
  "dangerousDisableAssetCspModification": ["style-src"]
}
```
Tauri then leaves the declared `'unsafe-inline'` effective → component `<style>` apply.
`script-src` stays strict (hashes + nonce — the real XSS surface is untouched). Do NOT instead
move every component's CSS into the global `styles.css` (doesn't scale, abandons encapsulation —
that was only ever a shell-only stopgap).

**Disproven theories — do NOT chase these again** (they cost 3 failed fixes before the real one):
it is NOT "WKWebView doesn't apply runtime-injected `<style>` until the viewport is invalidated"
(a window RESIZE does not fix it — decisive: a blocked style stays blocked through any reflow),
NOT an `inlineCritical`/`<link media=print onload>` issue (that's a *separate*, real `script-src`
bug fixed by `inlineCritical:false`, but it only affects the GLOBAL sheet, never component styles),
and NOT a hide-until-ready/`window.show()` timing bug. 0.4.0 "worked" only because its different
build masked the always-present CSP conflict.

**How to diagnose this class fast (no full Tauri build):** (1) the RESIZE test — resize the broken
window; if styles do NOT snap in, it's CSP-blocked, not a paint/timing bug. (2) Reproduce on the
real engine: serve `dist/meetnotes/browser` and load it in Playwright **WebKit** with the
`style-src` nonce added to a `Content-Security-Policy` header; check per-`<style>` `el.sheet === null`
(blocked) and read the console for the CSP refusal. A green `ng build` / a Chromium check will NOT
reproduce it — only WebKit + the real CSP does. Verify the actual fix in the REAL packaged WKWebView
build (notarized `.dmg`), never just `ng serve`.

---

## Quality gate (a change is not done until these are green)

- `npx ng lint` — clean (angular-eslint, inline-template rules included).
- `npx ng build` — clean, including the 16 kB per-component style budget.
- `bash scripts/ci.sh` — the full gate (Rust clippy/test/build, `ng lint`,
  `ng build`, headless core E2E). Frontend-only changes still must keep
  `ng lint` + `ng build` green within it.
