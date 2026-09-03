import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  Injector,
  OnInit,
  afterNextRender,
  computed,
  effect,
  inject,
  input,
  signal,
  untracked,
  viewChild,
} from "@angular/core";
import { RouterLink } from "@angular/router";

import { IpcService } from "../../core/ipc.service";
import { AskHistoryPrivacyBarrierService } from "../../core/ask-history-privacy-barrier.service";
import { TabsService } from "../../core/tabs.service";
import type {
  BacklinkSource,
  ContainerLevel,
  LinkEdge,
  LinkKind,
} from "../../core/models";
import { DocumentPreviewService } from "../../services/document-preview.service";
import { FoldersService } from "../../services/folders.service";
import { ToastService } from "../../services/toast.service";
import {
  RelatedHierarchyPickerComponent,
  type PickerTarget,
} from "../related-hierarchy-picker/related-hierarchy-picker.component";

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
  readonly kind: LinkKind;
  /**
   * The neighbour's raw id (the edge's `otherId` / the backlink's `id`) — the
   * key `route` is built from, AND what a `document` chip passes to
   * {@link DocumentPreviewService.open} (a document has no route, so its chip
   * opens the read-only preview modal instead of navigating).
   */
  readonly id: string;
  /** Current live item id when `kind === "org"`; local endpoints leave it null. */
  readonly navigationId: string | null;
  readonly title: string;
  readonly route: unknown[];
  /**
   * Space vs folder, for a `container` chip's glyph and its noun. NON-CONTENT
   * metadata carried on the edge, so the chip needs no second IPC round-trip.
   */
  readonly containerLevel: ContainerLevel | null;
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
  readonly kind: LinkKind;
  /** The neighbour's raw id (`edge.otherId`) — the key `route` is built from. */
  readonly id: string;
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
 * header trigger, which opens the opaque
 * {@link RelatedHierarchyPickerComponent} modal (`link_items` on the picked
 * target — a leaf, a Shared Brain document, or a whole Space/folder as ONE
 * relation), and the hover-`×` unlink on a MANUAL chip (`unlink_items`). Both are
 * gated by the same pending set and surface failures via the {@link ToastService}.
 *
 * The flat autocomplete chooser this panel used to own is GONE — with it went the
 * popover geometry (anchor rect, reposition-on-scroll), the query/active-index
 * state and the candidate list. The modal owns all of that now, including its own
 * gated reads and its own invalidation, so this panel is back to what it is: a
 * list of relationships and two commands.
 */
@Component({
  selector: "app-connections",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink, RelatedHierarchyPickerComponent],
  templateUrl: "./connections.component.html",
  styleUrl: "./connections.component.scss",
})
export class ConnectionsComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly privacyBarrier = inject(AskHistoryPrivacyBarrierService);
  private readonly tabs = inject(TabsService);
  private readonly folders = inject(FoldersService);
  private readonly toast = inject(ToastService);
  private readonly docPreview = inject(DocumentPreviewService);
  private readonly injector = inject(Injector);
  private readonly destroyRef = inject(DestroyRef);

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
  /** Start open on surfaces where relationships are primary context (meeting Note tab). */
  readonly expandedByDefault = input(false);
  /** Stronger visual hierarchy for a primary Related band; compact notes remain unchanged. */
  readonly prominent = input(false);

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

  /**
   * The exact `+ Link` trigger, so closing the modal restores focus to the control
   * that opened it rather than dropping it on `<body>`.
   */
  private readonly pickerTrigger =
    viewChild<ElementRef<HTMLElement>>("pickerTrigger");

  /** Whether the `+ Link` hierarchy modal is open. */
  readonly pickerOpen = signal(false);
  /** True while a `link_items` call from a pick is in flight (disables the trigger + the modal). */
  readonly linking = signal(false);

  /**
   * `${kind}:${id}` of every endpoint ALREADY related, fed to the modal so those
   * rows read `Linked` and stay inactive. Derived from the panel's own gated
   * `list_links` read — the picker never asks a second time, so it can never
   * disclose a relation this panel would not.
   */
  readonly linkedKeys = computed(
    () => new Set(this.edges().map((e) => `${e.otherKind}:${e.otherId}`)),
  );

  /**
   * Monotonic request token — a late `list_links`/`get_backlinks` reply for a
   * superseded `(kind, id)` or lock transition is dropped (stale-result guard, FE
   * failure mode #4), the same idiom as `entity-detail`'s `_load`.
   */
  private seq = 0;
  /**
   * Content-free, monotonic invalidation token. Bumped synchronously on every
   * org-feed event so `_load` clears stale org heads/picker rows before its
   * replacement IPC begins.
   */
  private readonly orgFeedRevision = signal(0);
  private feedUnlisten: (() => void) | null = null;
  private feedDestroyed = false;
  /** Relationship titles are readable only after this panel can revoke them on org changes. */
  private readonly orgFeedListenerReady = this.installOrgFeedListener();

  /**
   * DETERMINISTIC edges only — everything that is NOT a pending semantic suggestion:
   * `wikilink`/`companion`/`manual`, any `manual`-flagged chip, AND an ACCEPTED
   * semantic edge (`status !== "suggested"`). These become "linked"/"companion"/
   * "related" Related rows.
   */
  private readonly deterministic = computed(() =>
    this.edges().filter(
      (e) =>
        !(e.edgeType === "semantic" && e.status === "suggested" && !e.manual),
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
        id: edge.otherId,
        navigationId: edge.navigationId ?? null,
        title: edge.otherTitle,
        route: this.routeForKind(edge.otherKind, edge.otherId),
        containerLevel: edge.otherContainerLevel ?? null,
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
        id: bl.id,
        navigationId: null,
        title: bl.title,
        route: this.routeForKind(bl.kind, bl.id),
        containerLevel: null,
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
        id: edge.otherId,
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
   * Whether the section has rows to collapse/expand. With no rows the template
   * keeps only the quiet `+ Link` trigger visible, so a fresh item can create its
   * first relationship. This computed chooses the layout; it never gates the
   * unlocked panel itself.
   */
  readonly hasAnything = computed(
    () => this.relatedCount() > 0 || this.suggestionRows().length > 0,
  );

  ngOnInit(): void {
    if (this.expandedByDefault()) {
      this.expanded.set(true);
    }
  }

  constructor() {
    const unregisterPrivacy = this.privacyBarrier.registerInvalidator(() =>
      this.invalidateVisibleRelationships(),
    );
    this.destroyRef.onDestroy(() => {
      this.feedDestroyed = true;
      this.feedUnlisten?.();
      this.feedUnlisten = null;
      unregisterPrivacy();
    });
  }

  /**
   * Install the panel-owned org-feed listener and expose its acknowledgement as a hard read
   * barrier. An event revokes visible titles and invalidates in-flight replies synchronously,
   * before the revision signal schedules the replacement fetch.
   */
  private async installOrgFeedListener(): Promise<boolean> {
    try {
      const unlisten = await this.ipc.onOrgFeedUpdated(() => {
        this.invalidateVisibleRelationships();
        this.orgFeedRevision.update((revision) => revision + 1);
      });
      if (this.feedDestroyed) {
        unlisten();
        return false;
      }
      this.feedUnlisten = unlisten;
      return true;
    } catch {
      return false;
    }
  }

  /**
   * Load BOTH the edges and the inbound backlinks whenever `(kind, id)`,
   * `locked`, the session lock-tree, OR the content-free org-feed revision
   * changes. Reading `folders.tree()`
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
    this.orgFeedRevision();
    const seq = ++this.seq;
    // A new anchor, ANY lock-tree publication, or an org-feed revision may
    // revoke a neighbour that supplied a title. Clear synchronously before the
    // replacement read; privacy wins over stale-while-revalidate here.
    this.edges.set([]);
    this.backlinksIn.set([]);
    untracked(() => this.closePicker());
    if (locked || !id) {
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
    const [privacyReady, orgFeedReady] = await Promise.all([
      this.privacyBarrier.ensureReady(),
      this.orgFeedListenerReady,
    ]);
    if (seq !== this.seq) {
      return;
    }
    if (!privacyReady || !orgFeedReady) {
      this.edges.set([]);
      this.backlinksIn.set([]);
      return;
    }
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

  /** Process privacy barrier: synchronously revoke every title and stale reply. */
  private invalidateVisibleRelationships(): void {
    this.seq += 1;
    this.edges.set([]);
    this.backlinksIn.set([]);
    untracked(() => this.closePicker());
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

  /**
   * The click-through route array for a neighbour, split by kind. A `document`
   * neighbour has NO valid route (`get_note` rejects a document id, so
   * `["/notes", id]` is a dead end) — its chip opens the read-only preview via
   * {@link openDocument} instead of a `[routerLink]`, so this route is only ever
   * consumed for `meeting`/`note` chips. It still returns a value for a document
   * (kept as the identity for the row) but the template never binds it there.
   */
  private routeForKind(kind: LinkKind, id: string): unknown[] {
    // EXHAUSTIVE on purpose: a new endpoint kind that silently fell through to
    // `["/notes", id]` would produce a chip that navigates to a dead end. A
    // `container` chip routes to the container view the sidebar already opens.
    switch (kind) {
      case "meeting":
        return ["/meeting", id];
      case "container":
        return ["/container", id];
      case "note":
      case "document":
      case "org":
        return ["/notes", id];
    }
  }

  /**
   * Open a `document` neighbour in the app-wide read-only preview modal (a
   * document has no route). The gated `getDocument(id)` read is done by the
   * modal; a sealed folder masks it to "🔒 Locked", so this can't reveal
   * locked content.
   */
  openDocument(id: string, title: string): void {
    this.docPreview.open({ id, name: title, kind: "document" });
  }

  /** Open the CURRENT live revision behind a stable Shared Brain document link. */
  openOrgItem(itemId: string | null, title: string): void {
    if (!itemId) {
      this.toast.info("This shared note is no longer available.");
      return;
    }
    void this.tabs.openOrgItem(itemId, title || "Shared note");
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
   * PR-1 — remove a user-created link using its exact stored direction, then
   * re-fetch so the chip drops out. An incident `in` edge is `neighbour → anchor`;
   * an `out` edge is `anchor → neighbour`. Guarded by the same pending set
   * (keyed on the chip's edge id) so its `×` disables mid-flight. Surfaces a
   * failure via a toast.
   */
  unlink(e: LinkEdge): void {
    const anchorKind = this.kind();
    const anchorId = this.id();
    const srcKind = e.direction === "in" ? e.otherKind : anchorKind;
    const srcId = e.direction === "in" ? e.otherId : anchorId;
    const dstKind = e.direction === "in" ? anchorKind : e.otherKind;
    const dstId = e.direction === "in" ? anchorId : e.otherId;
    void this.mutate(
      e.id,
      () => this.ipc.unlinkItems(srcKind, srcId, dstKind, dstId, e.manualEdges),
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

  // --- `+ Link` (the opaque hierarchy modal) ---------------------------------

  /**
   * Open the "Add related" modal. It owns its own gated reads, its own keyboard
   * model and its own invalidation; this panel supplies the anchor, the
   * already-linked keys, and the write.
   */
  openPicker(): void {
    if (this.linking()) {
      return;
    }
    this.pickerOpen.set(true);
  }

  /**
   * Close the modal and restore focus to the EXACT `+ Link` trigger that opened
   * it — otherwise focus falls to `<body>` and the next Tab restarts from the top
   * of the page, which is the difference between a dialog and a trapdoor.
   */
  closePicker(): void {
    if (!this.pickerOpen()) {
      return;
    }
    this.pickerOpen.set(false);
    this.restorePickerTriggerFocus();
  }

  private restorePickerTriggerFocus(): void {
    afterNextRender(
      () => {
        const trigger = this.pickerTrigger()?.nativeElement;
        if (trigger && !trigger.hasAttribute("disabled")) {
          trigger.focus();
        }
      },
      { injector: this.injector },
    );
  }

  /**
   * A target was picked: create the directed link from this panel's anchor to it,
   * then re-fetch so the new chip appears. Exactly ONE `link_items` call — a
   * `container` target links the PLACE, never its contents. Guarded by `linking`
   * (a stuck spinner is impossible — cleared in `finally`); a failure toasts.
   */
  onPicked(target: PickerTarget): void {
    if (this.linking()) {
      return;
    }
    if (target.kind === this.kind() && target.id === this.id()) {
      // The picker already disables the anchor's own row; this is the
      // belt-and-braces refusal for a stale row that survived a reload.
      this.toast.info("You can't link an item to itself.");
      return;
    }
    this.linking.set(true);
    this.closePicker();
    void (async () => {
      try {
        await this.ipc.linkItems(
          this.kind(),
          this.id(),
          target.kind,
          target.id,
        );
        const seq = ++this.seq;
        await this.fetch(this.kind(), this.id(), seq);
      } catch {
        this.toast.danger("Couldn't create the link.");
      } finally {
        this.linking.set(false);
        // `closePicker()` ran while the trigger was disabled by `linking`; focus
        // can only be restored after the mutation settles and the trigger is enabled again.
        this.restorePickerTriggerFocus();
      }
    })();
  }
}
