import {
  DestroyRef,
  Directive,
  ElementRef,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
  untracked,
} from "@angular/core";
import type {
  FullGraphEdgeKind,
  FullGraphNodeKind,
} from "../../../core/models";

/**
 * A node the {@link FullBrainGraphComponent} has already laid out (world units,
 * origin-centred, by `full-brain-layout.ts`) — the directive only projects +
 * paints it, never lays out. `degree`/`date` are carried for the label ranking
 * and the hover tooltip.
 */
export interface FullSceneNode {
  id: string;
  kind: FullGraphNodeKind;
  label: string;
  /** In-graph edge count — drives label priority + the tooltip meta line. */
  degree: number;
  /** ISO/epoch-derived date string when the source carries one (entities: null). */
  date: string | null;
  /** Radius in world units (degree-scaled by the layout). */
  r: number;
  x: number;
  y: number;
}

/** A laid-out edge whose endpoints both survived the lens filter + draw cap. */
export interface FullSceneEdge {
  /** Stable identity. */
  key: string;
  src: string;
  dst: string;
  /** Endpoint kinds — endpoint matching is by `(kind, id)`, never bare `id`. */
  srcKind: FullGraphNodeKind;
  dstKind: FullGraphNodeKind;
  kind: FullGraphEdgeKind;
  /** `true` = un-accepted semantic suggestion → drawn DASHED. */
  suggested: boolean;
}

type Rgb = [number, number, number];

/** Tooltip chrome (the one piece of canvas paint that follows the app theme). */
interface Theme {
  textPri: Rgb;
  textSec: Rgb;
  overlay: Rgb;
  border: Rgb;
  font: string;
}

/* ── THE SCENE PALETTE — a FIXED dark field, independent of the page theme ──
 * Same rationale as neural-scene.directive.ts: glow/tint only reads on a dark
 * surface, so the canvas is its own artwork field even under the light theme.
 * The four node hues MIRROR the DOM tokens `--graph-entity/-meeting/-note/
 * -document` (whose values the chips + legend dots consume) — kept in sync by
 * intent, exactly like the neural-scene PERSON/PROJECT constants. */
const BG: Rgb = [8, 9, 20];
const BG_LIFT: Rgb = [17, 19, 40];
const NODE_COLORS: Record<FullGraphNodeKind, Rgb> = {
  entity: [91, 189, 255], // #5bbdff azure
  meeting: [255, 157, 92], // #ff9d5c amber
  note: [76, 224, 160], // #4ce0a0 mint
  document: [200, 111, 242], // #c86ff2 orchid
};
const KIND_LABEL: Record<FullGraphNodeKind, string> = {
  entity: "Person / project",
  meeting: "Meeting",
  note: "Note",
  document: "Document",
};
const LABEL: Rgb = [216, 222, 242];
const LABEL_DIM: Rgb = [150, 158, 190];
const WHITE: Rgb = [255, 255, 255];

const MIN_SCALE = 0.3;
const MAX_SCALE = 4.5;
/** The strongest N edges carry a travelling firing pulse (keeps dense graphs cheap). */
const PULSE_EDGE_CAP = 160;
/** Node breathing angular frequency — a ~2.6 s period. */
const BREATH_W = (Math.PI * 2) / 2.6;

/** djb2 hash → a deterministic per-edge/node phase so pulses never fire in lockstep. */
function djb2(s: string): number {
  let h = 5381;
  for (let i = 0; i < s.length; i++) {
    h = (((h << 5) + h) ^ s.charCodeAt(i)) >>> 0;
  }
  return h >>> 0;
}
/** Halo radius as a multiple of the soma radius (cached sprite space). */
const HALO_F = 2.4;
/** Always-on label count at fit zoom; grows as the camera zooms IN. */
const LABEL_TOP = 14;
/** Ghost labels kept alive OUTSIDE a focused neighbourhood (context). */
const LABEL_GHOSTS = 5;

function rgba(c: Rgb, a: number): string {
  return `rgba(${c[0]},${c[1]},${c[2]},${a})`;
}
function mix(a: Rgb, b: Rgb, t: number): Rgb {
  return [
    Math.round(a[0] + (b[0] - a[0]) * t),
    Math.round(a[1] + (b[1] - a[1]) * t),
    Math.round(a[2] + (b[2] - a[2]) * t),
  ];
}
function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v;
}
/** Parse a token value (`#rgb`, `#rrggbb`, `rgb()/rgba()`) with a fallback. */
function parseColor(raw: string, fallback: Rgb): Rgb {
  const s = raw.trim();
  const hex = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(s);
  if (hex) {
    const v = hex[1];
    if (v.length === 3) {
      return [
        parseInt(v[0] + v[0], 16),
        parseInt(v[1] + v[1], 16),
        parseInt(v[2] + v[2], 16),
      ];
    }
    return [
      parseInt(v.slice(0, 2), 16),
      parseInt(v.slice(2, 4), 16),
      parseInt(v.slice(4, 6), 16),
    ];
  }
  const fn = /^rgba?\(\s*([\d.]+)[,\s]+([\d.]+)[,\s]+([\d.]+)/i.exec(s);
  if (fn) {
    return [Number(fn[1]), Number(fn[2]), Number(fn[3])];
  }
  return fallback;
}

interface Proj {
  sx: number;
  sy: number;
  sr: number;
}

/**
 * The FULL-BRAIN scene renderer — a canvas-drawn, pan/zoom multi-kind graph over
 * the typed nodes/edges the component lays out: per-kind colored somas with
 * CACHED halo sprites (no per-frame gradient allocation), curved GLOWING synapses
 * (bloom underpass + a core gradienting between the endpoint node hues) carrying
 * travelling FIRING PULSES, breathing somas, COLLISION-DECLUTTERED adaptive labels
 * (largest-first, zoom-scaled budget, fan-out placement — the map-style labels),
 * a hover neighbourhood spotlight, an opaque hover tooltip, and click-to-focus /
 * double-click-to-open, on a fixed deep-indigo field.
 *
 * ZONELESS RULE (.claude/rules/angular-zoneless.md §5): a bare rAF/timer or a DOM
 * observer in a COMPONENT is banned — every DOM-loop concern (the ResizeObserver,
 * the visibilitychange + reduced-motion + color-scheme listeners, the FX loop, the
 * invalidate-on-demand paint) lives HERE in a directive and is released in
 * `DestroyRef.onDestroy()`. The animation loop is BOUNDED and drives ONLY the
 * decorative FX (synapse pulses + soma breathing) over the still, pre-computed
 * layout — there is NO per-frame physics. It is stopped entirely under
 * `prefers-reduced-motion: reduce` and while `document.hidden`, where the scene
 * falls back to a one-shot `invalidate()` repaint (a still), so it costs nothing
 * at rest.
 *
 * The component owns the deterministic layout — layered "neural" bands OR organic
 * per-component packing (pure functions in `full-brain-layout.ts`, called from a
 * `computed()`); this directive owns only projection, paint, camera, pointer.
 */
@Directive({
  selector: "canvas[appFullBrainScene]",
  host: {
    "[style.cursor]": "cursor()",
    "(pointerdown)": "onPointerDown($event)",
    "(pointermove)": "onPointerMove($event)",
    "(pointerup)": "onPointerUp($event)",
    "(pointercancel)": "onPointerUp($event)",
    "(pointerleave)": "onPointerLeave()",
    "(dblclick)": "onDblClick($event)",
    "(wheel)": "onWheel($event)",
  },
})
export class FullBrainSceneDirective {
  private readonly hostRef = inject<ElementRef<HTMLCanvasElement>>(ElementRef);
  private readonly destroyRef = inject(DestroyRef);

  /** Laid-out nodes (world coordinates, origin-centred). */
  readonly sceneNodes = input<FullSceneNode[]>([]);
  /** Laid-out edges between surviving nodes. */
  readonly sceneEdges = input<FullSceneEdge[]>([]);
  /** The focused node id (null = no focus) — drives neighbourhood dimming. */
  readonly selectedId = input<string | null>(null);

  /** Single click on a node → focus it (the component pins selection, no nav). */
  readonly nodePick = output<string | null>();
  /** Double click on a node → open it (the component owns navigation). */
  readonly nodeOpen = output<string>();
  /** Hover enter/leave over a node (the component drives an aria-live hint). */
  readonly nodeHover = output<string | null>();
  /** Current zoom as a percentage of the fit scale (100 = fit-to-view). */
  readonly zoom = output<number>();

  private readonly _hoverId = signal<string | null>(null);
  private readonly _dragging = signal(false);
  protected readonly cursor = computed(() =>
    this._dragging()
      ? "grabbing"
      : this._hoverId() !== null
        ? "pointer"
        : "grab",
  );

  /**
   * The focus ANCHOR + its one-hop neighbours. Anchored on the SELECTED node
   * (a pinned click) if any, else on the HOVERED node (a transient spotlight) —
   * this is what "hover to preview a neighbourhood, click to pin it" needs. The
   * dim floor is stronger for a pinned selection than for a hover preview.
   */
  private readonly focus = computed<{
    set: Set<string>;
    anchor: string;
    pinned: boolean;
  } | null>(() => {
    const sel = this.selectedId();
    const anchor = sel ?? this._hoverId();
    if (!anchor) {
      return null;
    }
    const set = new Set<string>([anchor]);
    for (const e of this.sceneEdges()) {
      if (e.src === anchor) {
        set.add(e.dst);
      } else if (e.dst === anchor) {
        set.add(e.src);
      }
    }
    return { set, anchor, pinned: sel !== null };
  });

  // camera (world → screen: screen = center + pan + world * scale)
  private scale = 1;
  private panX = 0;
  private panY = 0;
  private fitScale = 1;
  private fitted = false;

  // pointer
  private pointerDown = false;
  private dragMoved = false;
  private downX = 0;
  private downY = 0;
  private lastPX = 0;
  private lastPY = 0;

  // projected screen positions (index-aligned with sceneNodes())
  private proj: Proj[] = [];

  private cssW = 0;
  private cssH = 0;
  private oneShotId: number | null = null;
  private ctx: CanvasRenderingContext2D | null;

  // paint caches (rebuilt only on resize / theme flip)
  private theme: Theme | null = null;
  private bg: HTMLCanvasElement | null = null;
  private pulseC: HTMLCanvasElement | null = null;
  private readonly sprites = new Map<string, HTMLCanvasElement>();

  // animation loop — bounded, reduced-motion + visibility gated. Drives ONLY the
  // decorative FX (travelling synapse pulses + node breathing) over the still,
  // pre-computed layout; there is no per-frame physics. Off under reduced motion.
  private rafId: number | null = null;
  private loopRunning = false;
  private reduced: boolean;
  private readonly media = window.matchMedia("(prefers-reduced-motion: reduce)");
  private readonly onMedia = (): void => {
    this.reduced = this.media.matches;
    if (this.reduced) {
      this.stopLoop();
      this.invalidate();
    } else {
      this.startLoop();
    }
  };

  private readonly ro = new ResizeObserver((entries) => {
    const rect = entries[entries.length - 1]?.contentRect;
    if (!rect || rect.width < 2 || rect.height < 2) {
      return;
    }
    const wasZero = this.cssW < 2 || this.cssH < 2;
    this.cssW = rect.width;
    this.cssH = rect.height;
    this.bg = null;
    if (wasZero) {
      this.fitted = false; // (re)fit once we have a real size
    }
    this.invalidate();
  });
  private readonly onVisibility = (): void => {
    if (document.hidden) {
      this.stopLoop();
    } else if (this.reduced) {
      this.invalidate();
    } else {
      this.startLoop();
    }
  };
  private readonly scheme = window.matchMedia("(prefers-color-scheme: light)");
  private readonly onScheme = (): void => {
    this.theme = null;
    this.invalidate();
  };

  constructor() {
    this.ctx = this.hostRef.nativeElement.getContext("2d");
    this.reduced = this.media.matches;
    this.ro.observe(this.hostRef.nativeElement);
    document.addEventListener("visibilitychange", this.onVisibility);
    this.scheme.addEventListener("change", this.onScheme);
    this.media.addEventListener("change", this.onMedia);

    // Refit + repaint when the laid-out graph changes; a signal write inside a
    // tracked effect is allowed since Angular 19. The hover read is untracked so
    // hovering doesn't refit. Restart the FX loop so pulses run on the new graph.
    effect(() => {
      const nodes = this.sceneNodes();
      this.sceneEdges();
      this.fitted = false;
      const hover = untracked(() => this._hoverId());
      if (hover !== null && !nodes.some((n) => n.id === hover)) {
        this._hoverId.set(null);
        this.nodeHover.emit(null);
      }
      if (this.reduced) {
        this.invalidate();
      } else {
        this.startLoop();
      }
    });

    // Repaint on selection/hover change (neighbourhood spotlight). Under reduced
    // motion there's no loop, so a one-shot repaint keeps the spotlight live.
    effect(() => {
      this.selectedId();
      this._hoverId();
      if (this.reduced) {
        this.invalidate();
      }
    });

    this.destroyRef.onDestroy(() => {
      this.stopLoop();
      if (this.oneShotId !== null) {
        cancelAnimationFrame(this.oneShotId);
        this.oneShotId = null;
      }
      this.ro.disconnect();
      document.removeEventListener("visibilitychange", this.onVisibility);
      this.scheme.removeEventListener("change", this.onScheme);
      this.media.removeEventListener("change", this.onMedia);
    });
  }

  // ── animation loop (only decorative FX; no per-frame physics) ──────────
  private startLoop(): void {
    if (this.loopRunning || this.reduced || document.hidden) {
      return;
    }
    this.loopRunning = true;
    const step = (t: number): void => {
      if (!this.loopRunning) {
        return;
      }
      this.render(t);
      this.rafId = requestAnimationFrame(step);
    };
    this.rafId = requestAnimationFrame(step);
  }

  private stopLoop(): void {
    this.loopRunning = false;
    if (this.rafId !== null) {
      cancelAnimationFrame(this.rafId);
      this.rafId = null;
    }
  }

  // ── public camera API (the component's toolbar drives these) ──────────

  zoomBy(factor: number): void {
    const old = this.scale;
    this.scale = clamp(old * factor, MIN_SCALE, MAX_SCALE);
    const k = this.scale / old;
    this.panX *= k;
    this.panY *= k;
    this.emitZoom();
    this.invalidate();
  }

  /** Re-fit the whole cloud into the viewport. */
  resetView(): void {
    this.fitted = false;
    this.panX = 0;
    this.panY = 0;
    this.invalidate();
  }

  // ── pointer / wheel (host listeners) ──────────────────────────────────

  protected onPointerDown(event: PointerEvent): void {
    if (event.button !== 0) {
      return;
    }
    this.pointerDown = true;
    this.dragMoved = false;
    this.downX = event.clientX;
    this.downY = event.clientY;
    this.lastPX = event.clientX;
    this.lastPY = event.clientY;
    (event.target as Element).setPointerCapture?.(event.pointerId);
  }

  protected onPointerMove(event: PointerEvent): void {
    if (this.pointerDown) {
      const dx = event.clientX - this.lastPX;
      const dy = event.clientY - this.lastPY;
      this.lastPX = event.clientX;
      this.lastPY = event.clientY;
      if (
        !this.dragMoved &&
        Math.hypot(event.clientX - this.downX, event.clientY - this.downY) > 4
      ) {
        this.dragMoved = true;
        this._dragging.set(true);
      }
      if (this.dragMoved) {
        this.panX += dx;
        this.panY += dy;
        this.invalidate();
      }
      return;
    }
    const hit = this.hitTest(event);
    if (hit !== this._hoverId()) {
      this._hoverId.set(hit);
      this.nodeHover.emit(hit);
      this.invalidate();
    }
  }

  protected onPointerUp(event: PointerEvent): void {
    const wasClick = this.pointerDown && !this.dragMoved;
    this.pointerDown = false;
    this.dragMoved = false;
    this._dragging.set(false);
    (event.target as Element).releasePointerCapture?.(event.pointerId);
    if (wasClick) {
      // Single click = FOCUS (pin selection); it never navigates — that's the
      // double-click. Lets the user dwell on a node + read its neighbourhood.
      this.nodePick.emit(this.hitTest(event));
    }
    this.invalidate();
  }

  protected onPointerLeave(): void {
    if (this._hoverId() !== null) {
      this._hoverId.set(null);
      this.nodeHover.emit(null);
      this.invalidate();
    }
  }

  /** Double click = OPEN the node (navigate). Empty space is ignored. */
  protected onDblClick(event: MouseEvent): void {
    const hit = this.hitTestXY(event.clientX, event.clientY);
    if (hit !== null) {
      this.nodeOpen.emit(hit);
    }
  }

  /** Wheel = zoom toward the cursor (the point under it stays put). */
  protected onWheel(event: WheelEvent): void {
    event.preventDefault();
    const rect = this.hostRef.nativeElement.getBoundingClientRect();
    const ox = event.clientX - rect.left - rect.width / 2;
    const oy = event.clientY - rect.top - rect.height / 2;
    const factor = Math.exp(clamp(event.deltaY, -80, 80) * -0.0016);
    const old = this.scale;
    this.scale = clamp(old * factor, MIN_SCALE, MAX_SCALE);
    const k = this.scale / old;
    this.panX = ox - (ox - this.panX) * k;
    this.panY = oy - (oy - this.panY) * k;
    this.emitZoom();
    this.invalidate();
  }

  private emitZoom(): void {
    const pct = this.fitScale > 0 ? Math.round((this.scale / this.fitScale) * 100) : 100;
    this.zoom.emit(pct);
  }

  // ── paint (invalidate-on-demand, no continuous loop) ──────────────────

  /** One-shot repaint for the idle/reduced-motion path; coalesces into one frame.
   *  A no-op while the FX loop is running (it already renders every frame). */
  private invalidate(): void {
    if (this.loopRunning || this.oneShotId !== null || document.hidden) {
      return;
    }
    this.oneShotId = requestAnimationFrame((t) => {
      this.oneShotId = null;
      this.render(t);
    });
  }

  /**
   * Fit the laid-out cloud into the padded viewport (once per graph/resize).
   * Robust framing: fit to the node bounding box, then centre the box but blend
   * 30% toward the projected MASS centroid so a couple of stray singletons can't
   * park the whole connected mass off in a corner.
   */
  private fit(): void {
    const nodes = this.sceneNodes();
    if (nodes.length === 0 || this.cssW < 4 || this.cssH < 4) {
      this.scale = 1;
      this.fitScale = 1;
      this.panX = 0;
      this.panY = 0;
      return;
    }
    let minX = Infinity;
    let maxX = -Infinity;
    let minY = Infinity;
    let maxY = -Infinity;
    let cxm = 0;
    let cym = 0;
    for (const n of nodes) {
      minX = Math.min(minX, n.x - n.r);
      maxX = Math.max(maxX, n.x + n.r);
      minY = Math.min(minY, n.y - n.r);
      maxY = Math.max(maxY, n.y + n.r);
      cxm += n.x;
      cym += n.y;
    }
    cxm /= nodes.length;
    cym /= nodes.length;
    const spanX = Math.max(1, maxX - minX);
    const spanY = Math.max(1, maxY - minY);
    const padW = this.cssW - 96;
    const padH = this.cssH - 112; // extra bottom room for labels
    this.scale = clamp(
      Math.min(padW / spanX, padH / spanY),
      MIN_SCALE,
      MAX_SCALE,
    );
    this.fitScale = this.scale;
    // Centre the bbox, blended 15% toward the mass centroid (the spiral packer
    // already balances the cloud, so bbox-centre dominates — a heavier mass blend
    // biased the frame toward the densest cluster).
    const bx = (minX + maxX) / 2;
    const by = (minY + maxY) / 2;
    this.panX = -(0.85 * bx + 0.15 * cxm) * this.scale;
    this.panY = -(0.85 * by + 0.15 * cym) * this.scale;
    this.emitZoom();
  }

  private render(now: number): void {
    const ctx = this.ctx;
    const canvas = this.hostRef.nativeElement;
    const w = this.cssW;
    const h = this.cssH;
    if (!ctx || w < 4 || h < 4 || document.hidden) {
      return;
    }
    // In reduced motion the scene is a still: freeze the animation clock.
    const tSec = this.reduced ? 0 : now / 1000;
    const dpr = Math.min(2, window.devicePixelRatio || 1);
    const bw = Math.max(1, Math.round(w * dpr));
    const bh = Math.max(1, Math.round(h * dpr));
    if (canvas.width !== bw || canvas.height !== bh) {
      canvas.width = bw;
      canvas.height = bh;
      this.bg = null;
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

    if (!this.theme) {
      this.theme = this.buildTheme();
    }
    if (!this.bg) {
      this.bg = this.makeBg(w, h, dpr);
    }
    if (!this.fitted) {
      this.fit();
      this.fitted = true;
    }

    // ── backdrop (cached deep-indigo field + vignette) ──
    ctx.globalCompositeOperation = "source-over";
    ctx.globalAlpha = 1;
    ctx.drawImage(this.bg, 0, 0, w, h);

    const nodes = this.sceneNodes();
    const n = nodes.length;
    if (n === 0) {
      return;
    }

    const cx = w / 2 + this.panX;
    const cy = h / 2 + this.panY;
    const s = this.scale;
    const idIndex = new Map(nodes.map((nd, i) => [`${nd.kind}:${nd.id}`, i]));
    this.proj = nodes.map((nd) => ({
      sx: cx + nd.x * s,
      sy: cy + nd.y * s,
      sr: Math.max(2.5, nd.r * s),
    }));

    const focus = this.focus();
    const dimFloor = focus ? (focus.pinned ? 0.24 : 0.5) : 1;

    this.drawEdges(ctx, idIndex, focus, dimFloor, tSec);
    this.drawNodes(ctx, nodes, focus, dimFloor, tSec);
    this.drawLabels(ctx, w, h, nodes, focus);
    const hover = this._hoverId();
    if (hover !== null && !this.pointerDown) {
      this.drawTooltip(ctx, w, h, nodes, hover);
    }
    ctx.globalAlpha = 1;
    ctx.globalCompositeOperation = "source-over";
  }

  /**
   * SYNAPSES — every edge as a curved, glowing connection: an additive BLOOM
   * underpass (a wide soft ribbon of light) beneath a tapered CORE stroke whose
   * colour GRADIENTS between the two endpoint node hues (azure/amber/mint/orchid),
   * so connections read as vivid neural links rather than flat lines. Suggested
   * semantic links are dashed. The focus spotlight brightens the anchor's edges
   * and ghosts the rest. Firing pulses (when the animation loop runs) travel the
   * same curves — see {@link drawPulses}.
   */
  private drawEdges(
    ctx: CanvasRenderingContext2D,
    idIndex: Map<string, number>,
    focus: { set: Set<string>; anchor: string; pinned: boolean } | null,
    dimFloor: number,
    tSec: number,
  ): void {
    ctx.lineCap = "round";
    let pulsed = 0;
    for (const e of this.sceneEdges()) {
      const a = idIndex.get(`${e.srcKind}:${e.src}`);
      const b = idIndex.get(`${e.dstKind}:${e.dst}`);
      if (a === undefined || b === undefined) {
        continue;
      }
      const A = this.proj[a];
      const B = this.proj[b];
      const colA = NODE_COLORS[e.srcKind];
      const colB = NODE_COLORS[e.dstKind];
      const structural = e.kind === "wikilink" || e.kind === "companion";

      // Focus tier → alpha/width.
      let aBase = 0.5;
      let wBase = structural ? 1.7 : 1.25;
      let ghost = false;
      if (focus) {
        const touches = e.src === focus.anchor || e.dst === focus.anchor;
        const bothIn = focus.set.has(e.src) && focus.set.has(e.dst);
        if (touches) {
          aBase = 0.95;
          wBase += 0.9;
        } else if (bothIn) {
          aBase = 0.62;
        } else {
          aBase = 0.13 * (dimFloor / 0.24);
          ghost = true;
        }
      }

      // Curve geometry: quadratic bulge ~12% of length, hash-stable side.
      const mx = (A.sx + B.sx) / 2;
      const my = (A.sy + B.sy) / 2;
      const ddx = B.sx - A.sx;
      const ddy = B.sy - A.sy;
      const len = Math.hypot(ddx, ddy) || 1;
      const side = a < b ? 1 : -1;
      const cpx = mx + (-ddy / len) * len * 0.12 * side;
      const cpy = my + (ddx / len) * len * 0.12 * side;
      const q = (u: number): [number, number] => {
        const v = 1 - u;
        return [
          v * v * A.sx + 2 * v * u * cpx + u * u * B.sx,
          v * v * A.sy + 2 * v * u * cpy + u * u * B.sy,
        ];
      };
      // Trim endpoints to the soma surfaces so the synapse attaches cleanly.
      const tA = clamp((A.sr * 1.05) / len, 0, 0.24);
      const tB = clamp((B.sr * 1.05) / len, 0, 0.24);
      const span = Math.max(0.0001, 1 - tA - tB);
      const [bx0, by0] = q(tA);
      const [bx1, by1] = q(1 - tB);

      ctx.setLineDash(e.suggested ? [6, 6] : []);

      if (ghost) {
        // Out-of-neighbourhood: one dim curved stroke, still traceable.
        ctx.globalCompositeOperation = "source-over";
        ctx.globalAlpha = aBase;
        ctx.strokeStyle = rgba(mix(colA, colB, 0.5), 1);
        ctx.lineWidth = 0.8;
        ctx.beginPath();
        ctx.moveTo(bx0, by0);
        ctx.quadraticCurveTo(cpx, cpy, bx1, by1);
        ctx.stroke();
        continue;
      }

      // 1) additive BLOOM underpass — the soft light ribbon that makes it glow.
      ctx.globalCompositeOperation = "lighter";
      ctx.globalAlpha = aBase * 0.3;
      ctx.lineWidth = wBase * 3.2;
      ctx.strokeStyle = rgba(mix(colA, colB, 0.5), 1);
      ctx.beginPath();
      ctx.moveTo(bx0, by0);
      ctx.quadraticCurveTo(cpx, cpy, bx1, by1);
      ctx.stroke();

      // 2) tapered gradient CORE — brighter/wider at the somas, slim mid-span.
      ctx.globalCompositeOperation = "source-over";
      const segs = 5;
      let [px, py] = q(tA);
      for (let sg = 0; sg < segs; sg++) {
        const u1 = tA + (span * (sg + 1)) / segs;
        const um = (u1 - span / segs / 2 - tA) / span;
        const endness = Math.abs(2 * um - 1);
        const [nx, ny] = q(u1);
        ctx.globalAlpha = aBase * (0.6 + 0.4 * endness);
        ctx.lineWidth = wBase * (0.7 + 0.4 * endness);
        ctx.strokeStyle = rgba(mix(colA, colB, um), 1);
        ctx.beginPath();
        ctx.moveTo(px, py);
        ctx.lineTo(nx, ny);
        ctx.stroke();
        px = nx;
        py = ny;
      }

      // 3) FIRING PULSE — a hot orb + short ghost tail travelling src→dst (i.e.
      //    DOWN the layers: person → meeting → note → doc), the "data flowing
      //    through the network" signature. Only the strongest N edges fire.
      if (tSec > 0 && !e.suggested && pulsed < PULSE_EDGE_CAP) {
        pulsed++;
        const sprite = this.pulseSprite();
        const hh = djb2(e.key);
        const phase = (hh % 1000) / 1000;
        const period = 2.4 + (hh % 7) * 0.25; // 2.4…4.0 s, desynced per edge
        const tt = (tSec / period + phase) % 1;
        ctx.globalCompositeOperation = "lighter";
        for (let k = 3; k >= 1; k--) {
          const tg = tt - k * 0.03;
          if (tg < 0) {
            continue;
          }
          const [tx, ty] = q(tA + span * tg);
          const gs = 2.4 + (3 - k) * 0.5;
          ctx.globalAlpha = aBase * (0.05 + (3 - k) * 0.05);
          ctx.drawImage(sprite, tx - gs, ty - gs, gs * 2, gs * 2);
        }
        const [gx, gy] = q(tA + span * tt);
        ctx.globalAlpha = Math.min(1, aBase * 1.5);
        ctx.drawImage(sprite, gx - 3.6, gy - 3.6, 7.2, 7.2);
      }
    }
    ctx.setLineDash([]);
    ctx.globalCompositeOperation = "source-over";
  }

  /** Nodes — cached per-kind halo sprite (additive) + a crisp vector core.
   *  When the FX loop runs (`tSec > 0`) in-focus somas gently BREATHE (a slow
   *  glow pulse) — the "alive" signature. Only the glow breathes; the core radius
   *  and hit-test stay fixed. */
  private drawNodes(
    ctx: CanvasRenderingContext2D,
    nodes: FullSceneNode[],
    focus: { set: Set<string> } | null,
    dimFloor: number,
    tSec: number,
  ): void {
    const hover = this._hoverId();
    const selId = this.selectedId();
    for (let i = 0; i < nodes.length; i++) {
      const nd = nodes[i];
      const p = this.proj[i];
      const inSet = !focus || focus.set.has(nd.id);
      const a = inSet ? 1 : dimFloor;
      const isSel = selId === nd.id;
      const isHover = hover === nd.id;
      const col = NODE_COLORS[nd.kind];
      const glowB =
        tSec > 0 && inSet
          ? 1 + 0.18 * Math.sin(tSec * BREATH_W + (djb2(nd.id) % 628) / 100)
          : 1;

      // 1) cached halo sprite (additive `lighter` — no per-frame gradient)
      const halo = this.haloSprite(nd.kind);
      const hs = p.sr * HALO_F * 2;
      ctx.globalCompositeOperation = "lighter";
      ctx.globalAlpha = Math.min(1, a * 0.55 * glowB);
      ctx.drawImage(halo, p.sx - hs / 2, p.sy - hs / 2, hs, hs);
      if (isHover || isSel) {
        ctx.globalAlpha = a * 0.4;
        const hs2 = hs * 1.2;
        ctx.drawImage(halo, p.sx - hs2 / 2, p.sy - hs2 / 2, hs2, hs2);
      }

      // 2) crisp vector core: white-hot nucleus → hue body → darker membrane
      ctx.globalCompositeOperation = "source-over";
      const core = ctx.createRadialGradient(
        p.sx - p.sr * 0.25,
        p.sy - p.sr * 0.25,
        0,
        p.sx,
        p.sy,
        p.sr,
      );
      core.addColorStop(0, rgba(mix(col, WHITE, 0.6), 1));
      core.addColorStop(0.7, rgba(col, 1));
      core.addColorStop(1, rgba(mix(col, BG, 0.4), 1));
      ctx.globalAlpha = a;
      ctx.fillStyle = core;
      ctx.beginPath();
      ctx.arc(p.sx, p.sy, p.sr, 0, Math.PI * 2);
      ctx.fill();

      if (isSel || isHover) {
        ctx.globalAlpha = 0.95;
        ctx.strokeStyle = rgba(isSel ? col : WHITE, 0.95);
        ctx.lineWidth = isSel ? 2.4 : 1.4;
        ctx.beginPath();
        ctx.arc(p.sx, p.sy, p.sr + (isSel ? 4 : 2.5), 0, Math.PI * 2);
        ctx.stroke();
      }
    }
  }

  /**
   * COLLISION-DECLUTTERED adaptive labels (the "map-style" fix): candidates are
   * ranked (hovered → selected → focused neighbours → biggest projected somas),
   * each placed greedily and SKIPPED if its rect would overprint an already-placed
   * one — two labels never overlap. Zoom LOD: zooming IN raises the always-on
   * label budget. In focus mode the whole neighbourhood is labelled + a few ghost
   * labels survive outside for context. A dark halo keeps text readable over glow.
   */
  private drawLabels(
    ctx: CanvasRenderingContext2D,
    w: number,
    h: number,
    nodes: FullSceneNode[],
    focus: { set: Set<string> } | null,
  ): void {
    const n = nodes.length;
    if (n === 0) {
      return;
    }
    const hover = this._hoverId();
    const selId = this.selectedId();
    const t = this.theme as Theme;

    const cands: number[] = [];
    const ghost = new Set<number>();
    const push = (i: number): void => {
      if (i >= 0 && !cands.includes(i)) {
        cands.push(i);
      }
    };
    if (hover !== null) {
      push(nodes.findIndex((x) => x.id === hover));
    }
    if (selId !== null) {
      push(nodes.findIndex((x) => x.id === selId));
    }
    const bySize = nodes
      .map((_, i) => i)
      .sort((a, b) => this.proj[b].sr - this.proj[a].sr || a - b);
    const zoomK = clamp(this.scale / Math.max(0.0001, this.fitScale), 1, 3);
    const budget = Math.round(LABEL_TOP * zoomK);
    if (focus) {
      for (const i of bySize) {
        if (focus.set.has(nodes[i].id)) {
          push(i);
        }
      }
      let ghosts = 0;
      for (const i of bySize) {
        if (ghosts >= LABEL_GHOSTS) {
          break;
        }
        if (!focus.set.has(nodes[i].id)) {
          push(i);
          ghost.add(i);
          ghosts++;
        }
      }
    } else {
      let taken = 0;
      for (const i of bySize) {
        if (taken >= budget) {
          break;
        }
        push(i);
        taken++;
      }
    }

    ctx.globalCompositeOperation = "source-over";
    ctx.font = `600 12px ${t.font}`;
    ctx.textAlign = "center";
    ctx.lineJoin = "round";
    const placed: { x: number; y: number; w: number; h: number }[] = [];
    const collides = (r: { x: number; y: number; w: number; h: number }): boolean =>
      placed.some(
        (o) =>
          r.x < o.x + o.w && r.x + r.w > o.x && r.y < o.y + o.h && r.y + r.h > o.y,
      );

    for (const i of cands) {
      const nd = nodes[i];
      const p = this.proj[i];
      if (p.sr < 2 || p.sx < -60 || p.sx > w + 60 || p.sy < -24 || p.sy > h + 24) {
        continue;
      }
      const isHot = hover === nd.id || selId === nd.id;
      const isGhost = ghost.has(i);
      const label =
        nd.label.length > 26 ? nd.label.slice(0, 25) + "…" : nd.label;
      const tw = ctx.measureText(label).width;
      const spots: [number, number][] = [
        [p.sx, p.sy + p.sr * 1.2 + 14],
        [p.sx, p.sy - p.sr * 1.2 - 8],
        [p.sx + p.sr * 1.35 + tw / 2 + 7, p.sy + 4],
        [p.sx - p.sr * 1.35 - tw / 2 - 7, p.sy + 4],
      ];
      let lx = 0;
      let ly = 0;
      let rect: { x: number; y: number; w: number; h: number } | null = null;
      for (const [sx, sy] of spots) {
        const cand = { x: sx - tw / 2 - 5, y: sy - 12, w: tw + 10, h: 17 };
        if (!collides(cand)) {
          lx = sx;
          ly = sy;
          rect = cand;
          break;
        }
      }
      if (!rect) {
        continue; // hot labels are pushed first, so they always win a slot
      }
      placed.push(rect);
      ctx.globalAlpha = isHot ? 1 : isGhost ? 0.5 : 0.9;
      ctx.lineWidth = 3;
      ctx.strokeStyle = rgba(BG, 0.92);
      ctx.strokeText(label, lx, ly);
      ctx.fillStyle = rgba(isHot ? WHITE : isGhost ? LABEL_DIM : LABEL, 1);
      ctx.fillText(label, lx, ly);
    }
  }

  /** Opaque hover tooltip: label + a kind-dot meta line (kind · date · degree). */
  private drawTooltip(
    ctx: CanvasRenderingContext2D,
    w: number,
    h: number,
    nodes: FullSceneNode[],
    hoverId: string,
  ): void {
    const i = nodes.findIndex((x) => x.id === hoverId);
    if (i < 0) {
      return;
    }
    const t = this.theme as Theme;
    const nd = nodes[i];
    const p = this.proj[i];
    const parts = [KIND_LABEL[nd.kind]];
    if (nd.date) {
      parts.push(nd.date.slice(0, 10));
    }
    parts.push(`${nd.degree} connection${nd.degree === 1 ? "" : "s"}`);
    const meta = parts.join(" · ");
    const name = nd.label.length > 40 ? nd.label.slice(0, 39) + "…" : nd.label;

    ctx.textAlign = "left";
    ctx.font = `600 12.5px ${t.font}`;
    const w1 = ctx.measureText(name).width;
    ctx.font = `500 11px ${t.font}`;
    const w2 = ctx.measureText(meta).width + 12; // + the kind-dot
    const bw = Math.max(w1, w2) + 24;
    const bh = 44;
    const bx = clamp(p.sx - bw / 2, 8, w - bw - 8);
    let by = p.sy - p.sr * HALO_F - bh - 6;
    if (by < 8) {
      by = Math.min(h - bh - 8, p.sy + p.sr * HALO_F + 8);
    }
    ctx.globalAlpha = 1;
    ctx.globalCompositeOperation = "source-over";
    const r = 8;
    ctx.beginPath();
    ctx.moveTo(bx + r, by);
    ctx.arcTo(bx + bw, by, bx + bw, by + bh, r);
    ctx.arcTo(bx + bw, by + bh, bx, by + bh, r);
    ctx.arcTo(bx, by + bh, bx, by, r);
    ctx.arcTo(bx, by, bx + bw, by, r);
    ctx.closePath();
    ctx.fillStyle = rgba(t.overlay, 0.97);
    ctx.fill();
    ctx.strokeStyle = rgba(t.border, 0.9);
    ctx.lineWidth = 1;
    ctx.stroke();
    ctx.fillStyle = rgba(t.textPri, 1);
    ctx.font = `600 12.5px ${t.font}`;
    ctx.fillText(name, bx + 12, by + 18);
    ctx.beginPath();
    ctx.arc(bx + 15.5, by + 30.5, 3.5, 0, Math.PI * 2);
    ctx.fillStyle = rgba(NODE_COLORS[nd.kind], 1);
    ctx.fill();
    ctx.font = `500 11px ${t.font}`;
    ctx.fillStyle = rgba(t.textSec, 1);
    ctx.fillText(meta, bx + 24, by + 34);
    ctx.textAlign = "center";
  }

  /** Nearest node under the pointer (uses the last frame's projection). */
  private hitTest(event: PointerEvent): string | null {
    return this.hitTestXY(event.clientX, event.clientY);
  }

  private hitTestXY(clientX: number, clientY: number): string | null {
    const rect = this.hostRef.nativeElement.getBoundingClientRect();
    const px = clientX - rect.left;
    const py = clientY - rect.top;
    const nodes = this.sceneNodes();
    let best: string | null = null;
    let bestR = Infinity;
    for (let i = 0; i < nodes.length; i++) {
      const p = this.proj[i];
      if (!p) {
        continue;
      }
      const rr = Math.max(p.sr, 9) + 3;
      const dx = px - p.sx;
      const dy = py - p.sy;
      const d2 = dx * dx + dy * dy;
      if (d2 <= rr * rr && d2 < bestR) {
        bestR = d2;
        best = nodes[i].id;
      }
    }
    return best;
  }

  // ── theme + paint caches ──────────────────────────────────────────────

  private buildTheme(): Theme {
    const cs = getComputedStyle(this.hostRef.nativeElement);
    const read = (name: string, fb: Rgb): Rgb =>
      parseColor(cs.getPropertyValue(name), fb);
    return {
      textPri: read("--text-primary", [246, 246, 250]),
      textSec: read("--text-secondary", [166, 166, 182]),
      overlay: read("--surface-overlay", [27, 27, 36]),
      border: read("--border-strong", [255, 255, 255]),
      font: cs.getPropertyValue("--font-sans").trim() || "system-ui, sans-serif",
    };
  }

  /** Cached deep-indigo backdrop: vertical field gradient + corner vignette. */
  private makeBg(w: number, h: number, dpr: number): HTMLCanvasElement {
    const c = document.createElement("canvas");
    c.width = Math.max(1, Math.round(w * dpr));
    c.height = Math.max(1, Math.round(h * dpr));
    const g = c.getContext("2d");
    if (!g) {
      return c;
    }
    g.scale(dpr, dpr);
    const v = g.createLinearGradient(0, 0, 0, h);
    v.addColorStop(0, rgba(BG_LIFT, 1));
    v.addColorStop(0.6, rgba(BG, 1));
    v.addColorStop(1, rgba(mix(BG, BG_LIFT, 0.35), 1));
    g.fillStyle = v;
    g.fillRect(0, 0, w, h);
    // Two faint nebula washes in the entity/meeting hues for depth.
    const n1 = g.createRadialGradient(
      w * 0.28,
      h * 0.3,
      0,
      w * 0.28,
      h * 0.3,
      Math.max(w, h) * 0.55,
    );
    n1.addColorStop(0, rgba(NODE_COLORS.entity, 0.05));
    n1.addColorStop(1, rgba(NODE_COLORS.entity, 0));
    g.fillStyle = n1;
    g.fillRect(0, 0, w, h);
    const n2 = g.createRadialGradient(
      w * 0.74,
      h * 0.72,
      0,
      w * 0.74,
      h * 0.72,
      Math.max(w, h) * 0.5,
    );
    n2.addColorStop(0, rgba(NODE_COLORS.document, 0.045));
    n2.addColorStop(1, rgba(NODE_COLORS.document, 0));
    g.fillStyle = n2;
    g.fillRect(0, 0, w, h);
    const vg = g.createRadialGradient(
      w / 2,
      h / 2,
      Math.min(w, h) * 0.4,
      w / 2,
      h / 2,
      Math.max(w, h) * 0.78,
    );
    vg.addColorStop(0, "rgba(0,0,0,0)");
    vg.addColorStop(1, "rgba(0,0,0,0.45)");
    g.fillStyle = vg;
    g.fillRect(0, 0, w, h);
    return c;
  }

  /** Cached CAPPED halo sprite per kind — white-hot centre bleeding through a
   *  saturated hue glow, transparent by {@link HALO_F}× the soma radius. Drawn
   *  with `lighter`; the crisp core is vector-drawn on top (never in the sprite). */
  private haloSprite(kind: FullGraphNodeKind): HTMLCanvasElement {
    const key = `halo:${kind}`;
    const cached = this.sprites.get(key);
    if (cached) {
      return cached;
    }
    const col = NODE_COLORS[kind];
    const size = 128;
    const c = document.createElement("canvas");
    c.width = size;
    c.height = size;
    const g = c.getContext("2d");
    if (g) {
      const half = size / 2;
      const somaStop = 1 / HALO_F;
      const grad = g.createRadialGradient(half, half, 0, half, half, half);
      grad.addColorStop(0, rgba(mix(col, WHITE, 0.6), 0.55));
      grad.addColorStop(somaStop * 0.55, rgba(mix(col, WHITE, 0.28), 0.45));
      grad.addColorStop(somaStop, rgba(col, 0.4));
      grad.addColorStop(somaStop + (1 - somaStop) * 0.4, rgba(col, 0.12));
      grad.addColorStop(1, rgba(col, 0));
      g.fillStyle = grad;
      g.fillRect(0, 0, size, size);
    }
    this.sprites.set(key, c);
    return c;
  }

  /** Cached hot-white pulse orb sprite (white core → azure-orchid falloff). */
  private pulseSprite(): HTMLCanvasElement {
    if (this.pulseC) {
      return this.pulseC;
    }
    const size = 24;
    const c = document.createElement("canvas");
    c.width = size;
    c.height = size;
    const g = c.getContext("2d");
    if (g) {
      const half = size / 2;
      const glow = mix(NODE_COLORS.entity, NODE_COLORS.document, 0.5);
      const grad = g.createRadialGradient(half, half, 0, half, half, half);
      grad.addColorStop(0, "rgba(255,255,255,0.95)");
      grad.addColorStop(0.35, rgba(glow, 0.6));
      grad.addColorStop(1, rgba(glow, 0));
      g.fillStyle = grad;
      g.fillRect(0, 0, size, size);
    }
    this.pulseC = c;
    return c;
  }
}
