import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { Router } from "@angular/router";
import { IpcService } from "../../../core/ipc.service";
import type {
  FullGraphData,
  FullGraphEdge,
  FullGraphEdgeKind,
  FullGraphNode,
  FullGraphNodeKind,
} from "../../../core/models";
import { FoldersService } from "../../../services/folders.service";
import { DocumentPreviewService } from "../../../services/document-preview.service";
import { TabsService } from "../../../core/tabs.service";
import {
  FullBrainSceneDirective,
  type FullSceneEdge,
  type FullSceneNode,
} from "./full-brain-scene.directive";
import { layoutFullBrain, layoutLayered } from "./full-brain-layout";

/** The two graph shapes: layered "neural" bands, or organic clustered islands. */
type LayoutMode = "layers" | "clusters";

/** Per-kind node lens chip metadata (label + the token that colors its dot). */
interface NodeLens {
  kind: FullGraphNodeKind;
  label: string;
  token: string;
}
/** Per-kind edge lens chip metadata. */
interface EdgeLens {
  kind: FullGraphEdgeKind;
  label: string;
  token: string;
}

/**
 * Rendered-node hard cap — the strongest-degree top-K keep the draw bounded.
 * Raised 140 → 500 (2026-07-19): the previous cap dropped ~15 of a 155-node vault
 * ("Drawing 140 of 155"); 500 shows a normal vault IN FULL, and the per-component
 * layout + tiling in `full-brain-layout.ts` keeps even 500 nodes compact + legible.
 * Only a pathological vault (the backend returns up to ~2000) now hits the cap,
 * where the honest "Drawing N of M" disclosure + LOD labels carry it.
 */
const MAX_NODES = 500;

/**
 * The FULL-BRAIN GRAPH — `getFullGraph()` rendered as one unified, typed graph:
 * entities + meetings + notes + documents as per-kind-colored somas, and every
 * relation (co-occurrence / mention / wikilink / companion / semantic) as a
 * per-kind styled edge (suggested semantic links drawn DASHED, admitted only
 * when their lens is on).
 *
 * SPLIT OF RESPONSIBILITY (zoneless):
 * - THIS COMPONENT owns the pure data: the IPC effect-load (stale-guarded,
 *   re-fetching on a {@link FoldersService} lock-state change, and — only for
 *   the "Suggested links" toggle — passing `includeSuggested` so the backend
 *   admits/omits those rows), the LENS signals (node-kind + edge-kind toggle
 *   chips), the filtered view `computed()` (a lens toggle NEVER re-fetches — it
 *   recomputes over the already-fetched graph), and the deterministic one-shot
 *   Fruchterman-Reingold layout `computed()`.
 * - {@link FullBrainSceneDirective} owns every DOM-loop concern (ResizeObserver,
 *   invalidate-on-demand paint, pointer input) per angular-zoneless §5.
 *
 * CLICK-THROUGH: a node open routes by kind — meeting → `/meeting/:id`,
 * note → the note editor (`/notes/:id`), document → the app-wide read-only
 * preview modal (a `document` row has no route; `DocumentPreviewService`),
 * entity → the `/graph` page with `?entity=<id>` preselected (reuse existing
 * nav). Sizing is handled by the directive's ResizeObserver (no `setTimeout`).
 */
@Component({
  selector: "app-full-brain-graph",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FullBrainSceneDirective],
  templateUrl: "./full-brain-graph.component.html",
  styleUrl: "./full-brain-graph.component.scss",
})
export class FullBrainGraphComponent {
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);
  private readonly router = inject(Router);
  private readonly tabs = inject(TabsService);
  private readonly docPreview = inject(DocumentPreviewService);

  private readonly scene = viewChild(FullBrainSceneDirective);

  // ── fetched state ────────────────────────────────────────────────────────
  readonly graphData = signal<FullGraphData | null>(null);
  readonly loading = signal(true);
  readonly error = signal<string | null>(null);

  // ── lenses (node-type + edge-type toggle chips) ──────────────────────────
  /** Which NODE kinds are drawn. All on by default. */
  private readonly _nodeLens = signal<Record<FullGraphNodeKind, boolean>>({
    entity: true,
    meeting: true,
    note: true,
    document: true,
  });
  /** Which EDGE kinds are drawn. All on by default. */
  private readonly _edgeLens = signal<Record<FullGraphEdgeKind, boolean>>({
    co_occurrence: true,
    mention: true,
    wikilink: true,
    companion: true,
    manual: true,
    semantic: true,
  });
  readonly nodeLens = this._nodeLens.asReadonly();
  readonly edgeLens = this._edgeLens.asReadonly();

  /**
   * Include un-accepted (`status: "suggested"`) semantic links. This is the ONE
   * lens that RE-FETCHES (the backend must add/remove those rows); its `effect`
   * re-runs `getFullGraph({ includeSuggested })`. Every other lens filters the
   * already-fetched graph client-side.
   */
  readonly showSuggested = signal(false);

  protected readonly nodeLenses: readonly NodeLens[] = [
    { kind: "entity", label: "People & projects", token: "--graph-entity" },
    { kind: "meeting", label: "Meetings", token: "--graph-meeting" },
    { kind: "note", label: "Notes", token: "--graph-note" },
    { kind: "document", label: "Documents", token: "--graph-document" },
  ];
  protected readonly edgeLenses: readonly EdgeLens[] = [
    { kind: "co_occurrence", label: "Co-occurrence", token: "--text-muted" },
    { kind: "mention", label: "Mentions", token: "--text-secondary" },
    { kind: "wikilink", label: "Wikilinks", token: "--accent" },
    { kind: "companion", label: "Companion", token: "--graph-note" },
    { kind: "manual", label: "Manual", token: "--accent-hover" },
    { kind: "semantic", label: "Semantic", token: "--graph-document" },
  ];

  // ── hover hint (a11y / affordance) ───────────────────────────────────────
  readonly hoverId = signal<string | null>(null);
  readonly selectedId = signal<string | null>(null);
  /** Current zoom as a % of fit (100 = fit-to-view) — the scene reports it. */
  readonly zoomPct = signal(100);
  /** Graph shape: layered "neural" bands (default) or organic clustered islands. */
  readonly layoutMode = signal<LayoutMode>("layers");
  protected readonly layoutModes: readonly { key: LayoutMode; label: string }[] = [
    { key: "layers", label: "Layers" },
    { key: "clusters", label: "Clusters" },
  ];

  // ── honest disclosures (mirror the entity graph) ─────────────────────────
  protected readonly hasHidden = computed(
    () => this.graphData()?.hasHidden ?? false,
  );

  /**
   * PR-9 F2: the backend trimmed an EDGE leg (the mention or links cap). Distinct
   * from a node trim (`capDisclosure`) and a locked folder (`hasHidden`).
   */
  protected readonly edgesTruncated = computed(
    () => this.graphData()?.edgesTruncated ?? false,
  );

  /**
   * PR-9 F1 — the HONEST draw-cap disclosure. The old banner compared the backend's
   * post-per-kind-cap `nodes.length` (up to 2000) against `totalVisibleNodes` and
   * ignored BOTH the client-side lens filter AND the {@link MAX_NODES} draw cap — so
   * it could claim "Showing 500 of 812" while only 140 somas were painted,
   * reintroducing exactly the silent trim `totalVisibleNodes` exists to expose.
   *
   * The disclosure now compares what is ACTUALLY DRAWN ({@link drawnNodeCount},
   * i.e. `sceneNodes().length`) against the true visible universe
   * (`totalVisibleNodes`, the backend's uncapped count). Whenever fewer items are
   * drawn than exist — because the backend per-kind cap trimmed rows, the
   * {@link MAX_NODES} draw cap kept only the top-degree K, or a lens is hiding
   * kinds — it says "Drawing N of M items" so the count on screen always matches
   * the count in the caption.
   */
  protected readonly isCapped = computed(() => {
    const total = this.graphData()?.totalVisibleNodes ?? 0;
    return this.drawnNodeCount() < total;
  });
  protected readonly capDisclosure = computed<string | null>(() => {
    const d = this.graphData();
    if (!d || !this.isCapped()) {
      return null;
    }
    return `Drawing ${this.drawnNodeCount()} of ${d.totalVisibleNodes} items.`;
  });

  /** Total nodes the backend returned (before any lens filtering). */
  protected readonly totalNodes = computed(
    () => this.graphData()?.nodes.length ?? 0,
  );

  // ── the LENS-FILTERED view-model (pure computed — NO re-fetch on toggle) ──
  /**
   * The nodes actually drawn: the fetched nodes, keeping only lens-enabled kinds.
   * A lens toggle recomputes THIS — it never re-issues an IPC call (the graph is
   * already in hand). `showSuggested` is the sole exception and is handled by the
   * fetch effect, not here.
   */
  private readonly filteredNodes = computed<FullGraphNode[]>(() => {
    const d = this.graphData();
    if (!d) {
      return [];
    }
    const lens = this._nodeLens();
    return d.nodes.filter((n) => lens[n.kind]);
  });

  /**
   * The edges surviving the lens: both endpoints kind-visible AND the edge kind
   * lens-enabled. PR-9 F4: match each endpoint by `(kind, id)` — not bare `id` —
   * using the edge's `srcKind`/`dstKind`, so a cross-kind id collision can never
   * mis-match an edge onto the wrong node.
   */
  private readonly filteredEdges = computed<FullGraphEdge[]>(() => {
    const d = this.graphData();
    if (!d) {
      return [];
    }
    const eLens = this._edgeLens();
    const visible = new Set(this.filteredNodes().map((n) => `${n.kind}:${n.id}`));
    return d.edges.filter(
      (e) =>
        eLens[e.kind] &&
        visible.has(`${e.srcKind}:${e.src}`) &&
        visible.has(`${e.dstKind}:${e.dst}`),
    );
  });

  /**
   * Counts for the header caption ("N items · M links"). PR-9 F1: these reflect
   * what is ACTUALLY DRAWN — `sceneNodes()`/`sceneEdges()`, i.e. AFTER the
   * {@link MAX_NODES} draw cap — not the pre-cap lens-filtered sets, so the caption
   * never claims more items/links than the canvas paints.
   */
  protected readonly drawnNodeCount = computed(() => this.sceneNodes().length);
  protected readonly drawnEdgeCount = computed(() => this.sceneEdges().length);

  /**
   * The laid-out scene nodes — a PURE derivation of the lens-filtered graph via
   * {@link layoutFullBrain}: top-{@link MAX_NODES} by in-graph degree (id
   * tiebreak), then per-connected-component Fruchterman-Reingold + degree-0
   * singleton tiling + shelf-packing so disconnected orphans never scatter to the
   * corners. Deterministic (no `Math.random`) — same data → identical layout, as
   * the `computed()` re-run on every lens toggle requires.
   */
  protected readonly sceneNodes = computed<FullSceneNode[]>(() => {
    const all = this.filteredNodes();
    if (all.length === 0) {
      return [];
    }
    // Deterministic order: highest-degree first, id as a stable tiebreak; cap.
    const ordered = [...all].sort(
      (a, b) => b.degree - a.degree || a.id.localeCompare(b.id),
    );
    const capped = ordered.slice(0, MAX_NODES);
    const layout =
      this.layoutMode() === "layers" ? layoutLayered : layoutFullBrain;
    return layout(
      capped.map((nd) => ({
        id: nd.id,
        kind: nd.kind,
        label: nd.label,
        degree: nd.degree,
        date: nd.date,
      })),
      // Only edges whose endpoints both survive the cap contribute; the layout
      // filters internally, so passing all lens-filtered edges is fine.
      this.filteredEdges().map((e) => ({
        src: e.src,
        dst: e.dst,
        srcKind: e.srcKind,
        dstKind: e.dstKind,
      })),
    );
  });

  /**
   * The scene edges (between the somas that survived the {@link MAX_NODES} draw cap),
   * with a stable key + dashed flag. PR-9 F4: match each endpoint by `(kind, id)` —
   * keyed against the drawn nodes' `kind:id` — so a cross-kind id collision can't
   * paint an edge onto the wrong soma.
   */
  protected readonly sceneEdges = computed<FullSceneEdge[]>(() => {
    const keys = new Set(this.sceneNodes().map((p) => `${p.kind}:${p.id}`));
    const out: FullSceneEdge[] = [];
    for (const e of this.filteredEdges()) {
      if (keys.has(`${e.srcKind}:${e.src}`) && keys.has(`${e.dstKind}:${e.dst}`)) {
        out.push({
          key: `${e.srcKind}:${e.src}::${e.dstKind}:${e.dst}::${e.kind}`,
          src: e.src,
          dst: e.dst,
          srcKind: e.srcKind,
          dstKind: e.dstKind,
          kind: e.kind,
          suggested: e.status === "suggested",
        });
      }
    }
    return out;
  });

  protected readonly ariaLabel = computed(() => {
    const nodes = this.drawnNodeCount();
    const edges = this.drawnEdgeCount();
    if (nodes === 0) {
      return "Full-brain graph — empty.";
    }
    return (
      `Full-brain graph of ${nodes} ${nodes === 1 ? "item" : "items"} and ` +
      `${edges} ${edges === 1 ? "connection" : "connections"}.`
    );
  });

  /** The label of the currently hovered node, for the live affordance line. */
  protected readonly hoverLabel = computed<string | null>(() => {
    const id = this.hoverId();
    if (!id) {
      return null;
    }
    return this.filteredNodes().find((n) => n.id === id)?.label ?? null;
  });

  constructor() {
    // Make sure the folder tree is loaded (drives the lock-aware re-fetch below).
    void this.folders.load();

    /**
     * Load the full graph, re-loading whenever the folder lock-state changes OR
     * the "Suggested links" toggle flips (the ONLY option that changes what the
     * backend returns). Reading `folders.tree()` + `showSuggested()` registers
     * both as dependencies; a stale-result guard drops a late response so a fast
     * toggle/unlock never leaves a mismatched graph. Every OTHER lens filters
     * the fetched graph via `computed()` — it must NOT re-fetch.
     */
    let seq = 0;
    effect(() => {
      this.folders.tree();
      const includeSuggested = this.showSuggested();
      const mine = ++seq;
      this.error.set(null);
      void (async () => {
        try {
          const data = await this.ipc.getFullGraph({ includeSuggested });
          if (mine !== seq) {
            return; // superseded by a newer fetch
          }
          this.graphData.set(data);
          // Drop a selection whose node vanished (re-sealed / lens-hidden).
          const sel = this.selectedId();
          if (sel && !data.nodes.some((n) => n.id === sel)) {
            this.selectedId.set(null);
          }
        } catch (e) {
          if (mine !== seq) {
            return;
          }
          this.graphData.set(null);
          this.error.set(String(e));
        } finally {
          if (mine === seq) {
            this.loading.set(false);
          }
        }
      })();
    });
  }

  // ── lens toggles (recompute the view; never re-fetch) ────────────────────
  protected toggleNode(kind: FullGraphNodeKind): void {
    this._nodeLens.update((l) => ({ ...l, [kind]: !l[kind] }));
  }
  protected toggleEdge(kind: FullGraphEdgeKind): void {
    this._edgeLens.update((l) => ({ ...l, [kind]: !l[kind] }));
  }
  protected toggleSuggested(): void {
    this.showSuggested.update((v) => !v);
  }
  protected setLayoutMode(mode: LayoutMode): void {
    this.layoutMode.set(mode);
  }

  // ── camera toolbar (delegates to the scene directive) ────────────────────
  protected zoomIn(): void {
    this.scene()?.zoomBy(1.25);
  }
  protected zoomOut(): void {
    this.scene()?.zoomBy(0.8);
  }
  /** "Fit" — re-frame the whole cloud in the viewport. */
  protected fitView(): void {
    this.scene()?.resetView();
  }

  /**
   * Single click = FOCUS a node (pin selection so its neighbourhood spotlights
   * and the user can dwell on it), toggling off on a second click or on empty
   * space. It NEVER navigates — that's {@link onOpen} (double-click). This fixes
   * the old "a click yanks you to a route so you can never explore" behaviour.
   */
  protected onPick(id: string | null): void {
    this.selectedId.update((cur) => (id !== null && cur === id ? null : id));
  }

  /** Double click = OPEN the node (route by kind — reuse existing nav). */
  protected onOpen(id: string): void {
    const node = this.graphData()?.nodes.find((n) => n.id === id);
    if (!node) {
      return;
    }
    switch (node.kind) {
      case "meeting":
        void this.router.navigate(["/meeting", id]);
        break;
      case "note":
        // A `note` documents row IS a routable editor note.
        void this.tabs.openNote(id, node.label || "Note");
        break;
      case "document":
        // A brain-ingested `document` (e.g. a PDF) has NO route — `get_note`
        // rejects a document id (`["/notes", id]` was a dead end). Open the
        // app-wide read-only preview modal instead (gated `getDocument(id)`).
        this.docPreview.open({
          id,
          name: node.label || "Document",
          kind: "document",
        });
        break;
      case "entity":
        // Reuse the /graph page's entity detail via a preselect deep-link.
        void this.router.navigate(["/graph"], {
          queryParams: { entity: id },
        });
        break;
    }
  }
}
