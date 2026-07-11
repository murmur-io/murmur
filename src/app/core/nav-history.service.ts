import { Injectable, inject } from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { NavigationEnd, Router } from "@angular/router";
import { filter, map, scan, startWith } from "rxjs";

/** Fallback destination when the user deep-links straight into a drill-down. */
const DEFAULT_APP_ROUTE = "/record";

/**
 * A "drill-down" is a route that HIDES the primary rail so the current flow owns
 * the window with its own back-affordance: Settings, Meetings/Library, a
 * meeting's detail (`/meeting/:id`), AND Notes (`/notes` home + `/notes/:id`
 * editor) — the Notes home owns its own [rail | content] layout and the editor
 * stays "inside" the Notes flow, so the primary rail must NOT reappear there.
 * The "← Murmur" back target must be a NON-drill-down route, so both this service
 * and app-shell's rail-hide gate share this single predicate. Keep the two in
 * lockstep — a new drill-down route added here is also hidden in app-shell.
 */
export function isDrilldownRoute(url: string): boolean {
  return (
    url.startsWith("/settings") ||
    url.startsWith("/library") ||
    url.startsWith("/meeting") ||
    url.startsWith("/notes") ||
    // The org-item viewer is "inside" the Notes flow (reached from an org card; its
    // own Back returns to /notes). Treating it as a drill-down (a) hides the primary
    // rail so it owns the window like the note editor, and (b) keeps it OUT of
    // `_lastAppRoute` so the Notes "← Murmur" affordance returns to the real prior
    // app view (/record), not back into an org-item viewer.
    url.startsWith("/org-item")
  );
}

/**
 * Tracks the last app route the user was on that is NOT a drill-down, so a
 * drill-down's "← Murmur" affordance can return them to where they were
 * (Record → Meetings → back to Record), falling back to `/record` when there is
 * no such history (a fresh deep-link to `/settings` or `/library`).
 *
 * The router subscription lifecycle is framework-managed via `toSignal` — this
 * service never hand-rolls a `.subscribe()` that writes a field (zoneless rule).
 * The "last NON-drill-down" carry-over is done inside the rxjs `scan` (so we
 * never need a signal that reads its own previous value, which Angular 18.2's
 * `computed` cannot express and which would trip NG0600 inside an `effect`).
 */
@Injectable({ providedIn: "root" })
export class NavHistoryService {
  private readonly router = inject(Router);

  /**
   * The last completed navigation whose URL is NOT a drill-down. `scan` keeps the
   * prior value while the current URL is a drill-down route, so once inside a
   * drill-down this reports the pre-entry route rather than resetting. Seeded from
   * `router.url` (via `startWith`) so a cold deep-link is handled.
   */
  private readonly _lastAppRoute = toSignal(
    this.router.events.pipe(
      filter((e): e is NavigationEnd => e instanceof NavigationEnd),
      map(() => this.router.url),
      startWith(this.router.url),
      scan(
        (last, url) => (isDrilldownRoute(url) ? last : url),
        DEFAULT_APP_ROUTE,
      ),
    ),
    { initialValue: DEFAULT_APP_ROUTE },
  );

  /** The route "← Murmur" returns to; defaults to `/record`. */
  readonly lastAppRoute = this._lastAppRoute;

  /** Navigate back to the last non-drill-down route (or the default). */
  back(): void {
    void this.router.navigateByUrl(this.lastAppRoute());
  }
}
