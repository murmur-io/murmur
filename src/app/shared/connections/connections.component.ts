import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  effect,
  inject,
  input,
  signal,
  viewChild,
} from "@angular/core";
import { RouterLink } from "@angular/router";

import { IpcService } from "../../core/ipc.service";
import type { LinkEdge, LinkKind, NoteCitation } from "../../core/models";
import { FoldersService } from "../../services/folders.service";
import { ToastService } from "../../services/toast.service";
import { LinkPickerComponent } from "../../features/notes/link-picker/link-picker.component";

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
 *
 * PR-1 (user-initiated linking) adds two write affordances over the same edges:
 *  - a `+ Link` header control that opens the single-pick {@link LinkPickerComponent}
 *    (the SAME opaque, paginated autocomplete the note editor uses) filtered to
 *    `meeting | note | document`; on pick it calls `link_items(anchor → candidate)`
 *    and re-fetches. This panel OWNS the picker's `query` / `activeIndex` / keyboard
 *    nav (the picker is presentational) via a small header `<input>`.
 *  - a hover `×` on each DETERMINISTIC chip whose edge is `manual === true`
 *    (a user-created link), which calls `unlink_items(anchor → neighbour)` and
 *    re-fetches. Non-manual chips (auto wikilink/companion, semantic) get NO `×`.
 * Both writes are gated by the same pending set as Accept/Dismiss and surface a
 * failure through the {@link ToastService} rather than leaving a stuck spinner.
 */
@Component({
  selector: "app-connections",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink, LinkPickerComponent],
  templateUrl: "./connections.component.html",
  styleUrl: "./connections.component.scss",
})
export class ConnectionsComponent {
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);
  private readonly toast = inject(ToastService);
  private readonly injector = inject(Injector);

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
  /**
   * In-flight Accept/Dismiss/Unlink edge ids — disables that row's buttons (and its
   * `×`) meanwhile. Keyed by `LinkEdge.id` (unlink guards on the manual chip's id).
   */
  private readonly pending = signal<ReadonlySet<number>>(new Set());

  /** The single-pick `+ Link` chooser element (the header `<input>`) for anchoring. */
  private readonly pickerAnchor =
    viewChild<ElementRef<HTMLElement>>("pickerAnchor");

  /** Whether the `+ Link` picker is open. */
  readonly pickerOpen = signal(false);
  /** True while a `link_items` call from a pick is in flight (disables the chooser). */
  readonly linking = signal(false);
  /** The picker's live filter text (this panel owns it — the picker is presentational). */
  readonly pickerQuery = signal("");
  /** Keyboard-highlighted row in the open picker (↑/↓). */
  readonly pickerActiveIndex = signal(0);
  /** The picker's resolved candidates for the current query (drives keyboard nav). */
  readonly pickerCandidates = signal<NoteCitation[]>([]);
  /**
   * The chooser's viewport anchor rect for the popover. A NEW object re-positions
   * it; recomputed from the header `<input>` on open and on every reposition tick.
   */
  readonly pickerRect = signal<{
    top: number;
    left: number;
    right: number;
    bottom: number;
  }>({ top: 0, left: 0, right: 0, bottom: 0 });

  /**
   * A candidate kind that is NOT a valid `link_items` endpoint (a {@link LinkKind}
   * is `meeting | note | document`). `list_link_candidates` may surface Shared-Brain
   * `org` rows (and, in principle, `person`/`entity` from the shared shape); those
   * are NOT linkable, so a pick is refused with a toast rather than silently
   * dropped — the picker renders its own rows, so this guards them at pick time.
   */
  private static readonly NON_LINKABLE_KINDS: ReadonlySet<NoteCitation["kind"]> =
    new Set<NoteCitation["kind"]>(["person", "entity", "org"]);

  /** True when a candidate is a linkable meeting/note/document endpoint (not org/person/entity, not self). */
  private isLinkable(c: NoteCitation): boolean {
    return (
      !ConnectionsComponent.NON_LINKABLE_KINDS.has(c.kind) &&
      !(c.kind === this.kind() && c.id === this.id())
    );
  }

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
   * PR-1 — whether a deterministic chip is USER-REMOVABLE (shows the hover `×`).
   * Only manual links (`manual === true`, set by the backend on the deduped
   * manual+wikilink chip) are removable here; auto wikilink/companion edges are not.
   */
  isRemovable(e: LinkEdge): boolean {
    return e.manual === true;
  }

  /**
   * PR-1 — remove a user-created link: `unlink_items(anchor → neighbour)`, then
   * re-fetch so the chip drops out. Guarded by the same pending set (keyed on the
   * chip's edge id) so its `×` disables mid-flight. Surfaces a failure via a toast.
   */
  unlink(e: LinkEdge): void {
    void this.mutate(
      e.id,
      () => this.ipc.unlinkItems(this.kind(), this.id(), e.otherKind, e.otherId),
      "Couldn't remove the link.",
    );
  }

  /**
   * Run an Accept/Dismiss/Unlink mutation guarded by the pending set, then re-fetch
   * the edges for the CURRENT `(kind, id)` (never a stale closure) so the panel
   * reflects the server truth. The seq token drops a reply for a since-changed item.
   * A non-null `errorMsg` surfaces a toast on failure (used by Unlink; Accept/Dismiss
   * stay silent, matching their prior behavior).
   */
  private async mutate(
    id: number,
    run: () => Promise<void>,
    errorMsg?: string,
  ): Promise<void> {
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
      if (errorMsg) {
        this.toast.danger(errorMsg);
      }
    } finally {
      this.pending.update((s) => {
        const next = new Set(s);
        next.delete(id);
        return next;
      });
    }
  }

  // --- `+ Link` chooser (reuses the note editor's LinkPickerComponent) --------

  /**
   * Open the single-pick `+ Link` chooser: anchor the popover under the header
   * `<input>`, reset its query/keyboard state. The picker is presentational, so
   * THIS panel owns `pickerQuery` / `pickerActiveIndex` and feeds the picker an
   * `anchorRect`; the header input is the query field + keyboard target.
   */
  openPicker(): void {
    if (this.linking()) {
      return;
    }
    this.pickerQuery.set("");
    this.pickerActiveIndex.set(0);
    this.pickerCandidates.set([]);
    this.pickerOpen.set(true);
    this.repositionPicker();
    // Focus the query field once it's in the DOM (zoneless-safe, no setTimeout).
    afterNextRender(() => this.pickerAnchor()?.nativeElement.focus?.(), {
      injector: this.injector,
    });
  }

  /** Close the chooser without linking anything (Esc / outside / after a pick). */
  closePicker(): void {
    this.pickerOpen.set(false);
    this.pickerQuery.set("");
    this.pickerCandidates.set([]);
    this.pickerActiveIndex.set(0);
  }

  /** The chooser query changed (input event) → refresh the anchored query + reset selection. */
  onQueryInput(event: Event): void {
    const value = (event.target as HTMLInputElement).value;
    this.pickerQuery.set(value);
    this.pickerActiveIndex.set(0);
  }

  /**
   * The picker's resolved candidates for the current query (drives keyboard nav).
   * The picker renders + click-emits its OWN rows, so `pickerActiveIndex` indexes
   * THIS same list (kept in lockstep with what the popover shows).
   */
  onPickerCandidates(rows: NoteCitation[]): void {
    this.pickerCandidates.set(rows);
    // A shrunk list can leave the highlight past the end — clamp it.
    const max = rows.length - 1;
    if (this.pickerActiveIndex() > max) {
      this.pickerActiveIndex.set(Math.max(0, max));
    }
  }

  /** ↑/↓/Enter/Esc while the chooser input has focus (nav over the picker's rows). */
  onQueryKey(event: KeyboardEvent): void {
    if (!this.pickerOpen()) {
      return;
    }
    const rows = this.pickerCandidates();
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        if (rows.length > 0) {
          this.pickerActiveIndex.update((i) => (i + 1) % rows.length);
        }
        break;
      case "ArrowUp":
        event.preventDefault();
        if (rows.length > 0) {
          this.pickerActiveIndex.update(
            (i) => (i - 1 + rows.length) % rows.length,
          );
        }
        break;
      case "Enter":
        event.preventDefault();
        if (rows.length > 0) {
          this.pickCandidate(rows[this.pickerActiveIndex()]);
        }
        break;
      case "Escape":
        event.preventDefault();
        this.closePicker();
        break;
    }
  }

  /**
   * A candidate was picked (click or Enter): create the link from this panel's
   * anchor `(kind, id)` → the picked candidate, then re-fetch so the new chip
   * appears. The picker renders its own rows, so a non-linkable kind (a Shared-Brain
   * `org` hit / `person` / `entity`, or the anchor itself) is REFUSED here with a
   * toast rather than sent to `link_items`. Guarded by `linking` (a stuck spinner is
   * impossible — cleared in `finally`); a failure surfaces a toast.
   */
  pickCandidate(candidate: NoteCitation): void {
    if (this.linking()) {
      return;
    }
    if (!this.isLinkable(candidate)) {
      this.toast.info("You can only link meetings, notes, and documents.");
      return;
    }
    const dstKind = candidate.kind as LinkKind;
    this.linking.set(true);
    this.closePicker();
    void (async () => {
      try {
        await this.ipc.linkItems(this.kind(), this.id(), dstKind, candidate.id);
        const seq = ++this.seq;
        await this.fetch(this.kind(), this.id(), seq);
      } catch {
        this.toast.danger("Couldn't create the link.");
      } finally {
        this.linking.set(false);
      }
    })();
  }

  /** Recompute the popover anchor rect from the header chooser input. */
  repositionPicker(): void {
    afterNextRender(
      () => {
        const el = this.pickerAnchor()?.nativeElement;
        if (!el) {
          return;
        }
        const r = el.getBoundingClientRect();
        this.pickerRect.set({
          top: r.top,
          left: r.left,
          right: r.right,
          bottom: r.bottom,
        });
      },
      { injector: this.injector },
    );
  }
}
