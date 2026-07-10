# FE change-detection checklist (Angular 22 zoneless)

Deep reference for `/app-perf-audit`. The binding FE ruleset is `.claude/rules/angular-zoneless.md`
(imported every session) — this file is the **performance-specific** subset: how a zoneless CD storm
happens on Murmur, the shipped fixes, and how to measure the fix. Do not duplicate the rule file; this
extends it with numbers and war stories. Cite by SYMBOL — line numbers in `commands.rs`/`db.rs`/big
components drift; `grep` the name.

## The model you must hold

`src/app/app.config.ts` wires `provideZonelessChangeDetection()` (STABLE — never
`provideExperimentalZonelessChangeDetection`; `zone.js` is not a dependency, `polyfills` is empty).
Under zoneless, change detection runs ONLY when:
- a **signal** read in a template changes,
- a template **event** fires, or
- an explicit framework hook (`markForCheck`-equivalent) runs — which you never write; a signal write
  is the sanctioned trigger.

So the whole perf model is: **a `computed()` is cached + dependency-tracked (re-runs only when a
dependency changes); a method/getter called from the template re-runs on EVERY CD pass.** The eval
storm is always a value that should be a `computed` living in a method/getter instead.

## Anti-pattern 1 — O(n) method/getter binding in a template (the eval storm)

**The shipped war story (this is the reference fix).**
`src/app/features/detail/audio-panel/audio-panel.component.ts` used to bind the karaoke highlight
through an `isActiveSegment()` METHOD:

```html
<!-- BEFORE (storm): the method re-ran O(n) per fragment on EVERY CD pass -->
[class.is-active]="isActiveSegment(s)"
```

During playback the playhead ticks ~4×/s → a full CD pass each tick. With `isActiveSegment` called once
per fragment, a 1h transcript (thousands of fragments) meant an **~8k-eval/s storm** — the
component's own docstring records it: *"Replaces the former `isActiveSegment()` METHOD binding, which
Angular re-ran O(n) per fragment on EVERY change-detection pass (~4×/s during playback → an
~8k-eval/s storm for a 1h transcript)."*

The fix, verbatim in `activeSegKeys`:

```ts
// Scanned ONCE per currentTime tick (O(n)); the template then does O(1) .has() per fragment.
readonly activeSegKeys = computed<Set<number>>(() => {
  const t = this.currentTime();
  const out = new Set<number>();
  for (const s of this.segments()) {
    if (t >= s.startS && t < s.endS) out.add(s.idx);
  }
  return out;
});
```

```html
<!-- AFTER: one Set build per tick, O(1) membership per fragment -->
[class.is-active]="activeSegKeys().has(s.idx)"
```

A `Set` (not a single active key) preserves the original semantics of highlighting EVERY overlapping
me/others fragment. This is the canonical shape: **derive-once-into-a-Set-or-Map, membership-check in
the template.**

**Audit heuristic.** In `**/*.html`, any binding calling a component method or a getter — especially
inside a `@for` — that isn't a bare `signal()` call is a candidate. `.filter()/.map()/.find()/.sort()`
inside a template interpolation is always wrong (build a `computed` upstream). ESLint lints the external
templates (`templateRecommended` + `templateAccessibility` in `eslint.config.js`), but it does NOT flag
the perf cost of a method binding — you must eyeball it.

## Anti-pattern 2 — un-windowed `@for` over a long list (DOM node blowup)

A `@for` over the full transcript instantiates a DOM node per turn; for a 1h meeting that is a huge DOM
that both bloats RSS (feeds the OOM anatomy — see the rust-profiling-recipe) and makes every CD pass
walk more nodes. The shipped fix — **a `RENDER_CAP` window, NOT a new virtual-scroll dependency** — is
in the same `audio-panel.component.ts`:

```ts
private readonly RENDER_CAP = 80;
readonly visibleTurns = computed<Turn[]>(() => { /* filter (Find box) */ });
readonly renderedTurns = computed<Turn[]>(() => {
  const all = this.visibleTurns();
  if (this.transcriptExpanded() || all.length <= this.RENDER_CAP) return all;
  // first RENDER_CAP, always extended to include the turn the playhead is inside (karaoke)
  …
});
```

The template `@for`s over `renderedTurns()` (capped) with a "reveal full transcript" affordance that
drops the window. There is **no `@angular/cdk` in `package.json`** — reach for a `RENDER_CAP` window
before proposing virtual scroll (a new dep needs explicit user approval, per the no-new-deps rule).

`@for` MUST `track` a stable id (`track s.idx` / `track item.id`), never `track $index` for keyed data —
`track $index` forces Angular to re-render rows on any reorder (correctness + perf).

## Anti-pattern 3 — effect-orchestrated IPC fetch without a stale-result guard

Not a CD-storm, but the FE-seam perf/correctness bug: an `effect()` that re-fetches on an input change
can have a SLOW response land AFTER a newer input already moved on, clobbering fresh state with stale
data. Every effect-orchestrated fetch needs a guard. Reference:
`src/app/features/graph/entity-detail/entity-detail.component.ts` `_load`:

```ts
private readonly _load = effect(async () => {
  const id = this.entityId();            // track the input
  this.loading.set(true);
  const data = await this.ipc.getEntityDetail(id);
  if (this.entityId() !== id) return;    // STALE GUARD — a newer id superseded this fetch; drop it
  this.detail.set(data);
  if (this.entityId() === id) this.loading.set(false);
});
```

`src/app/features/graph/graph/graph.component.ts` `_refetchOnLock` mirrors it (re-fetch when
`folders.tree()` changes — a lock/unlock live-updates the graph without a stale view). Signal writes
inside a tracked `effect()` are ALLOWED since v19 — do NOT add `{ allowSignalWrites: true }` (a
deprecated no-op the v22 migration removed repo-wide; an AI trained on Angular 18 will try to
reintroduce it — refuse).

## Anti-pattern 4 — `setTimeout`/`rAF` in a component for DOM-after-render work

A bare `setTimeout`/`requestAnimationFrame` in a component is BANNED (it's untracked, leaks on destroy,
and fights zoneless CD). DOM-after-render work (focus, scroll-into-view, measure) uses
**`afterNextRender(fn)`** — a zoneless-safe one-shot, auto-torn-down. When registered outside the
constructor/field-init injection context (e.g. in a click handler) pass the injector:
`afterNextRender(fn, { injector: this.injector })`. Reference: the `_karaokeScroll` effect in
`audio-panel.component.ts` scrolls the active turn into view via `afterNextRender`, skipping when the
user is typing in the Find box. The only sanctioned `setTimeout` is a tracked handle in a
`providedIn:"root"` service cleared in `DestroyRef.onDestroy` (`services/toast.service.ts`).

## Anti-pattern 5 — subscribe-for-state / hand-rolled interval instead of `toSignal`

`.subscribe()` that writes a plain field, `async` pipe, or a hand-rolled `setInterval` that writes a
field are all banned — they bypass signals and either don't trigger CD or leak. Polled streams bridge an
rxjs `interval` into a signal via `toObservable` + `switchMap` + `toSignal` so the framework owns the
subscription lifecycle. Reference: `src/app/core/recorder.store.ts` `level` (`interval(100)`) and
`elapsed` (`interval(250)`) — each `switchMap`s off `isRecording` so it emits ONLY while recording (the
interval is torn down when idle, not left running). Event streams (`onStatus`, `onLiveCaption`) are
`listen<T>()` subscriptions kept once in a store's `init()` and released via the stored `UnlistenFn`.

## Measuring the FE fix (on WebKit — this is load-bearing)

1. Run the dev app (`/tauri-dev` recipe) → `http://localhost:1420`.
2. Drive the exact path via Playwright MCP (navigate to `/detail/:id`, start playback, etc.).
3. Read `browser_console_messages` — ZERO NG errors (an `NG0600`/`ɵcmp`/unhandled rejection is a FAIL
   even if `ng build` was green).
4. For the CD-storm claim, confirm the value is a `computed` (fires on dependency change) not a method
   (fires every pass) — read the diff, and if instrumenting, count invocations.
5. **T4 — verify render/CSP claims on WebKit, not Chromium.** A green `ng serve`/Chromium check will NOT
   reproduce the packaged WKWebView behavior. Serve `dist/meetnotes/browser` and load it in Playwright
   **WebKit** with the real `style-src` CSP; a Chromium pass is not proof. `npx ng lint` + `npx ng build`
   are necessary but not sufficient — the WebKit render is the real gate.

## The related bug classes to also probe (from the adversarial hunt list)

A CD/perf change on a lock-visibility component must not regress a leak: a locked meeting's masked DTO
carries no `note`/`segments` and `audioPath: null`; a windowed/cached render must not resurrect
plaintext for a sealed meeting. An import-cycle among mutually-recursive standalone components needs
`forwardRef(() => Other)` in both `imports:` (the `folder-tree` ↔ `folder-row` `ɵcmp`-undefined trap) —
a `computed`/windowing refactor that touches those files must keep the `forwardRef`.
