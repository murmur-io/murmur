import { Injectable, inject } from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { NavigationEnd, Router } from "@angular/router";
import { filter, map, scan, startWith } from "rxjs";

/** Fallback destination when the user deep-links straight into a drill-down. */
const DEFAULT_APP_ROUTE = "/record";

/**
 * A "drill-down" is a route that HIDES the primary rail so the current flow
 * owns the window with its own back-affordance. The "← Murmur" back target
 * must be a NON-drill-down route, so both this service and app-shell's
 * rail-hide gate share this single predicate. Keep the two in lockstep — a
 * new drill-down route added here is also hidden in app-shell.
 *
 * Notes (`/notes` home + `/notes/:id` editor, Stage 1, 2026-07-12) and
 * Meetings/meeting-detail (`/library`, `/meeting/:id`, Stage 2, same day) are
 * DELIBERATELY NOT drill-downs — Notion/Obsidian-style always-visible
 * sidebar: the primary rail now hosts BOTH folder trees directly
 * (`NotesSidebarTreeComponent`, `MeetingsSidebarTreeComponent`), so
 * `NotesHomeComponent`/`NoteEditorComponent`/`LibraryComponent` render as
 * normal in-flow content beside the sidebar, like `/record`, with no local
 * back-affordance (`DetailComponent`, `/meeting/:id`, never had one to begin
 * with — its own "← Meetings" is a plain in-page breadcrumb link, not the
 * rail-hiding drill-down chrome, so it needed no structural change here).
 *
 * `/settings` and the org-item viewer (`/org-item`, reached from an org card
 * inside Notes) are STILL drill-downs for now — each retains its own
 * "← Murmur" back button and hides the primary rail, until a later stage
 * folds them into the same model too.
 */
export function isDrilldownRoute(url: string): boolean {
  return url.startsWith("/settings") || url.startsWith("/org-item");
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
