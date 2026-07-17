import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  input,
  signal,
} from "@angular/core";
import { RouterLink } from "@angular/router";

import { IpcService } from "../../core/ipc.service";
import type { LinkEdge, LinkKind } from "../../core/models";
import { FoldersService } from "../../services/folders.service";

/** One rendered semantic-suggestion confidence tier (chip color + label). */
type ConfidenceTier = "high" | "med" | "low";

/**
 * Brain v3 PR-3 "Connections" — a self-contained panel of the persisted link
 * edges incident on one item (`list_links(kind, id)`), shown BESIDE the existing
 * "Linked mentions" backlinks chip row in the meeting Note tab and the note
 * editor. It renders two groups:
 *
 *  - DETERMINISTIC edges (`wikilink` / `companion`) as plain link chips that route
 *    to the neighbour (`meeting` → `/meeting/:id`, `note`/`document` → `/notes/:id`),
 *    mirroring the `app-backlinks` chip visual language.
 *  - SEMANTIC suggestions (`status === "suggested"`) as rows carrying a 3-tier
 *    confidence chip (score ≥ 0.88 high / ≥ 0.84 med / ≥ 0.80 low) plus Accept /
 *    Dismiss buttons. Accept flips the edge active (and materializes the
 *    `[[Title]]` server-side); Dismiss tombstones it. Both re-fetch on success.
 *
 * SELF-LOADING (unlike the presentational `app-backlinks`, whose host owns the
 * fetch): it takes `kind` + `id` + `locked` inputs and owns the gated fetch, so a
 * host only drops the tag in. The fetch is skipped/cleared while `locked` is true
 * (never surface connections behind a lock) and re-runs on a session lock-tree
 * change (a folder unlock/relock, or screen-share relock-all) so sealed neighbours
 * drop out — or reappear — live, exactly like `graph.component`'s `_refetchOnLock`.
 */
@Component({
  selector: "app-connections",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  templateUrl: "./connections.component.html",
  styleUrl: "./connections.component.scss",
})
export class ConnectionsComponent {
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);

  /** The link-endpoint kind this panel is anchored to. */
  readonly kind = input.required<LinkKind>();
  /** The anchored item's id; changing it re-loads the panel. */
  readonly id = input.required<string>();
  /**
   * Whether the anchored item is currently locked/masked. While true the fetch
   * is skipped and the edges are cleared — connections are never surfaced behind
   * a lock (belt-and-braces with the backend's own both-endpoint visibility gate).
   */
  readonly locked = input(false);

  /** The visible edges for the current `(kind, id)`; `[]` while locked/loading. */
  readonly edges = signal<LinkEdge[]>([]);
  /** In-flight Accept/Dismiss link ids — disables that row's buttons meanwhile. */
  private readonly pending = signal<ReadonlySet<number>>(new Set());

  /**
   * Monotonic request token — a late `list_links` reply for a superseded
   * `(kind, id)` or lock transition is dropped (stale-result guard, FE failure
   * mode #4), the same idiom as `entity-detail`'s `_load` / `detail`'s backlinks.
   */
  private seq = 0;

  /** Deterministic edges (`wikilink` / `companion`) → plain link chips. */
  readonly deterministic = computed(() =>
    this.edges().filter((e) => e.edgeType !== "semantic"),
  );

  /** Semantic suggestions (`status === "suggested"`) → Accept/Dismiss rows. */
  readonly suggestions = computed(() =>
    this.edges().filter(
      (e) => e.edgeType === "semantic" && e.status === "suggested",
    ),
  );

  /**
   * Load the edges whenever `(kind, id)`, `locked`, OR the session lock-tree
   * changes. Reading `folders.tree()` registers this effect as its dependent so a
   * later unlock/relock re-asks the backend (sealed neighbours drop out / reappear
   * live). A legitimate signal-writing effect (T1): async IPC keyed on inputs with
   * a stale-result guard; the `seq` check drops a reply for a superseded item.
   */
  private readonly _load = effect(() => {
    const kind = this.kind();
    const id = this.id();
    const locked = this.locked();
    // Establish the lock-tree dependency (value unused — the backend is the
    // authority on visibility; we just re-ask when it may have changed).
    this.folders.tree();
    const seq = ++this.seq;
    if (locked || !id) {
      this.edges.set([]);
      return;
    }
    void this.fetch(kind, id, seq);
  });

  private async fetch(kind: LinkKind, id: string, seq: number): Promise<void> {
    try {
      const rows = await this.ipc.listLinks(kind, id);
      if (seq !== this.seq) {
        return; // superseded by a newer item / lock transition
      }
      this.edges.set(Array.isArray(rows) ? rows : []);
    } catch {
      if (seq === this.seq) {
        this.edges.set([]);
      }
    }
  }

  /** The confidence tier for a semantic suggestion's cosine score. */
  tier(score: number): ConfidenceTier {
    if (score >= 0.88) {
      return "high";
    }
    if (score >= 0.84) {
      return "med";
    }
    return "low";
  }

  /** Whole-number percentage label for a suggestion's confidence chip. */
  scoreLabel(score: number): string {
    return `${Math.round(score * 100)}%`;
  }

  /** True while an Accept/Dismiss for this edge is in flight (buttons disabled). */
  isPending(id: number): boolean {
    return this.pending().has(id);
  }

  /** The click-through route array for an edge's neighbour, split by kind. */
  routeFor(e: LinkEdge): unknown[] {
    return e.otherKind === "meeting"
      ? ["/meeting", e.otherId]
      : ["/notes", e.otherId];
  }

  /** Accept a suggestion → materialize server-side, then re-fetch to reflect it. */
  accept(e: LinkEdge): void {
    void this.mutate(e.id, () => this.ipc.acceptLink(e.id));
  }

  /** Dismiss a suggestion → tombstone server-side, then re-fetch to drop it. */
  dismiss(e: LinkEdge): void {
    void this.mutate(e.id, () => this.ipc.dismissLink(e.id));
  }

  /**
   * Run an Accept/Dismiss mutation guarded by the pending set, then re-fetch the
   * edges for the CURRENT `(kind, id)` (never a stale closure) so the panel reflects
   * the server truth. The seq token drops a reply for a since-changed item.
   */
  private async mutate(id: number, run: () => Promise<void>): Promise<void> {
    if (this.isPending(id)) {
      return;
    }
    this.pending.update((s) => new Set(s).add(id));
    try {
      await run();
      const seq = ++this.seq;
      await this.fetch(this.kind(), this.id(), seq);
    } catch {
      // Leave the current edges untouched; drop the pending flag below.
    } finally {
      this.pending.update((s) => {
        const next = new Set(s);
        next.delete(id);
        return next;
      });
    }
  }
}
