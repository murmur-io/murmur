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
import type {
  BacklinkSource,
  LinkEdge,
  LinkKind,
  NoteCitation,
} from "../../core/models";
import { FoldersService } from "../../services/folders.service";
import { ToastService } from "../../services/toast.service";
import { LinkPickerComponent } from "../../features/notes/link-picker/link-picker.component";

/** The small direction/type tag shown on a Related chip. */
type RelationTag = "linked" | "mentions" | "companion" | "related";

/**
 * ONE merged, deduped "Related" row (2026-07-19 IA consolidation). A single
 * neighbour — whether it arrived as an outbound/incident link edge
 * (`list_links`), an inbound backlink (`get_backlinks`), or BOTH — renders as
 * exactly one chip here. The deterministic `edge` (when present) drives the
 * removable `×`; a backlink-only neighbour has no removable edge. All per-row
 * values (route / tag / removable / pending) are resolved ONCE in a `computed`
 * so the template binds fields only (the zoneless computed-only rule).
 */
interface RelatedRow {
  /** Dedup identity — `${otherKind}:${otherId}`. Also the `@for` track key. */
  readonly key: string;
  readonly kind: "meeting" | "note" | "document";
  readonly title: string;
  readonly route: unknown[];
  /** The small direction/type tag ("linked" / "mentions" / "companion" / "related"). */
  readonly tag: RelationTag;
  /** True when this row is a user-created MANUAL link that shows a removable hover `×`. */
  readonly removable: boolean;
  /** True while an unlink for this row's edge is in flight. */
  readonly pending: boolean;
  /** The removable MANUAL edge behind this row (present only when `removable`). */
  readonly edge: LinkEdge | null;
}

/**
 * One ambient SEMANTIC-suggestion chip (2026-07-19): "related, not yet linked".
 * Rendered as a dashed chip — the chip BODY promotes (`acceptLink` materializes
 * the `[[wikilink]]`), a hover `×` dismisses (`dismissLink`). No raw confidence
 * `%`, no persistent Accept/Dismiss buttons (the chip IS the affordance).
 */
interface SuggestionRow {
  readonly edge: LinkEdge;
  readonly kind: "meeting" | "note" | "document";
  readonly title: string;
  readonly route: unknown[];
  readonly pending: boolean;
}

/**
 * "Related" (Brain v3 PR-3, IA-consolidated 2026-07-19) — a self-contained,
 * gated panel of every relationship incident on one item, collapsed by default
 * behind an "N related" count. It MERGES two backend reads into ONE deduped
 * chip list:
 *
 *  - the persisted link edges (`list_links(kind, id)`) — deterministic
 *    `wikilink`/`companion`/`manual` (+ accepted semantic) chips, AND
 *  - the inbound backlinks (`get_backlinks(kind, id)`) — the meetings/notes
 *    that mention this item.
 *
 * A neighbour that appears in BOTH (an inbound wikilink) renders ONCE, keyed on
 * `(otherKind, otherId)`, with a small direction/type tag. Semantic SUGGESTIONS
 * (`status === "suggested"`) render as ambient dashed chips inside the same
 * section — tap promotes (`acceptLink`), hover `×` dismisses (`dismissLink`),
 * no `%`, no two-button rows. Manual chips keep their hover-`×` unlink.
 *
 * SELF-LOADING & GATED: it takes `kind` + `id` + `locked` and owns BOTH gated
 * fetches, skipped/cleared while `locked` is true (never surface relationships
 * behind a lock) and re-run on a session lock-tree change (a folder
 * unlock/relock, screen-share relock-all) so sealed neighbours drop out — or
 * reappear — live, exactly like `graph.component`'s `_refetchOnLock`. A late
 * reply for a superseded `(kind, id)` / lock transition is dropped (stale
 * guard). The optional `inlineWikilinkTitles` input suppresses a `wikilink`-edge
 * chip whose title is ALREADY materialized inline in the note body (no visual
 * triplication with the inline chip).
 *
 * PR-1 (user-initiated linking) keeps two write affordances: the `+ Link`
 * header chooser (`link_items` via the shared opaque {@link LinkPickerComponent})
 * and the hover-`×` unlink on a MANUAL chip (`unlink_items`). Both are gated by
 * the same pending set and surface failures via the {@link ToastService}.
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
   * Whether the anchored item is currently locked/masked. While true both
   * fetches are skipped and everything is cleared — relationships are never
   * surfaced behind a lock (belt-and-braces with the backend's own
   * both-endpoint visibility gate).
   */
  readonly locked = input(false);
  /**
   * Titles of neighbours the note BODY already links inline via `[[Title]]`
   * (2026-07-19, optional item 4). A `wikilink`-edge row whose title is in this
   * set is suppressed from the Related list so the inline chip in the body isn't
   * triplicated (inline chip + Related chip). Case-insensitive. Empty by default
   * ⇒ no suppression (the routed meeting Note tab has no body text to feed).
   * Only `wikilink` edges are filtered — a manual/companion/backlink neighbour is
   * a distinct relationship worth showing even if also linked inline.
   */
  readonly inlineWikilinkTitles = input<string[]>([]);

  /** The visible edges for the current `(kind, id)`; `[]` while locked/loading. */
  readonly edges = signal<LinkEdge[]>([]);
  /** The visible inbound backlinks for the current `(kind, id)`; `[]` while locked/loading. */
  readonly backlinksIn = signal<BacklinkSource[]>([]);
  /**
   * In-flight Accept/Dismiss/Unlink edge ids — disables that row's `×`/tap
   * meanwhile. Keyed by `LinkEdge.id`.
   */
  private readonly pending = signal<ReadonlySet<number>>(new Set());

  /** Whether the whole Related section is expanded (Notion-style collapse-by-default). */
  readonly expanded = signal(false);

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
   * Monotonic request token — a late `list_links`/`get_backlinks` reply for a
   * superseded `(kind, id)` or lock transition is dropped (stale-result guard, FE
   * failure mode #4), the same idiom as `entity-detail`'s `_load`.
   */
  private seq = 0;

  /**
   * DETERMINISTIC edges only — everything that is NOT a pending semantic suggestion:
   * `wikilink`/`companion`/`manual`, any `manual`-flagged chip, AND an ACCEPTED
   * semantic edge (`status !== "suggested"`). These become "linked"/"companion"/
   * "related" Related rows.
   */
  private readonly deterministic = computed(() =>
    this.edges().filter(
      (e) => !(e.edgeType === "semantic" && e.status === "suggested" && !e.manual),
    ),
  );

  /**
   * Semantic suggestions (`status === "suggested"`, not `manual`) → ambient
   * dashed chips. A `manual`-flagged chip is EXCLUDED (a user already actively
   * linked that pair — it renders as a removable Related chip, never an
   * unconfirmed suggestion).
   */
  private readonly semanticSuggestions = computed(() =>
    this.edges().filter(
      (e) => e.edgeType === "semantic" && e.status === "suggested" && !e.manual,
    ),
  );

  /**
   * The MERGED, DEDUPED "Related" rows (2026-07-19). Inbound backlinks and
   * incident deterministic edges are reconciled by `(otherKind, otherId)` so a
   * neighbour that is BOTH shows ONCE. Priority when both exist: the link EDGE
   * wins (it carries the removable/typed relationship); a backlink-only neighbour
   * becomes a "mentions" row. Ordering: deterministic edges first (companion >
   * manual/wikilink > accepted-semantic, mirroring the old strongest-first
   * intent), then backlink-only mentions. Depends on `edges()`, `backlinksIn()`,
   * `pending()`, and `inlineWikilinkTitles()`.
   */
  readonly relatedRows = computed<RelatedRow[]>(() => {
    const pending = this.pending();
    const inline = new Set(
      this.inlineWikilinkTitles().map((t) => t.trim().toLowerCase()),
    );
    const seen = new Set<string>();
    const rows: RelatedRow[] = [];

    // 1) Deterministic edges → typed/removable rows (strongest identity first).
    for (const edge of this.orderedDeterministic()) {
      // Suppress a wikilink chip already materialized inline in the body (item 4).
      if (
        edge.edgeType === "wikilink" &&
        !edge.manual &&
        inline.has(edge.otherTitle.trim().toLowerCase())
      ) {
        continue;
      }
      const key = `${edge.otherKind}:${edge.otherId}`;
      if (seen.has(key)) {
        continue;
      }
      seen.add(key);
      rows.push({
        key,
        kind: edge.otherKind,
        title: edge.otherTitle,
        route: this.routeForKind(edge.otherKind, edge.otherId),
        tag: this.tagForEdge(edge),
        removable: edge.manual === true,
        pending: pending.has(edge.id),
        edge: edge.manual === true ? edge : null,
      });
    }

    // 2) Inbound backlinks with no matching edge → "mentions" rows.
    for (const bl of this.backlinksIn()) {
      const key = `${bl.kind}:${bl.id}`;
      if (seen.has(key)) {
        continue; // already shown via an incident edge
      }
      seen.add(key);
      rows.push({
        key,
        kind: bl.kind,
        title: bl.title,
        route: this.routeForKind(bl.kind, bl.id),
        tag: "mentions",
        removable: false,
        pending: false,
        edge: null,
      });
    }

    return rows;
  });

  /**
   * The deterministic edges in stable display order: `companion` first, then
   * `manual`/`wikilink` links, then accepted-semantic — a modest strongest-first
   * ordering so the most explicit relationships lead. A pure sort over the
   * `deterministic()` set (no side effects).
   */
  private readonly orderedDeterministic = computed<LinkEdge[]>(() => {
    const weight = (e: LinkEdge): number => {
      if (e.edgeType === "companion") {
        return 0;
      }
      if (e.manual || e.edgeType === "manual" || e.edgeType === "wikilink") {
        return 1;
      }
      return 2; // accepted semantic (or anything else)
    };
    return [...this.deterministic()].sort((a, b) => weight(a) - weight(b));
  });

  /**
   * The `(kind:id)` keys already present as a Related row — used to drop a semantic
   * suggestion whose neighbour is ALSO an inbound/outbound relationship. Without
   * this the SAME note renders in BOTH "Related" and "Suggested" (the exact
   * duplication the consolidation set out to kill — a suggestion is only ever
   * "related, not yet linked", never a repeat of something already linked/mentioned).
   */
  private readonly relatedKeys = computed(
    () => new Set(this.relatedRows().map((r) => r.key)),
  );

  /**
   * The ambient suggestion chips (strongest-first), EXCLUDING any neighbour already
   * shown as a Related row (cross-surface dedup).
   */
  readonly suggestionRows = computed<SuggestionRow[]>(() => {
    const pending = this.pending();
    const related = this.relatedKeys();
    return [...this.semanticSuggestions()]
      .filter((e) => !related.has(`${e.otherKind}:${e.otherId}`))
      .sort((a, b) => b.score - a.score)
      .map((edge) => ({
        edge,
        kind: edge.otherKind,
        title: edge.otherTitle,
        route: this.routeForKind(edge.otherKind, edge.otherId),
        pending: pending.has(edge.id),
      }));
  });

  /** The Related-row count (the "linked"/"mentions" chips). */
  readonly relatedCount = computed(() => this.relatedRows().length);

  /**
   * The whole-section count behind the collapsed "Related · N" affordance: Related
   * rows + (deduped) suggestions. The section is COLLAPSED BY DEFAULT (Notion-style,
   * near-zero footprint above the note body); `expanded` reveals both groups.
   */
  readonly totalCount = computed(
    () => this.relatedCount() + this.suggestionRows().length,
  );

  /**
   * Whether the whole section renders at all: hide entirely when there are zero
   * related rows AND zero suggestions (the panel is auto-hidden when empty — the
   * near-zero-footprint "Related" IA). The `+ Link` chooser lives inside the
   * section header, so hiding when empty means a brand-new item shows no
   * relationship UI until it has one; that's the intended minimalist default (a
   * user reaches linking via the note body `[[` / slash menu instead).
   */
  readonly hasAnything = computed(
    () => this.relatedCount() > 0 || this.suggestionRows().length > 0,
  );

  /**
   * Load BOTH the edges and the inbound backlinks whenever `(kind, id)`,
   * `locked`, OR the session lock-tree changes. Reading `folders.tree()`
   * registers this effect as its dependent so a later unlock/relock re-asks the
   * backend (sealed neighbours drop out / reappear live). A legitimate
   * signal-writing effect (T1): async IPC keyed on inputs with a stale-result
   * guard; the `seq` check drops a reply for a superseded item.
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
      this.backlinksIn.set([]);
      return;
    }
    void this.fetch(kind, id, seq);
  });

  /**
   * Fetch the edges AND the inbound backlinks in parallel, dropping a reply for a
   * superseded `(kind, id)` / lock transition. `get_backlinks` takes a
   * `SourceKind` (`meeting | note`), so a `document`-anchored panel (which can't
   * have note-backlinks) skips the backlink read entirely.
   */
  private async fetch(kind: LinkKind, id: string, seq: number): Promise<void> {
    // `get_backlinks` takes a `SourceKind` (`meeting | note`), so a
    // `document`-anchored panel (which can't have note-backlinks) skips it.
    const backlinksP: Promise<BacklinkSource[]> =
      kind === "meeting" || kind === "note"
        ? this.ipc.getBacklinks(kind, id).catch(() => [] as BacklinkSource[])
        : Promise.resolve([] as BacklinkSource[]);
    try {
      const [rows, backlinks] = await Promise.all([
        this.ipc.listLinks(kind, id),
        backlinksP,
      ]);
      if (seq !== this.seq) {
        return; // superseded by a newer item / lock transition
      }
      this.edges.set(Array.isArray(rows) ? rows : []);
      this.backlinksIn.set(Array.isArray(backlinks) ? backlinks : []);
    } catch {
      if (seq === this.seq) {
        this.edges.set([]);
        this.backlinksIn.set([]);
      }
    }
  }

  /** The direction/type tag for a deterministic edge. */
  private tagForEdge(e: LinkEdge): RelationTag {
    if (e.edgeType === "companion") {
      return "companion";
    }
    if (e.manual || e.edgeType === "manual" || e.edgeType === "wikilink") {
      return "linked";
    }
    return "related"; // accepted semantic
  }

  /** True while an Accept/Dismiss/Unlink for this edge is in flight (guards `mutate`). */
  private isPending(id: number): boolean {
    return this.pending().has(id);
  }

  /** The click-through route array for a neighbour, split by kind. */
  private routeForKind(
    kind: "meeting" | "note" | "document",
    id: string,
  ): unknown[] {
    return kind === "meeting" ? ["/meeting", id] : ["/notes", id];
  }

  /** Toggle the whole Related section collapsed ↔ expanded. */
  toggleExpanded(): void {
    this.expanded.update((v) => !v);
  }

  /** Promote a suggestion → materialize server-side, then re-fetch to reflect it. */
  accept(e: LinkEdge): void {
    void this.mutate(e.id, () => this.ipc.acceptLink(e.id));
  }

  /** Dismiss a suggestion → tombstone server-side, then re-fetch to drop it. */
  dismiss(e: LinkEdge): void {
    void this.mutate(e.id, () => this.ipc.dismissLink(e.id));
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
   * the edges + backlinks for the CURRENT `(kind, id)` (never a stale closure) so the
   * panel reflects the server truth. The seq token drops a reply for a since-changed
   * item. A non-null `errorMsg` surfaces a toast on failure (used by Unlink; Accept/
   * Dismiss stay silent, matching their prior behavior).
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
      // Distinguish the two refusals so the toast is honest (the anchor is now excluded from the
      // candidate list, so the self-case is belt-and-braces — but a stale/edge pick must still read
      // right, not the generic "wrong type" message it used to show for a self-link).
      const isSelf =
        candidate.kind === this.kind() && candidate.id === this.id();
      this.toast.info(
        isSelf
          ? "You can't link an item to itself."
          : "You can only link meetings, notes, and documents.",
      );
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
