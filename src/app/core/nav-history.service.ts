import { Injectable, inject } from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { NavigationEnd, Router } from "@angular/router";
import { filter, map, scan, startWith } from "rxjs";

/** Fallback destination when the user deep-links straight into settings. */
const DEFAULT_APP_ROUTE = "/record";

/**
 * Tracks the last app route the user was on that is NOT under `/settings`, so the
 * settings drill-down "← Murmur" affordance can return them to where they were
 * (Meetings → back to Meetings), falling back to `/record` when there is no such
 * history (a fresh deep-link to `/settings`).
 *
 * The router subscription lifecycle is framework-managed via `toSignal` — this
 * service never hand-rolls a `.subscribe()` that writes a field (zoneless rule).
 * The "last NON-settings" carry-over is done inside the rxjs `scan` (so we never
 * need a signal that reads its own previous value, which Angular 18.2's
 * `computed` cannot express and which would trip NG0600 inside an `effect`).
 */
@Injectable({ providedIn: "root" })
export class NavHistoryService {
  private readonly router = inject(Router);

  /**
   * The last completed navigation whose URL is NOT under `/settings`. `scan`
   * keeps the prior value while the current URL is a settings route, so once
   * inside settings this reports the pre-entry route rather than resetting.
   * Seeded from `router.url` (via `startWith`) so a cold deep-link is handled.
   */
  private readonly _lastAppRoute = toSignal(
    this.router.events.pipe(
      filter((e): e is NavigationEnd => e instanceof NavigationEnd),
      map(() => this.router.url),
      startWith(this.router.url),
      scan(
        (last, url) => (url.startsWith("/settings") ? last : url),
        DEFAULT_APP_ROUTE,
      ),
    ),
    { initialValue: DEFAULT_APP_ROUTE },
  );

  /** The route "← Murmur" returns to; defaults to `/record`. */
  readonly lastAppRoute = this._lastAppRoute;

  /** Navigate back to the last non-settings route (or the default). */
  back(): void {
    void this.router.navigateByUrl(this.lastAppRoute());
  }
}
