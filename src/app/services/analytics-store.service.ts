import { Injectable, signal } from "@angular/core";
import type { Analytics } from "../core/models";

/**
 * Root-persisted backing signals for {@link AnalyticsComponent} — split out
 * from the component itself so the DATA survives a destroy+recreate (leaving
 * `/analytics` for another tab, then coming back): a component-local
 * `signal<Analytics | null>(null)` is wiped to `null` on every remount,
 * forcing a full "Loading…" flash even though the numbers barely changed.
 * `/analytics` is NOT one of the three routes `TabRouteReuseStrategy` keeps
 * alive (`meeting/:id` / `notes/:id` / `org-item/:id` — see its doc), so this
 * component is genuinely destroyed and recreated on every navigate-away and
 * back. A root service instance outlives the component, so the dashboard
 * renders with the LAST-KNOWN numbers INSTANTLY on return while
 * `AnalyticsComponent.ngOnInit`'s existing reload (unchanged — still a real
 * refetch every visit) quietly replaces it underneath.
 *
 * Deliberately a thin signal holder, NOT a service with its own load()
 * method: `AnalyticsComponent` keeps owning the fetch — it just reads/writes
 * THESE signals instead of component-local ones. Mirrors `MeetingsListStore`
 * (see `angular-zoneless.md` §8).
 */
@Injectable({ providedIn: "root" })
export class AnalyticsStore {
  readonly data = signal<Analytics | null>(null);
  readonly loading = signal(true);
}
