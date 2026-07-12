/**
 * Shared tab-identity key helpers — used by BOTH `TabsService` (the open-tab
 * list) and `TabRouteReuseStrategy` (the router-native detach/reattach cache),
 * kept in a tiny neutral module so neither one depends on the other. A tab's
 * `id` and its route-reuse cache key are DELIBERATELY the same string, so
 * closing a tab (`TabsService.closeTab`) can evict the exact cached instance
 * (`TabRouteReuseStrategy.evict`) with no translation step.
 */

/** The "document" tab kinds Murmur supports (browser-tab scope: meetings,
 * notes, and read-only org (Shared Brain) items — added 2026-07-12: an
 * org-shared note/meeting used to open in a full-page navigation instead of
 * a tab, unlike its owned counterparts). */
export type TabKind = "meeting" | "note" | "org-item";

/**
 * The stable identity key for a meeting, note, or org-item tab. Note the
 * route-segment asymmetry is intentional: meetings live at `/meeting/:id`
 * (singular) and notes at `/notes/:id` (plural) — the key mirrors each
 * route's own path. Org items live at `/org-item/:id`.
 */
export function tabKeyFor(kind: TabKind, entityId: string): string {
  if (kind === "meeting") {
    return `meeting:${entityId}`;
  }
  if (kind === "org-item") {
    return `org-item:${entityId}`;
  }
  return `notes:${entityId}`;
}
