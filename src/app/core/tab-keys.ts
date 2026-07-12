/**
 * Shared tab-identity key helpers — used by BOTH `TabsService` (the open-tab
 * list) and `TabRouteReuseStrategy` (the router-native detach/reattach cache),
 * kept in a tiny neutral module so neither one depends on the other. A tab's
 * `id` and its route-reuse cache key are DELIBERATELY the same string, so
 * closing a tab (`TabsService.closeTab`) can evict the exact cached instance
 * (`TabRouteReuseStrategy.evict`) with no translation step.
 */

/** The two "document" tab kinds Murmur supports (browser-tab scope: meetings + notes). */
export type TabKind = "meeting" | "note";

/**
 * The stable identity key for a meeting or note tab. Note the route-segment
 * asymmetry is intentional: meetings live at `/meeting/:id` (singular) and
 * notes at `/notes/:id` (plural) — the key mirrors each route's own path.
 */
export function tabKeyFor(kind: TabKind, entityId: string): string {
  return kind === "meeting" ? `meeting:${entityId}` : `notes:${entityId}`;
}
