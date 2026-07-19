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
import { TabsService } from "../../../core/tabs.service";
import {
  FullBrainSceneDirective,
  type FullSceneEdge,
  type FullSceneNode,
} from "./full-brain-scene.directive";

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

/** Rendered-node hard cap — the strongest-degree top-K keep the draw bounded. */
const MAX_NODES = 140;
/** Fixed force-directed iteration count — run ONCE, synchronously, no loop. */
const ITERATIONS = 260;
/** Logical world scale (world units); the scene camera fits to the cloud. */
const WORLD = 1000;

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
 * CLICK-THROUGH: a node click routes by kind — meeting → `/meeting/:id`,
 * note/document → the note editor (`/notes/:id`, both are `documents` rows),
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
    { kind: "semantic", label: "Semantic", token: "--graph-document" },
  ];

  // ── hover hint (a11y / affordance) ───────────────────────────────────────
  readonly hoverId = signal<string | null>(null);
  readonly selectedId = signal<string | null>(null);

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
   * The laid-out scene nodes — a PURE derivation of the lens-filtered graph.
   * Top-{@link MAX_NODES} by in-graph degree (id tiebreak), circular seed keyed
   * by sort index, a fixed {@link ITERATIONS}-iteration 2-D Fruchterman-Reingold
   * pass (pairwise repulsion + edge springs + gentle centre pull), then an
   * overlap-relaxation pass. Deterministic: no `Math.random`, coincident points
   * get index-hash nudges — same data → identical layout every render.
   */
  protected readonly sceneNodes = computed<FullSceneNode[]>(() => {
    const all = this.filteredNodes();
    if (all.length === 0) {
      return [];
    }
    // Deterministic order: highest-degree first, id as a stable tiebreak.
    const ordered = [...all].sort(
      (a, b) => b.degree - a.degree || a.id.localeCompare(b.id),
    );
    const nodes = ordered.slice(0, MAX_NODES);
    const n = nodes.length;
    const idIndex = new Map(nodes.map((x, i) => [x.id, i]));

    // Degree → soma radius (7…20 world units), sqrt-scaled.
    const maxDeg = Math.max(1, ...nodes.map((x) => x.degree));
    const radii = nodes.map(
      (x) => 7 + (Math.sqrt(x.degree) / Math.sqrt(maxDeg)) * 13,
    );

    // SEED: a golden-angle spiral keyed by sort index → identical every render.
    const golden = Math.PI * (3 - Math.sqrt(5));
    const seedR = WORLD * 0.42;
    const xs = new Float64Array(n);
    const ys = new Float64Array(n);
    for (let i = 0; i < n; i++) {
      const rr = seedR * Math.sqrt((i + 0.5) / n);
      const ang = i * golden;
      xs[i] = rr * Math.cos(ang);
      ys[i] = rr * Math.sin(ang);
    }

    // Only edges whose endpoints both survived the cap participate in springs.
    const edges = this.filteredEdges().filter(
      (e) => idIndex.has(e.src) && idIndex.has(e.dst),
    );
    const degree = new Map<string, number>();
    for (const e of edges) {
      degree.set(e.src, (degree.get(e.src) ?? 0) + 1);
      degree.set(e.dst, (degree.get(e.dst) ?? 0) + 1);
    }

    // 2-D Fruchterman-Reingold.
    const k = Math.sqrt((WORLD * WORLD) / Math.max(1, n)) * 1.1;
    const k2 = k * k;
    let temp = WORLD * 0.14;
    const cool = temp / (ITERATIONS + 1);
    const dxs = new Float64Array(n);
    const dys = new Float64Array(n);

    for (let iter = 0; iter < ITERATIONS; iter++) {
      dxs.fill(0);
      dys.fill(0);
      for (let i = 0; i < n; i++) {
        for (let j = i + 1; j < n; j++) {
          let dx = xs[i] - xs[j];
          let dy = ys[i] - ys[j];
          let d2 = dx * dx + dy * dy;
          if (d2 < 0.01) {
            dx = ((i * 31 + j) % 7) - 3 + 0.5;
            dy = ((i * 17 + j) % 5) - 2 + 0.5;
            d2 = dx * dx + dy * dy;
          }
          const dist = Math.sqrt(d2);
          const force = k2 / dist;
          const fx = (dx / dist) * force;
          const fy = (dy / dist) * force;
          dxs[i] += fx;
          dys[i] += fy;
          dxs[j] -= fx;
          dys[j] -= fy;
        }
      }
      for (const e of edges) {
        const i = idIndex.get(e.src) as number;
        const j = idIndex.get(e.dst) as number;
        const dx = xs[i] - xs[j];
        const dy = ys[i] - ys[j];
        const dist = Math.sqrt(dx * dx + dy * dy) || 0.01;
        // Attenuate hub springs (÷√min-degree) so high-degree nodes don't crush
        // their whole neighbourhood into one clump.
        const hub = Math.sqrt(
          Math.min(degree.get(e.src) ?? 1, degree.get(e.dst) ?? 1),
        );
        const force = ((dist * dist) / k / hub) * 0.9;
        const fx = (dx / dist) * force;
        const fy = (dy / dist) * force;
        dxs[i] -= fx;
        dys[i] -= fy;
        dxs[j] += fx;
        dys[j] += fy;
      }
      for (let i = 0; i < n; i++) {
        const dl = Math.sqrt(dxs[i] * dxs[i] + dys[i] * dys[i]) || 1;
        const step = Math.min(dl, temp);
        xs[i] += (dxs[i] / dl) * step;
        ys[i] += (dys[i] / dl) * step;
        xs[i] -= xs[i] * 0.006;
        ys[i] -= ys[i] * 0.006;
      }
      temp = Math.max(0, temp - cool);
    }

    // Recentre on the centroid.
    let mx = 0;
    let my = 0;
    for (let i = 0; i < n; i++) {
      mx += xs[i];
      my += ys[i];
    }
    mx /= n;
    my /= n;
    for (let i = 0; i < n; i++) {
      xs[i] -= mx;
      ys[i] -= my;
    }

    // OVERLAP RELAXATION: push any two somas apart until clear.
    for (let pass = 0; pass < 30; pass++) {
      let moved = false;
      for (let i = 0; i < n; i++) {
        for (let j = i + 1; j < n; j++) {
          let dx = xs[i] - xs[j];
          let dy = ys[i] - ys[j];
          let dist = Math.sqrt(dx * dx + dy * dy);
          const need = radii[i] + radii[j] + 16;
          if (dist >= need) {
            continue;
          }
          if (dist < 0.01) {
            dx = ((i * 31 + j) % 7) - 3 + 0.5;
            dy = ((i * 17 + j) % 5) - 2 + 0.5;
            dist = Math.sqrt(dx * dx + dy * dy);
          }
          const push = (need - dist) / 2 / dist;
          xs[i] += dx * push;
          ys[i] += dy * push;
          xs[j] -= dx * push;
          ys[j] -= dy * push;
          moved = true;
        }
      }
      if (!moved) {
        break;
      }
    }

    return nodes.map((node, i) => ({
      id: node.id,
      kind: node.kind,
      label: node.label,
      r: radii[i],
      x: xs[i],
      y: ys[i],
    }));
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

  // ── camera toolbar (delegates to the scene directive) ────────────────────
  protected zoomIn(): void {
    this.scene()?.zoomBy(1.25);
  }
  protected zoomOut(): void {
    this.scene()?.zoomBy(0.8);
  }
  protected resetView(): void {
    this.scene()?.resetView();
  }

  // ── click-through (route by node kind — reuse existing nav) ──────────────
  protected onPick(id: string | null): void {
    if (id === null) {
      this.selectedId.set(null);
      return;
    }
    const node = this.graphData()?.nodes.find((n) => n.id === id);
    if (!node) {
      return;
    }
    this.selectedId.set(id);
    switch (node.kind) {
      case "meeting":
        void this.router.navigate(["/meeting", id]);
        break;
      case "note":
      case "document":
        // Both are `documents` rows — the note editor renders either.
        void this.tabs.openNote(id, node.label || "Note");
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
