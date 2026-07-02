# Settings drill-down (L2) navigation — design

## Problem
Settings shows THREE columns at once: the primary app rail (`app-shell` — Record/Meetings/Analytics/Graph/Brain/Ask + Settings/Collapse footer), a separate settings-section list (`app-settings`'s own `settings-sidebar` card, added in the #133 split), and the content pane. Three columns leave the content (e.g. the AI & Models connection cards) cramped.

## Decision
**Full drill-down (Option A, user-approved).** Entering `/settings` hides the primary app rail; the settings-section list becomes the leftmost column (level 2), leaving two columns `[settings sections | content]` and giving content back ~230px. A "← Murmur" affordance at the top of the L2 rail returns to wherever the user was.

## Interaction
- Navigate to any `/settings*` route → the `app-shell` primary `<aside>` is hidden (`display:none`); the router-outlet (`app-settings`) spans full width and renders its own `[section rail | content]`.
- The settings section rail gains a **"← Murmur"** back item at the very top (above search). It navigates to the **last non-settings route** (e.g. came from Meetings → returns to Meetings); fallback `/record` when there is no such history (deep-link straight to `/settings`). The `murmur` brand/logo does the same.
- The section rail restyles from a floating card to a **full-height left rail** (flush-left, matching the primary rail's role) so it reads as a first-class column, not a popover. Search at top (under Back), Save at bottom — unchanged.
- Subtle slide-in on the rail when entering settings; honor `@media (prefers-reduced-motion: reduce)`.
- `Esc` while in settings triggers the same back navigation (nice-to-have).
- The primary rail's "Collapse" toggle and "unlocked" lock indicator are NOT shown at the settings level (they return on exit). No collapse of the settings rail.

## Architecture (no state coupling)
- **`NavHistoryService`** (`providedIn:'root'`, the only new unit): tracks the last URL that does not start with `/settings` via `toSignal(router.events …)` filtered to `NavigationEnd`. Exposes `lastAppRoute()` (default `/record`) and a `back()` that `router.navigateByUrl(lastAppRoute())`. Owns the router subscription lifecycle (framework-managed via `toSignal`).
- **`app-shell.component.ts`**: `inject(Router)`; `inSettings = computed(() => currentUrl().startsWith('/settings'))` where `currentUrl` is a `toSignal` of router `NavigationEnd`/`router.url`. The primary `<aside>` is hidden (`@if (!inSettings())` or a `[class.hidden]` with `display:none`) when in settings, so the outlet gets full width. No settings state enters the shell.
- **`app-settings.component.ts`**: prepend a "← Murmur" back button to the `settings-sidebar` that calls `NavHistoryService.back()` (label can read the target section name or just "Murmur"); restyle `settings-shell`/`settings-sidebar` to a full-height flush-left rail; add the reduced-motion-aware slide.

Rationale for Approach 2 (shell hides its rail; settings keeps owning its sidebar) over "shell renders the settings nav itself": zero coupling — `app-shell` never learns the settings sections/active-section/Save; `app-settings` already owns that sidebar after #133.

## Constraints (binding)
Angular zoneless: signals/`computed`/`toSignal` (never subscribe-for-state in a component), `@if`/`@for`, `inject()`, `input()/output()`, no `setTimeout` in components, `var(--token)` styling, ≤16 kB per-component style budget, no new npm deps. Nav is path-routing (already). `com.meetnotes.app` immutable.

## Testing (headless, mocked IPC)
Playwright against a built dist with mocked `__TAURI_INTERNALS__`:
1. `/settings` renders **two** columns, not three — the primary app rail (Record/Meetings/…) is absent from the DOM/hidden.
2. From `/library` (Meetings) → open Settings → "← Murmur" returns to `/library`.
3. Deep-link straight to `/settings` → back navigates to `/record`.
4. Section switching, search filtering, and Save still work (payload unchanged).
5. Console clean: no NG0600, no `ɵcmp`.
6. `ng lint` + `ng build` green; per-component style budgets respected.

## Out of scope (v1)
Route transition animations beyond a simple rail slide; showing the unlocked/lock indicator at the settings level; a persistent icon rail (that was Option B, rejected).
