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
import type { GraphData, GraphNode } from "../../core/models";

/** A node placed by the deterministic layout (SVG user-space coordinates). */
interface PlacedNode {
  id: string;
  name: string;
  kind: GraphNode["kind"];
  mentionCount: number;
  x: number;
  y: number;
  /** Node radius ∝ mention count (clamped). */
  r: number;
}

/** An edge resolved to its two endpoints' placed coordinates. */
interface PlacedEdge {
  /** `${source}::${target}` — stable @for track key. */
  key: string;
  source: string;
  target: string;
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  /** Stroke width ∝ co-occurrence weight (clamped). */
  width: number;
  /** Stroke opacity ∝ weight (clamped). */
  opacity: number;
}

/** The current pan/zoom window, mapped straight to the SVG `viewBox`. */
interface ViewBox {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** Logical layout canvas (user units). The viewBox pans/zooms over this. */
const WORLD = 1000;
/** Cap on rendered nodes — the strongest top-K by mention count. Big graphs
 *  cluster down to this so the layout + DOM stay bounded. */
const MAX_NODES = 60;
/** Fixed force-directed iteration count — run ONCE, synchronously, no loop. */
const ITERATIONS = 240;
/** Zoom clamp (× the world size visible). */
const MIN_W = WORLD * 0.18;
const MAX_W = WORLD * 1.6;

/**
 * The interactive BRAIN MAP — `get_graph()` rendered as a hand-rolled node-link
 * SVG (no graph library, no new dependency).
 *
 * LAYOUT (deterministic, one-shot, zoneless-safe): nodes are SEEDED on a
 * golden-angle spiral keyed by their sort index (stable across renders), then a
 * fixed {@link ITERATIONS}-iteration Fruchterman-Reingold-style force pass runs
 * SYNCHRONOUSLY inside a `computed()` — pairwise repulsion + per-edge spring
 * attraction + a gentle pull to centre, with a cooling schedule. There is NO
 * `requestAnimationFrame`, NO `setInterval`, NO animation loop: positions are a
 * pure, cached function of the (capped) graph data, so the same data always
 * lays out identically. Large graphs cluster to the top-{@link MAX_NODES} by
 * mention count (with a `has-hidden`/`capped` disclosure surfaced by the parent).
 *
 * PAN/ZOOM is a single {@link ViewBox} signal bound to the SVG `viewBox`: wheel
 * zooms toward the cursor, pointer-drag pans. Clicking a node highlights its
 * one-hop neighbourhood (dimming the rest); clicking empty space clears it.
 *
 * HONESTY: a polished ANIMATED force graph (live tick simulation, drag-to-reflow
 * individual nodes, WebGL for thousands of nodes) would want a real graph lib
 * (d3-force / cytoscape / sigma) — a new npm dependency, which is forbidden here
 * without approval. This is the best STATIC, no-dep, one-shot-layout version.
 */
@Component({
  selector: "app-brain-map",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="bm">
      <div class="bm-toolbar" role="toolbar" aria-label="Map controls">
        <div class="bm-legend" aria-hidden="true">
          <span class="bm-legend-item">
            <span class="bm-dot is-person"></span>People
          </span>
          <span class="bm-legend-item">
            <span class="bm-dot is-project"></span>Projects
          </span>
        </div>
        <div class="bm-zoom">
          <button
            type="button"
            class="btn btn-ghost bm-zbtn"
            aria-label="Zoom in"
            (click)="zoomBy(0.8)"
          >
            +
          </button>
          <button
            type="button"
            class="btn btn-ghost bm-zbtn"
            aria-label="Zoom out"
            (click)="zoomBy(1.25)"
          >
            −
          </button>
          <button
            type="button"
            class="btn btn-ghost bm-zbtn bm-zreset"
            aria-label="Reset view"
            (click)="resetView()"
          >
            Reset
          </button>
        </div>
      </div>

      <svg
        #canvas
        class="bm-canvas"
        [class.is-panning]="panning()"
        [attr.viewBox]="viewBoxStr()"
        role="img"
        [attr.aria-label]="ariaLabel()"
        (wheel)="onWheel($event)"
        (pointerdown)="onPointerDown($event)"
        (pointermove)="onPointerMove($event)"
        (pointerup)="onPointerUp($event)"
        (pointercancel)="onPointerUp($event)"
        (click)="onCanvasClick($event)"
      >
        <!-- Edges first, beneath the nodes. -->
        <g class="bm-edges">
          @for (e of placedEdges(); track e.key) {
            <line
              class="bm-edge"
              [class.is-dim]="isEdgeDim(e)"
              [attr.x1]="e.x1"
              [attr.y1]="e.y1"
              [attr.x2]="e.x2"
              [attr.y2]="e.y2"
              [attr.stroke-width]="e.width"
              [style.opacity]="isEdgeDim(e) ? 0.06 : e.opacity"
            />
          }
        </g>

        <!-- Nodes: each a clickable group (dot + label). -->
        <g class="bm-nodes">
          @for (n of placedNodes(); track n.id) {
            <g
              class="bm-node"
              [class.is-project]="n.kind === 'project'"
              [class.is-selected]="selectedId() === n.id"
              [class.is-dim]="isNodeDim(n.id)"
              role="button"
              tabindex="0"
              [attr.aria-label]="nodeLabel(n)"
              [attr.aria-pressed]="selectedId() === n.id"
              (click)="onNodeClick($event, n.id)"
              (keydown.enter)="onNodeActivate(n.id)"
              (keydown.space)="onNodeSpace($event, n.id)"
            >
              <circle
                class="bm-dot-hit"
                [attr.cx]="n.x"
                [attr.cy]="n.y"
                [attr.r]="n.r + 8"
              />
              <circle
                class="bm-dot"
                [attr.cx]="n.x"
                [attr.cy]="n.y"
                [attr.r]="n.r"
              />
              <text
                class="bm-label"
                [attr.x]="n.x"
                [attr.y]="n.y + n.r + 13"
                text-anchor="middle"
              >
                {{ n.name }}
              </text>
            </g>
          }
        </g>
      </svg>

      <p class="bm-hint">
        Scroll to zoom · drag to pan · click a node to focus its connections.
      </p>
    </div>
  `,
  styles: [
    `
      :host {
        display: block;
      }
      .bm {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }

      .bm-toolbar {
        display: flex;
        align-items: center;
        justify-content: space-between;
        flex-wrap: wrap;
        gap: var(--space-3);
      }
      .bm-legend {
        display: inline-flex;
        align-items: center;
        gap: var(--space-4);
        font-size: 0.8125rem;
        color: var(--text-secondary);
      }
      .bm-legend-item {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
      }
      .bm-dot {
        width: 10px;
        height: 10px;
        border-radius: var(--radius-pill);
        background: var(--accent);
        box-shadow: 0 0 0 3px var(--accent-soft);
      }
      .bm-dot.is-project {
        background: #9d7bff;
        box-shadow: 0 0 0 3px rgba(157, 123, 255, 0.18);
      }
      .bm-zoom {
        display: inline-flex;
        gap: var(--space-2);
      }
      .bm-zbtn {
        min-width: 36px;
        height: 32px;
        padding: 0 var(--space-2);
        font-size: 1rem;
        line-height: 1;
      }
      .bm-zreset {
        font-size: 0.8125rem;
      }

      .bm-canvas {
        display: block;
        width: 100%;
        height: clamp(360px, 60vh, 620px);
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-lg);
        background:
          radial-gradient(
            120% 120% at 50% 0%,
            rgba(110, 118, 255, 0.06),
            transparent 60%
          ),
          var(--surface-input);
        touch-action: none;
        cursor: grab;
        user-select: none;
      }
      .bm-canvas.is-panning {
        cursor: grabbing;
      }

      .bm-edge {
        stroke: var(--border-strong);
        stroke-linecap: round;
        transition: opacity var(--transition);
      }

      .bm-node {
        cursor: pointer;
      }
      .bm-dot-hit {
        fill: transparent;
      }
      .bm-dot {
        fill: var(--accent);
        stroke: var(--surface-base);
        stroke-width: 2;
        transition:
          fill var(--transition),
          opacity var(--transition);
      }
      .bm-node.is-project .bm-dot {
        fill: #9d7bff;
      }
      .bm-label {
        fill: var(--text-secondary);
        font-family: var(--font-sans);
        font-size: 13px;
        font-weight: 550;
        pointer-events: none;
        paint-order: stroke;
        stroke: var(--surface-base);
        stroke-width: 3px;
        transition:
          fill var(--transition),
          opacity var(--transition);
      }
      .bm-node:hover .bm-dot,
      .bm-node:focus-visible .bm-dot {
        fill: var(--accent-hover);
      }
      .bm-node.is-project:hover .bm-dot,
      .bm-node.is-project:focus-visible .bm-dot {
        fill: #b69bff;
      }
      .bm-node:hover .bm-label,
      .bm-node:focus-visible .bm-label {
        fill: var(--text-primary);
      }
      .bm-node:focus-visible {
        outline: none;
      }
      .bm-node:focus-visible .bm-dot {
        stroke: var(--accent-ring);
        stroke-width: 3;
      }
      .bm-node.is-selected .bm-dot {
        stroke: var(--accent-hover);
        stroke-width: 3;
      }
      .bm-node.is-selected .bm-label {
        fill: var(--text-primary);
      }
      .bm-node.is-dim .bm-dot {
        opacity: 0.18;
      }
      .bm-node.is-dim .bm-label {
        opacity: 0.1;
      }

      .bm-hint {
        margin: 0;
        text-align: center;
        color: var(--text-muted);
        font-size: 0.75rem;
      }
    `,
  ],
})
export class BrainMapComponent {
  private readonly injector = inject(Injector);

  /** The graph to visualise. Re-laying out when it changes (pure derivation). */
  readonly data = input<GraphData | null>(null);

  /** The currently focused node id, or null. Drives neighbourhood highlight. */
  readonly selectedId = signal<string | null>(null);

  private readonly canvas =
    viewChild<ElementRef<SVGSVGElement>>("canvas");

  /** The pan/zoom window → the SVG viewBox. Starts centred on the world. */
  private readonly _viewBox = signal<ViewBox>({
    x: 0,
    y: 0,
    w: WORLD,
    h: WORLD,
  });
  readonly viewBoxStr = computed(() => {
    const v = this._viewBox();
    return `${v.x} ${v.y} ${v.w} ${v.h}`;
  });

  /** True while a pointer-drag pan is in flight (toggles the grab cursor). */
  readonly panning = signal(false);
  /** Last pointer position during a pan, in CLIENT pixels. */
  private panStart: { px: number; py: number } | null = null;
  /** Measured SVG client size (for px→user-unit conversion). Updated on demand. */
  private readonly clientSize = signal<{ w: number; h: number }>({
    w: WORLD,
    h: WORLD,
  });

  /** Canvas width/height ratio (for squaring the fit-box to the viewport). */
  private readonly canvasAspect = computed(() => {
    const cs = this.clientSize();
    return cs.h > 0 ? cs.w / cs.h : 1.6;
  });

  /** True once the user has panned/zoomed — stops the auto-fit from stomping them. */
  private touched = false;

  constructor() {
    // Measure the SVG once after first render so wheel/drag deltas convert from
    // client px to user units accurately. afterNextRender — never setTimeout.
    afterNextRender(() => this.measure(), { injector: this.injector });

    // FIT-TO-VIEW: when the laid-out node bounding box changes (new data, or the
    // first layout) and the user hasn't panned/zoomed yet, snap the viewBox to
    // that bbox + padding so the nodes FILL the canvas instead of clustering in a
    // corner of the 1000×1000 world. Reading `fitBox()` registers the dependency;
    // this writes `_viewBox`, so allowSignalWrites is required (NG0600 guard).
    effect(
      () => {
        const box = this.fitBox();
        if (box && !this.touched) {
          this._viewBox.set(box);
        }
      },
      { allowSignalWrites: true },
    );
  }

  /**
   * The tight bounding box of the laid-out nodes (their circles + labels),
   * padded so nothing clips the canvas edge, and squared to the canvas aspect so
   * the SVG's `preserveAspectRatio` doesn't letterbox it off-centre. Null while
   * there are no nodes (nothing to fit). This is what fixes "4 dots in a corner":
   * the initial view frames exactly the drawn graph, not the whole 1000×1000
   * world the layout happens to seed into.
   */
  private readonly fitBox = computed<ViewBox | null>(() => {
    const nodes = this.placedNodes();
    if (nodes.length === 0) {
      return null;
    }
    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;
    for (const n of nodes) {
      // Include the node radius AND its label (drawn below the dot).
      minX = Math.min(minX, n.x - n.r);
      minY = Math.min(minY, n.y - n.r);
      maxX = Math.max(maxX, n.x + n.r);
      maxY = Math.max(maxY, n.y + n.r + 20);
    }
    // Pad by a fraction of the larger span (min floor so a lone node isn't zoomed
    // to fill the whole canvas).
    const spanX = maxX - minX;
    const spanY = maxY - minY;
    const pad = Math.max(60, Math.max(spanX, spanY) * 0.12);
    let x = minX - pad;
    let y = minY - pad;
    let w = spanX + pad * 2;
    let h = spanY + pad * 2;
    // Square to the ~viewport aspect (canvas is roughly landscape); grow the
    // shorter axis so the fit stays centred and the whole graph is visible.
    const aspect = this.canvasAspect();
    if (w / h < aspect) {
      const nw = h * aspect;
      x -= (nw - w) / 2;
      w = nw;
    } else {
      const nh = w / aspect;
      y -= (nh - h) / 2;
      h = nh;
    }
    return { x, y, w, h };
  });

  /**
   * The capped, laid-out nodes. Pure function of {@link data}: take the top-K by
   * mention count, seed them deterministically, then run a FIXED-iteration force
   * pass synchronously. Cached by `computed`; no simulation loop.
   */
  protected readonly placedNodes = computed<PlacedNode[]>(() => {
    const d = this.data();
    if (!d || d.nodes.length === 0) {
      return [];
    }

    // Deterministic order: most-mentioned first, name as a stable tiebreak.
    const ordered = [...d.nodes].sort(
      (a, b) =>
        b.mentionCount - a.mentionCount || a.name.localeCompare(b.name),
    );
    const nodes = ordered.slice(0, MAX_NODES);
    const n = nodes.length;
    const ids = nodes.map((x) => x.id);
    const idIndex = new Map(ids.map((id, i) => [id, i]));

    // Mention-count → radius (10…34 user units), sqrt-scaled so big counts
    // don't dominate. A lone node still reads.
    const maxM = Math.max(1, ...nodes.map((x) => x.mentionCount));
    const radii = nodes.map(
      (x) => 10 + (Math.sqrt(x.mentionCount) / Math.sqrt(maxM)) * 24,
    );

    // SEED: golden-angle spiral, keyed by index → identical every render.
    const cx = WORLD / 2;
    const cy = WORLD / 2;
    const golden = Math.PI * (3 - Math.sqrt(5));
    const xs = new Float64Array(n);
    const ys = new Float64Array(n);
    for (let i = 0; i < n; i++) {
      const radius = (WORLD * 0.42) * Math.sqrt((i + 0.5) / n);
      const angle = i * golden;
      xs[i] = cx + radius * Math.cos(angle);
      ys[i] = cy + radius * Math.sin(angle);
    }

    // Only edges whose endpoints both survived the cap participate.
    const edges = d.edges.filter(
      (e) => idIndex.has(e.source) && idIndex.has(e.target),
    );

    // Fruchterman-Reingold-ish constants. k = ideal edge length.
    const area = WORLD * WORLD;
    const k = Math.sqrt(area / Math.max(1, n)) * 0.62;
    const k2 = k * k;
    let temp = WORLD * 0.16;
    const cool = temp / (ITERATIONS + 1);

    const dispX = new Float64Array(n);
    const dispY = new Float64Array(n);

    for (let iter = 0; iter < ITERATIONS; iter++) {
      dispX.fill(0);
      dispY.fill(0);

      // Repulsion between every pair (n ≤ MAX_NODES so O(n²) is bounded).
      for (let i = 0; i < n; i++) {
        for (let j = i + 1; j < n; j++) {
          let dx = xs[i] - xs[j];
          let dy = ys[i] - ys[j];
          let dist2 = dx * dx + dy * dy;
          if (dist2 < 0.01) {
            // Deterministic nudge for coincident seeds (no Math.random).
            dx = ((i * 31 + j) % 7) - 3 + 0.5;
            dy = ((i * 17 + j) % 5) - 2 + 0.5;
            dist2 = dx * dx + dy * dy;
          }
          const dist = Math.sqrt(dist2);
          const force = k2 / dist;
          const fx = (dx / dist) * force;
          const fy = (dy / dist) * force;
          dispX[i] += fx;
          dispY[i] += fy;
          dispX[j] -= fx;
          dispY[j] -= fy;
        }
      }

      // Attraction along edges (spring), weighted by co-occurrence.
      for (const e of edges) {
        const i = idIndex.get(e.source)!;
        const j = idIndex.get(e.target)!;
        const dx = xs[i] - xs[j];
        const dy = ys[i] - ys[j];
        const dist = Math.sqrt(dx * dx + dy * dy) || 0.01;
        const w = 1 + Math.log2(1 + e.weight);
        const force = ((dist * dist) / k) * w;
        const fx = (dx / dist) * force;
        const fy = (dy / dist) * force;
        dispX[i] -= fx;
        dispY[i] -= fy;
        dispX[j] += fx;
        dispY[j] += fy;
      }

      // Apply displacement, capped by the cooling temperature; gentle pull to
      // centre keeps disconnected components from drifting off-canvas.
      for (let i = 0; i < n; i++) {
        const dlen = Math.sqrt(dispX[i] * dispX[i] + dispY[i] * dispY[i]) || 1;
        xs[i] += (dispX[i] / dlen) * Math.min(dlen, temp);
        ys[i] += (dispY[i] / dlen) * Math.min(dlen, temp);
        xs[i] += (cx - xs[i]) * 0.012;
        ys[i] += (cy - ys[i]) * 0.012;
      }
      temp = Math.max(0, temp - cool);
    }

    return nodes.map((node, i) => ({
      id: node.id,
      name: node.name,
      kind: node.kind,
      mentionCount: node.mentionCount,
      x: Math.round(xs[i] * 100) / 100,
      y: Math.round(ys[i] * 100) / 100,
      r: Math.round(radii[i] * 100) / 100,
    }));
  });

  /** Edges resolved to placed endpoints (only those between surviving nodes). */
  protected readonly placedEdges = computed<PlacedEdge[]>(() => {
    const d = this.data();
    if (!d) {
      return [];
    }
    const pos = new Map(this.placedNodes().map((p) => [p.id, p]));
    const maxW = Math.max(1, ...d.edges.map((e) => e.weight));
    const out: PlacedEdge[] = [];
    for (const e of d.edges) {
      const a = pos.get(e.source);
      const b = pos.get(e.target);
      if (!a || !b) {
        continue;
      }
      const ratio = e.weight / maxW;
      out.push({
        key: `${e.source}::${e.target}`,
        source: e.source,
        target: e.target,
        x1: a.x,
        y1: a.y,
        x2: b.x,
        y2: b.y,
        width: Math.round((0.8 + ratio * 4.2) * 100) / 100,
        opacity: Math.round((0.22 + ratio * 0.5) * 100) / 100,
      });
    }
    return out;
  });

  /** Ids in the focused node's one-hop neighbourhood (the node + its peers). */
  private readonly neighborhood = computed<Set<string> | null>(() => {
    const sel = this.selectedId();
    if (!sel) {
      return null;
    }
    const set = new Set<string>([sel]);
    for (const e of this.placedEdges()) {
      if (e.source === sel) {
        set.add(e.target);
      } else if (e.target === sel) {
        set.add(e.source);
      }
    }
    return set;
  });

  protected readonly ariaLabel = computed(() => {
    const n = this.placedNodes().length;
    if (n === 0) {
      return "Brain map — empty.";
    }
    return `Brain map of ${n} ${n === 1 ? "entity" : "entities"} and their connections. Scroll to zoom, drag to pan.`;
  });

  protected isNodeDim(id: string): boolean {
    const nb = this.neighborhood();
    return nb !== null && !nb.has(id);
  }

  protected isEdgeDim(e: PlacedEdge): boolean {
    const nb = this.neighborhood();
    return nb !== null && !(nb.has(e.source) && nb.has(e.target));
  }

  protected nodeLabel(n: PlacedNode): string {
    const kind = n.kind === "project" ? "Project" : "Person";
    const m =
      n.mentionCount === 1 ? "1 mention" : `${n.mentionCount} mentions`;
    return `${n.name} — ${kind}, ${m}. Focus its connections.`;
  }

  // ── interaction ────────────────────────────────────────────────────────

  protected onNodeClick(event: Event, id: string): void {
    event.stopPropagation();
    this.toggleSelect(id);
  }

  protected onNodeActivate(id: string): void {
    this.toggleSelect(id);
  }

  protected onNodeSpace(event: Event, id: string): void {
    event.preventDefault();
    this.toggleSelect(id);
  }

  private toggleSelect(id: string): void {
    this.selectedId.update((cur) => (cur === id ? null : id));
  }

  /** A bare canvas click (not on a node) clears the neighbourhood focus. */
  protected onCanvasClick(event: Event): void {
    // Node clicks stopPropagation, so anything reaching here is empty space.
    if (event.target === this.canvas()?.nativeElement) {
      this.selectedId.set(null);
    }
  }

  /** Wheel = zoom toward the cursor (clamped). */
  protected onWheel(event: WheelEvent): void {
    event.preventDefault();
    this.touched = true;
    const factor = event.deltaY > 0 ? 1.12 : 1 / 1.12;
    this.zoomAt(event.clientX, event.clientY, factor);
  }

  protected onPointerDown(event: PointerEvent): void {
    // Left button (or touch/pen) starts a pan; node clicks are handled
    // separately and don't reach a meaningful pan because we record the start.
    if (event.button !== 0) {
      return;
    }
    this.touched = true;
    this.measure();
    this.panning.set(true);
    this.panStart = { px: event.clientX, py: event.clientY };
    (event.target as Element).setPointerCapture?.(event.pointerId);
  }

  protected onPointerMove(event: PointerEvent): void {
    if (!this.panning() || !this.panStart) {
      return;
    }
    const v = this._viewBox();
    const cs = this.clientSize();
    // Convert client-px delta → user-unit delta via the current scale.
    const scaleX = v.w / Math.max(1, cs.w);
    const scaleY = v.h / Math.max(1, cs.h);
    const dx = (event.clientX - this.panStart.px) * scaleX;
    const dy = (event.clientY - this.panStart.py) * scaleY;
    this._viewBox.set({ x: v.x - dx, y: v.y - dy, w: v.w, h: v.h });
    this.panStart = { px: event.clientX, py: event.clientY };
  }

  protected onPointerUp(event: PointerEvent): void {
    this.panning.set(false);
    this.panStart = null;
    (event.target as Element).releasePointerCapture?.(event.pointerId);
  }

  /** Toolbar +/− : zoom about the viewBox centre. */
  protected zoomBy(factor: number): void {
    this.touched = true;
    const v = this._viewBox();
    const cx = v.x + v.w / 2;
    const cy = v.y + v.h / 2;
    this.applyZoom(cx, cy, factor);
  }

  /**
   * Reset = re-fit to the laid-out graph (NOT the whole 1000×1000 world), so the
   * nodes fill the canvas. Clears the `touched` flag so the auto-fit effect owns
   * the view again. Falls back to the full world only when there is nothing to fit.
   */
  protected resetView(): void {
    this.touched = false;
    this.measure();
    const box = this.fitBox();
    this._viewBox.set(box ?? { x: 0, y: 0, w: WORLD, h: WORLD });
  }

  /** Zoom about a CLIENT (px) anchor — keeps the point under the cursor fixed. */
  private zoomAt(clientX: number, clientY: number, factor: number): void {
    const el = this.canvas()?.nativeElement;
    if (!el) {
      this.zoomBy(factor);
      return;
    }
    const rect = el.getBoundingClientRect();
    const v = this._viewBox();
    const userX = v.x + ((clientX - rect.left) / Math.max(1, rect.width)) * v.w;
    const userY = v.y + ((clientY - rect.top) / Math.max(1, rect.height)) * v.h;
    this.applyZoom(userX, userY, factor);
  }

  /** Apply a clamped zoom about a USER-space anchor point. */
  private applyZoom(anchorX: number, anchorY: number, factor: number): void {
    const v = this._viewBox();
    let newW = v.w * factor;
    let newH = v.h * factor;
    // Clamp width; keep the aspect by scaling height with the applied ratio.
    const clampedW = Math.min(MAX_W, Math.max(MIN_W, newW));
    const ratio = clampedW / newW;
    newW = clampedW;
    newH = newH * ratio;
    // Keep the anchor point fixed on screen.
    const tx = (anchorX - v.x) / v.w;
    const ty = (anchorY - v.y) / v.h;
    this._viewBox.set({
      x: anchorX - tx * newW,
      y: anchorY - ty * newH,
      w: newW,
      h: newH,
    });
  }

  /** Cache the SVG's client size for px→user-unit conversion. */
  private measure(): void {
    const el = this.canvas()?.nativeElement;
    if (el) {
      const rect = el.getBoundingClientRect();
      if (rect.width > 0 && rect.height > 0) {
        this.clientSize.set({ w: rect.width, h: rect.height });
      }
    }
  }
}
