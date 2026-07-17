import { Injectable, inject } from "@angular/core";
import { IpcService } from "../core/ipc.service";
import type { LinkKind, SourceRef } from "../core/models";

/**
 * Source-scoped Brain — the ONE place the "sensible default scope for an anchor"
 * is computed, so every Ask surface (meeting @brain chat, the Ask-about-this-note
 * panel) pre-fills its `<mur-source-picker>` identically.
 *
 * The default scope for an anchor `(kind, id)` is:
 *   1. the anchor itself (`{kind, id, title}`), then
 *   2. every ACTIVE (`status: "active"`) neighbour from {@link IpcService.listLinks}
 *      — deterministic wikilink/companion/manual edges — mapped to the neighbour's
 *      `{kind: otherKind, id: otherId, title: otherTitle}`.
 * The list is deduped by `kind + id` (the same identity `SourceRef` uses) with the
 * anchor kept first. `suggested` (un-accepted semantic) edges are deliberately
 * EXCLUDED — a default scope should only include links the user has confirmed.
 *
 * `listLinks` is visibility-gated server-side (a sealed queried item yields `[]`,
 * a sealed neighbour is dropped), so the default never leaks a locked source.
 */
@Injectable({ providedIn: "root" })
export class SourceScopeService {
  private readonly ipc = inject(IpcService);

  /**
   * The default source scope for an anchor: the anchor itself + its ACTIVE linked
   * neighbours, deduped. `title` is the display label for the picker's chips.
   *
   * Best-effort: if `listLinks` rejects (e.g. no Tauri, a transient error) this
   * still resolves with JUST the anchor, so a picker always pre-fills with at
   * least the current item. The CALLER owns the stale-result guard (drop a late
   * reply when its `kind`/`id` has changed since the fetch began).
   */
  async defaultSources(
    kind: LinkKind,
    id: string,
    title?: string,
  ): Promise<SourceRef[]> {
    const anchor: SourceRef = { kind, id, title };
    const out: SourceRef[] = [anchor];
    const seen = new Set<string>([kind + id]);
    try {
      const edges = await this.ipc.listLinks(kind, id);
      for (const e of edges) {
        if (e.status !== "active") {
          continue; // only confirmed links seed the default scope.
        }
        const key = e.otherKind + e.otherId;
        if (seen.has(key)) {
          continue; // dedupe by kind + id (anchor + a repeated edge).
        }
        seen.add(key);
        out.push({ kind: e.otherKind, id: e.otherId, title: e.otherTitle });
      }
    } catch {
      // Best-effort: fall back to just the anchor.
    }
    return out;
  }
}
