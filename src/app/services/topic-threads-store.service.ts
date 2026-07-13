import { Injectable, signal } from "@angular/core";
import type { TopicThread } from "../core/models";

/**
 * Root-persisted backing signals for {@link TopicThreadsComponent} — split
 * out from the component itself so the DATA survives a destroy+recreate
 * (leaving `/analytics` for another tab, then coming back). `/analytics` is
 * NOT covered by `TabRouteReuseStrategy` (only `meeting/:id` / `notes/:id` /
 * `org-item/:id` are — see its doc), so this component — nested inside
 * `AnalyticsComponent` — is genuinely destroyed and recreated on every
 * navigate-away and back; a component-local `signal<TopicThread[]>([])`
 * would wipe to empty every time, forcing a "Loading…" flash. A root
 * service instance outlives the component, so the threads render with the
 * LAST-KNOWN rows INSTANTLY on return while `ngOnInit`'s existing reload
 * (unchanged — still a real refetch every visit) quietly replaces it
 * underneath.
 *
 * Deliberately a thin signal holder, NOT a service with its own load()
 * method: `TopicThreadsComponent` keeps owning the fetch — it just
 * reads/writes THESE signals instead of component-local ones. Mirrors
 * `MeetingsListStore` (see `angular-zoneless.md` §8).
 */
@Injectable({ providedIn: "root" })
export class TopicThreadsStore {
  readonly threads = signal<TopicThread[]>([]);
  readonly loading = signal(true);
  readonly error = signal<string | null>(null);
}
