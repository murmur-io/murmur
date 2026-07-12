import { Injectable } from "@angular/core";
import {
  type ActivatedRouteSnapshot,
  type DetachedRouteHandle,
  RouteReuseStrategy,
  destroyDetachedRouteHandle,
} from "@angular/router";
import { tabKeyFor } from "./tab-keys";

/**
 * The router-native tab cache for Murmur's "document" routes (`/meeting/:id`,
 * `/notes/:id`) — browser-style tabs (see `tabs.service.ts`) need each open
 * meeting/note to keep its own live component instance (scroll position,
 * in-progress edits, audio playback) while backgrounded, and to switch back to
 * it instantly. Angular's DEFAULT `RouteReuseStrategy` only compares
 * `routeConfig` (not params), so `/meeting/A` → `/meeting/B` already silently
 * REUSES the same component instance — which is also what forced
 * `DetailComponent` to carry a manual "reload in place" workaround before this
 * strategy existed.
 *
 * Only `meeting/:id` and `notes/:id` are in scope — every other route falls
 * through to Angular's default `routeConfig`-only comparison, so unrelated
 * routes behave exactly as before.
 *
 * DELIBERATELY OUT OF SCOPE: `notes/new`. It always transitions onward to a
 * real `/notes/:id` (via `NoteEditorComponent.createAndOpen`'s `replaceUrl`
 * navigate) or is abandoned outright — it is never itself a destination worth
 * caching. If it WERE in scope, leaving it would detach-and-store a handle
 * keyed `notes:new` that nothing ever `shouldAttach`s back onto (the next
 * `/notes/new` visit is a DIFFERENT creation, not a return to this one) — a
 * guaranteed one-detached-instance-per-click leak, and worse, a wrong
 * reattach if TWO "new note" flows ever raced. Falling through to the default
 * strategy for this one config makes the create-then-`replaceUrl` transition
 * a plain destroy+recreate, exactly its pre-existing (already safe — the
 * editor body is `loading()`-gated, so nothing typed can be lost) behavior.
 */
function tabKey(route: ActivatedRouteSnapshot): string | null {
  const path = route.routeConfig?.path;
  if (path === "meeting/:id") {
    const id = route.paramMap.get("id");
    return id ? tabKeyFor("meeting", id) : null;
  }
  if (path === "notes/:id") {
    const id = route.paramMap.get("id");
    return id ? tabKeyFor("note", id) : null;
  }
  return null;
}

@Injectable({ providedIn: "root" })
export class TabRouteReuseStrategy extends RouteReuseStrategy {
  /** Detached-but-alive component handles, keyed by `tabKeyFor(...)`. */
  private readonly cache = new Map<string, DetachedRouteHandle>();

  override shouldDetach(route: ActivatedRouteSnapshot): boolean {
    return tabKey(route) !== null;
  }

  override store(
    route: ActivatedRouteSnapshot,
    handle: DetachedRouteHandle | null,
  ): void {
    const key = tabKey(route);
    if (!key) {
      return;
    }
    if (handle) {
      this.cache.set(key, handle);
    } else {
      // Per the RouteReuseStrategy contract, storing `null` erases the entry.
      this.cache.delete(key);
    }
  }

  override shouldAttach(route: ActivatedRouteSnapshot): boolean {
    const key = tabKey(route);
    return key !== null && this.cache.has(key);
  }

  override retrieve(route: ActivatedRouteSnapshot): DetachedRouteHandle | null {
    const key = tabKey(route);
    return key ? this.cache.get(key) ?? null : null;
  }

  override shouldReuseRoute(
    future: ActivatedRouteSnapshot,
    curr: ActivatedRouteSnapshot,
  ): boolean {
    const futureKey = tabKey(future);
    const currKey = tabKey(curr);
    if (futureKey !== null || currKey !== null) {
      // In-scope: reuse ONLY when the resolved identity (config + params) is
      // the exact same tab — `/meeting/A` -> `/meeting/B` is a DIFFERENT
      // instance, unlike Angular's default `routeConfig`-only comparison.
      return futureKey !== null && futureKey === currKey;
    }
    // Out of scope — fall back to Angular's own default behavior.
    return future.routeConfig === curr.routeConfig;
  }

  /**
   * Explicitly destroy and drop a cached detached instance. Called by
   * `TabsService.closeTab` — without this, a closed tab's component would
   * stay detached in `cache` forever (a real leak: its effects/signals keep
   * running off-DOM).
   */
  evict(key: string): void {
    const handle = this.cache.get(key);
    if (handle) {
      destroyDetachedRouteHandle(handle);
      this.cache.delete(key);
    }
  }
}
