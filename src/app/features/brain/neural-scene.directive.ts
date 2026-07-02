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
import type { GraphNode } from "../../core/models";

/** A node placed by the component's deterministic 3-D layout (world units, origin-centred). */
export interface SceneNode {
  id: string;
  name: string;
  kind: GraphNode["kind"];
  mentionCount: number;
  /** Connection count (for the hover tooltip + a11y announcements). */
  degree: number;
  x: number;
  y: number;
  z: number;
  /** Soma radius in world units (~8…26 of a ~1000-unit world). */
  r: number;
}

/** An edge whose endpoints both survived the component's top-K cap. */
export interface SceneEdge {
  /** `${source}::${target}` — stable identity. */
  key: string;
  source: string;
  target: string;
  weight: number;
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

interface Dendrite {
  ang: number;
  /** Length as a multiple of the projected soma radius (1.7…3.0). */
  lenF: number;
  /** Signed perpendicular curl of the quadratic control point. */
  curl: number;
  /** Fork point along the arbor (0 = none) + its signed branch angle — the
   *  secondary branch is what reads "dendrite" instead of "lens flare" (R4). */
  fork: number;
  forkAng: number;
}

interface RenderNode {
  id: string;
  name: string;
  kind: GraphNode["kind"];
  mentionCount: number;
  degree: number;
  x: number;
  y: number;
  z: number;
  r: number;
  /** Hash-derived breathing phase so somas never pulse in lockstep. */
  phase: number;
  dendrites: Dendrite[];
}

interface RenderEdge {
  a: number;
  b: number;
  /** Normalised weight 0…1 (drives alpha/width/pulse count). */
  wn: number;
  /** Hash-stable side (±1) of the bezier control-point offset. */
  side: number;
  /** Only the strongest edges fire pulses (perf cap). */
  pulsed: boolean;
  pulses: { phase: number; period: number }[];
  colA: Rgb;
  colB: Rgb;
}

interface Projected {
  sx: number;
  sy: number;
  s: number;
  sr: number;
  depth: number;
  /** Depth-based alpha falloff (far = dimmer/softer). */
  da: number;
}

/* ── THE SCENE PALETTE — deliberately FIXED, independent of the page theme ──
 * Design review R3 (binding): "glow only works on dark — make the map canvas a
 * deep indigo/near-black surface even if the page stays light." Round 2 derived
 * these from the CSS tokens; under the LIGHT theme that produced pale lilac on
 * near-white, and every additive (`lighter`) effect — halos, firing pulses,
 * breathing — composited to pure white, i.e. was INVISIBLE. The scene is
 * artwork painted on its own dark field, so its palette is scene constants
 * (mirrored by the component's canvas background + legend dots); the DOM
 * chrome around it (toolbar, hint) stays fully tokenised. */
/** Deep indigo-black canvas floor (matches the component's `.bm-canvas` bg). */
const BG: Rgb = [8, 9, 20];
/** Slight indigo lift for the top of the backdrop gradient. */
const BG_LIFT: Rgb = [17, 19, 40];
/** People — #5bbdff azure. R3 mandate: ≥60° of hue separation from the
 *  project-purple (the page `--accent` #6e76ff sits only ~26° away — the two
 *  kinds were unreadable as an encoding on the canvas). */
const PERSON: Rgb = [91, 189, 255];
/** Projects — #9d7bff, the ONE established literal (toolbar legend + the old
 *  SVG map, `--accent-gradient`'s end stop). */
const PROJECT: Rgb = [157, 123, 255];
const WHITE: Rgb = [255, 255, 255];
/** Canvas label ink — fixed light greys (the labels sit on the dark scene). */
const LABEL: Rgb = [216, 222, 242];
const LABEL_DIM: Rgb = [148, 156, 188];

const FOCAL = 1000;
const MIN_DIST = 260;
const MAX_DIST = 12000;
const MORPH_MS = 600;
/** Cinematic auto-orbit speed (rad/s) — pauses on hover/press, stops on drag. */
const ROT_SPEED = 0.05;
/** Breathing angular frequency — a ~2.4 s period. */
const BREATH_W = (Math.PI * 2) / 2.4;
const DUST_COUNT = 40;
/** Only the strongest N edges carry firing pulses (keeps dense graphs at 60fps). */
const PULSE_EDGE_CAP = 140;
/** Base always-on label count at fit zoom; grows as the camera dollies in. */
const LABEL_TOP = 16;
/** Ghost labels kept alive OUTSIDE a focused neighbourhood (context, R3). */
const LABEL_GHOSTS = 6;
/** Halo radius as a multiple of the soma radius — CAPPED so adjacent glows
 *  never merge into one luminous blob (design review R2). */
const HALO_F = 2.6;
/** The dim FLOOR for out-of-neighbourhood nodes — ~30% ghosts, never gone
 *  (R3: context must survive selection; ~5% lost the whole map). */
const DIM_NODE = 0.3;
/** Post-selection synapse flash duration (ms). */
const FLASH_MS = 1100;

function djb2(s: string): number {
  let h = 5381;
  for (let i = 0; i < s.length; i++) {
    h = (((h << 5) + h) ^ s.charCodeAt(i)) >>> 0;
  }
  return h >>> 0;
}

/** Deterministic 0…1 derived from a hash + salt — the layout/FX "randomness". */
function r01(h: number, salt: number): number {
  let x = (h ^ Math.imul(salt + 1, 2654435761)) >>> 0;
  x = Math.imul(x ^ (x >>> 15), 2246822519) >>> 0;
  x = Math.imul(x ^ (x >>> 13), 3266489917) >>> 0;
  return ((x ^ (x >>> 16)) >>> 0) / 4294967296;
}

function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v;
}

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

/**
 * The NEURAL SCENE renderer — a perspective-projected, canvas-drawn "living
 * brain" over the graph the component lays out: neuron somas with a white-hot
 * core + saturated halo (cached sprites, no per-frame shadowBlur), tapered
 * procedural dendrites, glowing gradient synapse curves with firing pulses
 * traveling along them, a 3-D orbit / 2-D pan camera, depth fog,
 * collision-decluttered labels, a hover tooltip, ambient dust and a static
 * nebula-and-filament backdrop — all on a fixed deep-indigo field.
 *
 * ZONELESS RULE (.claude/rules/angular-zoneless.md §5): a bare
 * `requestAnimationFrame`/timer in a COMPONENT is banned — DOM-loop concerns
 * (the rAF loop, the ResizeObserver, the `prefers-reduced-motion` matchMedia
 * listener and the `visibilitychange` listener) all belong in a DIRECTIVE and
 * are ALL released in `DestroyRef.onDestroy()` below. That is exactly why this
 * renderer is a directive on the component's `<canvas>` rather than component
 * code.
 *
 * Reduced motion / power: under `prefers-reduced-motion: reduce` there is NO
 * continuous loop — no auto-rotate, pulses, breathing or dust drift; single
 * frames render on state change only (`invalidate()` on demand). The loop also
 * stops entirely while `document.hidden`.
 *
 * Hot per-frame state (camera, morph, projections) is deliberately plain
 * fields: nothing in a template reads it — the canvas repaints it directly, so
 * signal graph traffic at 60 fps would be pure overhead. Everything the
 * template/host DOES read (hover, dragging → cursor) is a signal.
 */
@Directive({
  selector: "canvas[appNeuralScene]",
  standalone: true,
  host: {
    "[style.cursor]": "cursor()",
    "(pointerdown)": "onPointerDown($event)",
    "(pointermove)": "onPointerMove($event)",
    "(pointerup)": "onPointerUp($event)",
    "(pointercancel)": "onPointerUp($event)",
    "(pointerleave)": "onPointerLeave()",
    "(wheel)": "onWheel($event)",
  },
})
export class NeuralSceneDirective {
  private readonly hostRef = inject<ElementRef<HTMLCanvasElement>>(ElementRef);
  private readonly destroyRef = inject(DestroyRef);

  /** Laid-out nodes (world coordinates centred on the origin). */
  readonly sceneNodes = input<SceneNode[]>([]);
  /** Edges between surviving nodes. */
  readonly sceneEdges = input<SceneEdge[]>([]);
  /** The focused node id (null = no focus) — drives neighbourhood dimming. */
  readonly selectedId = input<string | null>(null);
  /** Camera mode: 3-D orbit (default) or flat 2-D pan. */
  readonly mode = input<"3d" | "2d">("3d");

  /** Click: a node id, or null for empty space (the component owns selection). */
  readonly nodePick = output<string | null>();
  /** Hover enter/leave over a node. */
  readonly nodeHover = output<string | null>();

  // ── reactive state the host binding reads ─────────────────────────────
  private readonly _hoverId = signal<string | null>(null);
  private readonly _dragging = signal(false);
  protected readonly cursor = computed(() =>
    this._dragging()
      ? "grabbing"
      : this._hoverId() !== null
        ? "pointer"
        : "grab",
  );

  /** Selected node + its one-hop neighbours (null = no focus). */
  private readonly neighborSet = computed<Set<string> | null>(() => {
    const sel = this.selectedId();
    if (!sel) {
      return null;
    }
    const set = new Set<string>([sel]);
    for (const e of this.sceneEdges()) {
      if (e.source === sel) {
        set.add(e.target);
      } else if (e.target === sel) {
        set.add(e.source);
      }
    }
    return set;
  });

  // ── plain render state (hot path — see class doc) ─────────────────────
  private renderNodes: RenderNode[] = [];
  private renderEdges: RenderEdge[] = [];
  private proj: Projected[] = [];
  private sortIdx: number[] = [];
  private boundR = 400;
  private readonly dust = Array.from({ length: DUST_COUNT }, (_, i) => {
    const h = djb2(`dust-${i}`);
    return {
      x: (r01(h, 1) - 0.5) * 1150,
      y: (r01(h, 2) - 0.5) * 900,
      z: (r01(h, 3) - 0.5) * 900,
      a: 0.05 + r01(h, 4) * 0.07,
      sp: 0.15 + r01(h, 5) * 0.25,
      ph: r01(h, 6) * Math.PI * 2,
    };
  });

  // camera
  private camYaw = -0.5;
  private camPitch = 0.32;
  private camDist = 2200;
  /** The distance the last auto-fit chose — the label-LOD zoom reference. */
  private fitDist = 2200;
  private panX = 0;
  private panY = 0;
  /** Set on wheel/drag/toolbar-zoom; cleared by reset — mirrors the old SVG
   *  `touched` semantics so auto-fit (and auto-rotate) never stomp the user. */
  private touched = false;
  private needsFit = true;

  // 3d↔2d morph (0 = full 3-D, 1 = flat)
  private morph = 0;
  private morphFrom = 0;
  private morphTarget = 0;
  private morphStart = 0;

  // selection flash (a light pulse down the selected node's synapses)
  private selFlashAt = -Infinity;
  private prevSel: string | null = null;

  // pointer
  private pointerDown = false;
  private dragMoved = false;
  private downX = 0;
  private downY = 0;
  private lastPX = 0;
  private lastPY = 0;

  // loop / lifecycle
  private cssW = 0;
  private cssH = 0;
  private rafId: number | null = null;
  private oneShotId: number | null = null;
  private loopRunning = false;
  private lastT = 0;
  private reduced: boolean;

  // paint caches
  private ctx: CanvasRenderingContext2D | null;
  private theme: Theme | null = null;
  private bg: HTMLCanvasElement | null = null;
  private readonly sprites = new Map<string, HTMLCanvasElement>();
  private pulseC: HTMLCanvasElement | null = null;

  private readonly media = window.matchMedia(
    "(prefers-reduced-motion: reduce)",
  );
  private readonly onMedia = (): void => {
    this.reduced = this.media.matches;
    if (this.reduced) {
      this.stopLoop();
      this.morph = this.morphTarget; // snap the 3d↔2d transition
    } else {
      this.startLoop();
    }
    this.invalidate();
  };
  private readonly onVisibility = (): void => {
    if (document.hidden) {
      this.stopLoop();
    } else if (this.reduced) {
      this.invalidate();
    } else {
      this.startLoop();
    }
  };
  /** The app can flip light/dark at runtime — the SCENE palette is fixed, but
   *  the tooltip chrome follows the app tokens, so re-read them + repaint. */
  private readonly scheme = window.matchMedia("(prefers-color-scheme: light)");
  private readonly onScheme = (): void => this.refreshTheme();
  private readonly themeMo = new MutationObserver(() => this.refreshTheme());
  private readonly ro = new ResizeObserver((entries) => {
    const rect = entries[entries.length - 1]?.contentRect;
    if (!rect || rect.width < 2 || rect.height < 2) {
      return; // mounted collapsed at 0×0 — wait for a real size
    }
    const wasZero = this.cssW < 2 || this.cssH < 2;
    this.cssW = rect.width;
    this.cssH = rect.height;
    this.bg = null;
    if (wasZero && !this.touched) {
      this.needsFit = true;
    }
    this.invalidate();
  });

  constructor() {
    this.ctx = this.hostRef.nativeElement.getContext("2d");
    this.reduced = this.media.matches;
    this.media.addEventListener("change", this.onMedia);
    this.scheme.addEventListener("change", this.onScheme);
    this.themeMo.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    document.addEventListener("visibilitychange", this.onVisibility);
    this.ro.observe(this.hostRef.nativeElement);

    // Rebuild render caches when the graph inputs change; clears a stale hover
    // (a signal WRITE inside a tracked effect → allowSignalWrites, the Angular
    // 18 NG0600 guard). The hover read is untracked so hovering doesn't rebuild.
    effect(
      () => {
        const nodes = this.sceneNodes();
        const edges = this.sceneEdges();
        this.rebuild(nodes, edges);
        const hover = untracked(() => this._hoverId());
        if (hover !== null && !nodes.some((n) => n.id === hover)) {
          this._hoverId.set(null);
          this.nodeHover.emit(null);
        }
        if (!this.touched) {
          this.needsFit = true;
        }
        this.invalidate();
      },
      { allowSignalWrites: true },
    );

    // Ease the 3d↔2d morph on mode change (snapped under reduced motion).
    effect(() => {
      const target = this.mode() === "2d" ? 1 : 0;
      if (target !== this.morphTarget) {
        this.morphFrom = this.morph;
        this.morphTarget = target;
        this.morphStart = performance.now();
        if (this.reduced) {
          this.morph = target;
        }
      }
      this.invalidate();
    });

    // Repaint on selection/hover change; a NEW selection also arms the
    // synapse-flash so light briefly pulses down the selected node's edges.
    effect(() => {
      const sel = this.selectedId();
      this._hoverId();
      if (sel !== this.prevSel) {
        this.prevSel = sel;
        if (sel !== null && !this.reduced) {
          this.selFlashAt = performance.now();
        }
      }
      this.invalidate();
    });

    if (!this.reduced) {
      this.startLoop();
    }

    this.destroyRef.onDestroy(() => {
      this.stopLoop();
      if (this.oneShotId !== null) {
        cancelAnimationFrame(this.oneShotId);
        this.oneShotId = null;
      }
      this.ro.disconnect();
      this.themeMo.disconnect();
      this.media.removeEventListener("change", this.onMedia);
      this.scheme.removeEventListener("change", this.onScheme);
      document.removeEventListener("visibilitychange", this.onVisibility);
    });
  }

  /** Re-read the tooltip tokens + repaint (app theme flip). */
  private refreshTheme(): void {
    this.theme = null;
    this.invalidate();
  }

  // ── public camera API (the component's toolbar drives these) ──────────

  /** Dolly the camera about the canvas centre (factor < 1 = zoom in). */
  zoomBy(factor: number): void {
    this.touched = true;
    const old = this.camDist;
    this.camDist = clamp(old * factor, MIN_DIST, MAX_DIST);
    const k = old / this.camDist;
    this.panX *= k;
    this.panY *= k;
    this.invalidate();
  }

  /** Re-fit + re-arm auto-fit/auto-rotate (clears the `touched` flag). */
  resetView(): void {
    this.touched = false;
    this.camYaw = -0.5;
    this.camPitch = 0.32;
    this.panX = 0;
    this.panY = 0;
    this.needsFit = true;
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
        this.touched = true; // the user took control — auto-rotate stops
        this._dragging.set(true);
      }
      if (this.dragMoved) {
        if (this.morphTarget === 1) {
          this.panX += dx;
          this.panY += dy;
        } else {
          this.camYaw += dx * 0.005;
          this.camPitch = clamp(this.camPitch + dy * 0.005, -1.25, 1.25);
        }
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

  /** Wheel = dolly zoom toward the cursor (the point under it stays put). */
  protected onWheel(event: WheelEvent): void {
    event.preventDefault();
    this.touched = true;
    const rect = this.hostRef.nativeElement.getBoundingClientRect();
    const ox = event.clientX - rect.left - rect.width / 2;
    const oy = event.clientY - rect.top - rect.height / 2;
    const factor = Math.exp(clamp(event.deltaY, -80, 80) * 0.0016);
    const old = this.camDist;
    this.camDist = clamp(old * factor, MIN_DIST, MAX_DIST);
    const k = old / this.camDist; // ≈ screen-scale multiplier at the focus plane
    this.panX = ox - (ox - this.panX) * k;
    this.panY = oy - (oy - this.panY) * k;
    this.invalidate();
  }

  // ── loop management ───────────────────────────────────────────────────

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
    this.lastT = 0;
  }

  /** One-shot render for invalidate-on-demand (reduced motion / idle states). */
  private invalidate(): void {
    if (this.loopRunning || this.oneShotId !== null || document.hidden) {
      return;
    }
    this.oneShotId = requestAnimationFrame((t) => {
      this.oneShotId = null;
      this.render(t);
    });
  }

  // ── scene build ───────────────────────────────────────────────────────

  private rebuild(nodes: SceneNode[], edges: SceneEdge[]): void {
    const idx = new Map(nodes.map((n, i) => [n.id, i]));
    this.renderNodes = nodes.map((n) => {
      const h = djb2(n.id);
      const count = 4 + (h % 4); // 4…7 dendrites, deterministic per id
      const dendrites: Dendrite[] = [];
      for (let k = 0; k < count; k++) {
        dendrites.push({
          ang: r01(h, k * 5 + 1) * Math.PI * 2,
          lenF: 1.7 + r01(h, k * 5 + 2) * 1.3,
          // Strong curl — straight radial strokes read as diffraction spikes
          // ("stars, not neurons" — design-panel R4).
          curl: (r01(h, k * 5 + 3) - 0.5) * 2.2,
          fork: r01(h, k * 5 + 4) < 0.55 ? 0.45 + r01(h, k * 5 + 5) * 0.25 : 0,
          forkAng: (r01(h, k * 5 + 5) - 0.5) * 1.6,
        });
      }
      return {
        id: n.id,
        name: n.name,
        kind: n.kind,
        mentionCount: n.mentionCount,
        degree: n.degree,
        x: n.x,
        y: n.y,
        z: n.z,
        r: n.r,
        phase: r01(h, 97) * Math.PI * 2,
        dendrites,
      };
    });

    let boundR = 120;
    for (const n of nodes) {
      boundR = Math.max(boundR, Math.hypot(n.x, n.y, n.z) + n.r);
    }
    this.boundR = boundR;

    const kept = edges.filter((e) => idx.has(e.source) && idx.has(e.target));
    const maxW = kept.reduce((m, e) => Math.max(m, e.weight), 1);
    const pulsedKeys = new Set(
      [...kept]
        .sort((a, b) => b.weight - a.weight)
        .slice(0, PULSE_EDGE_CAP)
        .map((e) => e.key),
    );
    this.renderEdges = kept.map((e) => {
      const h = djb2(e.key);
      const wn = e.weight / maxW;
      const count = 1 + Math.round(wn * 2); // 1…3 pulses, weight-scaled
      const pulses: { phase: number; period: number }[] = [];
      for (let k = 0; k < count; k++) {
        pulses.push({
          phase: r01(h, k + 11),
          period: 2 + r01(h, k + 41) * 2, // 2…4 s
        });
      }
      const a = idx.get(e.source) as number;
      const b = idx.get(e.target) as number;
      return {
        a,
        b,
        wn,
        side: r01(h, 7) < 0.5 ? -1 : 1,
        pulsed: pulsedKeys.has(e.key),
        pulses,
        colA: nodes[a].kind === "project" ? PROJECT : PERSON,
        colB: nodes[b].kind === "project" ? PROJECT : PERSON,
      };
    });
    this.proj = new Array<Projected>(this.renderNodes.length);
    this.sortIdx = this.renderNodes.map((_, i) => i);
  }

  // ── painting ──────────────────────────────────────────────────────────

  private render(now: number): void {
    const ctx = this.ctx;
    const canvas = this.hostRef.nativeElement;
    const w = this.cssW;
    const h = this.cssH;
    if (!ctx || w < 4 || h < 4 || document.hidden) {
      return;
    }
    const dpr = Math.min(2, window.devicePixelRatio || 1);
    const bw = Math.max(1, Math.round(w * dpr));
    const bh = Math.max(1, Math.round(h * dpr));
    if (canvas.width !== bw || canvas.height !== bh) {
      canvas.width = bw;
      canvas.height = bh;
      this.bg = null;
    }
    if (!this.theme) {
      this.theme = this.buildTheme();
    }
    if (!this.bg) {
      this.bg = this.makeBg(w, h, dpr);
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

    const dtMs = this.lastT === 0 ? 16 : Math.min(50, now - this.lastT);
    this.lastT = now;
    const dt = dtMs / 1000;
    // In reduced motion the scene is a still: freeze the animation clock.
    const tSec = this.reduced ? 0 : now / 1000;

    // 3d↔2d morph easing (~600 ms, ease-in-out cubic; snapped when reduced).
    if (this.morph !== this.morphTarget) {
      if (this.reduced) {
        this.morph = this.morphTarget;
      } else {
        const p = clamp((now - this.morphStart) / MORPH_MS, 0, 1);
        const eased = p < 0.5 ? 4 * p * p * p : 1 - Math.pow(-2 * p + 2, 3) / 2;
        this.morph =
          p >= 1
            ? this.morphTarget
            : this.morphFrom + (this.morphTarget - this.morphFrom) * eased;
      }
    }

    // AUTO-FIT: while the user hasn't taken control, keep the projected cloud
    // framed every frame (the auto-orbit changes the silhouette; a one-shot
    // fit would slowly drift off-frame). Cheap: one rotate+project of ≤60
    // nodes. `touched` (wheel/drag/toolbar-zoom) hands the camera to the user;
    // Reset clears it — the old SVG map's semantics.
    if ((this.needsFit || !this.touched) && this.renderNodes.length > 0) {
      this.fitCamera(w, h);
      this.needsFit = false;
    }

    const hover = this._hoverId();
    const selId = this.selectedId();
    const focus = this.neighborSet();

    // Auto-rotate: slow yaw drift; pauses while pressed/hovering, permanently
    // stops once the user drags (touched); off in 2-D and under reduced motion.
    if (
      !this.reduced &&
      !this.touched &&
      !this.pointerDown &&
      hover === null &&
      this.morphTarget === 0
    ) {
      this.camYaw += ROT_SPEED * dt;
    }

    // ── background (cached nebula + filaments + vignette) ──
    ctx.globalCompositeOperation = "source-over";
    ctx.globalAlpha = 1;
    ctx.drawImage(this.bg, 0, 0, w, h);

    // ── shared projection ──
    const f = 1 - this.morph; // 1 = full 3-D, 0 = flat straight-on
    const yaw = this.camYaw * f;
    const pitch = this.camPitch * f;
    const cosY = Math.cos(yaw);
    const sinY = Math.sin(yaw);
    const cosP = Math.cos(pitch);
    const sinP = Math.sin(pitch);
    const cx0 = w / 2 + this.panX;
    const cy0 = h / 2 + this.panY;
    const dist = this.camDist;
    const boundR = Math.max(1, this.boundR);
    // Anamorphic view-space stretch: the layout cloud is near-spherical but the
    // canvas is wide — stretch projected x so the composition FILLS the frame
    // instead of huddling in a centre column (R3). Positions only; radii stay
    // circular. fitCamera applies the same factor.
    const stretchX = this.stretchX(w, h);
    const project = (x: number, y: number, z: number): Projected => {
      const zz = z * f;
      const x1 = x * cosY + zz * sinY;
      const z1 = -x * sinY + zz * cosY;
      const y2 = y * cosP - z1 * sinP;
      const z2 = y * sinP + z1 * cosP;
      const s = FOCAL / Math.max(60, dist - z2);
      // Depth layers: back of the cloud falls to 0.3, front holds 1.0 — the
      // R3 depth cue (far = smaller, dimmer, fogged; near = big and crisp).
      const da3 = clamp(0.3 + 0.7 * ((z2 + boundR) / (2 * boundR)), 0.26, 1);
      return {
        sx: cx0 + x1 * stretchX * s,
        sy: cy0 + y2 * s,
        s,
        sr: 0,
        depth: z2,
        da: 1 - f * (1 - da3), // flat mode has no depth fog
      };
    };

    // ── ambient dust (1 px motes, slow 3-D drift) ──
    for (const m of this.dust) {
      const ox = this.reduced ? 0 : Math.sin(tSec * m.sp + m.ph) * 26;
      const oy = this.reduced
        ? 0
        : Math.cos(tSec * m.sp * 0.7 + m.ph * 1.3) * 20;
      const p = project(m.x + ox, m.y + oy, m.z);
      if (p.sx < -4 || p.sx > w + 4 || p.sy < -4 || p.sy > h + 4) {
        continue;
      }
      ctx.fillStyle = rgba(LABEL, m.a * p.da);
      ctx.fillRect(p.sx, p.sy, 1.2, 1.2);
    }

    // ── project nodes ──
    const n = this.renderNodes.length;
    for (let i = 0; i < n; i++) {
      const nd = this.renderNodes[i];
      const p = project(nd.x, nd.y, nd.z);
      p.sr = nd.r * p.s;
      this.proj[i] = p;
    }

    // ── synapses: glow underpass + tapered gradient curves + firing pulses ──
    const segs = this.renderEdges.length > 220 ? 5 : 9;
    const flashAge = now - this.selFlashAt;
    for (const e of this.renderEdges) {
      const A = this.proj[e.a];
      const B = this.proj[e.b];
      const na = this.renderNodes[e.a];
      const nb = this.renderNodes[e.b];
      const inFocus = !focus || (focus.has(na.id) && focus.has(nb.id));
      const touchesSel = selId !== null && (na.id === selId || nb.id === selId);
      const hoverHit = hover !== null && (na.id === hover || nb.id === hover);

      const mx = (A.sx + B.sx) / 2;
      const my = (A.sy + B.sy) / 2;
      const ddx = B.sx - A.sx;
      const ddy = B.sy - A.sy;
      const len = Math.hypot(ddx, ddy) || 1;
      // Control point ~12% of length, perpendicular, hash-stable side.
      const cpx = mx + (-ddy / len) * len * 0.12 * e.side;
      const cpy = my + (ddx / len) * len * 0.12 * e.side;
      const q = (u: number): [number, number] => {
        const v = 1 - u;
        return [
          v * v * A.sx + 2 * v * u * cpx + u * u * B.sx,
          v * v * A.sy + 2 * v * u * cpy + u * u * B.sy,
        ];
      };

      // OUT-OF-FOCUS edges: one dimmed stroke — ~30% ghosts, still traceable
      // (R3: context must survive selection, never fade to nothing).
      if (!inFocus) {
        ctx.globalCompositeOperation = "source-over";
        ctx.globalAlpha = (0.1 + e.wn * 0.08) * ((A.da + B.da) / 2);
        ctx.strokeStyle = rgba(mix(e.colA, e.colB, 0.5), 1);
        ctx.lineWidth = 0.8;
        ctx.beginPath();
        ctx.moveTo(A.sx, A.sy);
        ctx.quadraticCurveTo(cpx, cpy, B.sx, B.sy);
        ctx.stroke();
        continue;
      }

      // Base alpha/width from weight × depth; focus/hover boosts. Tuned so
      // every synapse READS as a drawn connection at rest (R3 + R4: "edges
      // are invisible at rest" is the recurring hard fail — the weight ramp
      // 0.45→0.85 alpha / 1.3→3.7 px must be legible on the dark field) and
      // a focused neighbourhood renders at FULL strength regardless of depth.
      const depthA =
        focus && inFocus
          ? Math.max(0.9, (A.da + B.da) / 2)
          : 0.5 + 0.5 * ((A.da + B.da) / 2);
      let aBase = (0.45 + e.wn * 0.4) * depthA;
      let wBase = 1.3 + e.wn * 2.4;
      if (touchesSel) {
        aBase = Math.max(0.85, aBase * 2);
        wBase += 1.1;
      } else if (focus) {
        aBase = Math.min(0.8, aBase * 1.7);
      }
      if (hoverHit) {
        aBase = Math.min(0.95, aBase * 1.8);
        wBase += 0.6;
      }

      // Trim endpoints to the soma surfaces so each synapse visibly ATTACHES
      // to its two cell bodies instead of vanishing under the glow.
      const tA = clamp((A.sr * 1.05) / len, 0, 0.24);
      const tB = clamp((B.sr * 1.05) / len, 0, 0.24);
      const span = 1 - tA - tB;

      // 1) additive BLOOM underpass — a wide soft ribbon of light beneath the
      //    core stroke (this is what makes a synapse luminesce on the dark
      //    field instead of reading as a hairline).
      ctx.globalCompositeOperation = "lighter";
      ctx.lineCap = "round";
      ctx.globalAlpha = aBase * 0.3;
      ctx.lineWidth = wBase * 3.4;
      ctx.strokeStyle = rgba(mix(e.colA, e.colB, 0.5), 1);
      ctx.beginPath();
      const [bx0, by0] = q(tA);
      const [bx1, by1] = q(1 - tB);
      ctx.moveTo(bx0, by0);
      ctx.quadraticCurveTo(cpx, cpy, bx1, by1);
      ctx.stroke();

      // 2) tapered gradient core: brighter + wider at the somas, slim mid-span.
      ctx.globalCompositeOperation = "source-over";
      let [px, py] = q(tA);
      for (let s = 0; s < segs; s++) {
        const u0 = tA + (span * s) / segs;
        const u1 = tA + (span * (s + 1)) / segs;
        const um = ((u0 + u1) / 2 - tA) / Math.max(0.0001, span);
        const endness = Math.abs(2 * um - 1); // 1 at endpoints, 0 mid
        const prof = 0.6 + 0.4 * Math.pow(endness, 1.1);
        const [nx, ny] = q(u1);
        ctx.globalAlpha = aBase * prof;
        ctx.lineWidth = wBase * (0.6 + 0.55 * endness);
        ctx.strokeStyle = rgba(mix(e.colA, e.colB, um), 1);
        ctx.beginPath();
        ctx.moveTo(px, py);
        ctx.lineTo(nx, ny);
        ctx.stroke();
        px = nx;
        py = ny;
      }

      // Firing pulses — hot orbs + short ghost tails along the curve. A
      // focused neighbourhood fires faster + brighter (the "alive" signature).
      if (!this.reduced && e.pulsed) {
        const rate = touchesSel ? 1.6 : 1;
        const sprite = this.pulseSprite();
        const vis =
          Math.min(1, aBase * 1.8 + 0.35) * (hoverHit || touchesSel ? 1.25 : 1);
        ctx.globalCompositeOperation = "lighter";
        for (const pu of e.pulses) {
          const tt = (tSec / (pu.period / rate) + pu.phase) % 1;
          for (let g = 4; g >= 1; g--) {
            const tg = tt - g * 0.02;
            if (tg < 0) {
              continue;
            }
            const [gx, gy] = q(tg);
            const gs = 2.6 + (4 - g) * 0.55;
            ctx.globalAlpha = vis * (0.06 + (4 - g) * 0.06);
            ctx.drawImage(sprite, gx - gs, gy - gs, gs * 2, gs * 2);
          }
          const [qx, qy] = q(tt);
          ctx.globalAlpha = vis * 0.9;
          ctx.drawImage(sprite, qx - 3.6, qy - 3.6, 7.2, 7.2);
        }
        // Selection flash: one bright pulse racing OUT of the just-selected
        // node along each of its synapses.
        if (touchesSel && flashAge >= 0 && flashAge < FLASH_MS) {
          const ft = flashAge / FLASH_MS;
          const eased = 1 - Math.pow(1 - ft, 2);
          const u = na.id === selId ? eased : 1 - eased;
          const [fx, fy] = q(u);
          ctx.globalAlpha = (1 - ft) * 0.9;
          ctx.drawImage(sprite, fx - 4.5, fy - 4.5, 9, 9);
        }
      }
    }

    // ── neurons: painter-sorted (far → near) ──
    this.sortIdx.sort((a, b) => this.proj[a].depth - this.proj[b].depth);
    for (const i of this.sortIdx) {
      const nd = this.renderNodes[i];
      const p = this.proj[i];
      const isSel = selId === nd.id;
      const isHover = hover === nd.id;
      // Out-of-neighbourhood dims to a READABLE ~30% ghost, never to smudge;
      // the focused neighbourhood is lifted out of the depth fog (fully lit)
      // so selection FLARES the neighbourhood instead of fading the map (R3).
      const inSet = !focus || focus.has(nd.id);
      const focusA = inSet ? 1 : DIM_NODE;
      if (focus && inSet) {
        p.da = Math.max(p.da, 0.9);
      }
      const animate = !this.reduced && focusA === 1;
      const breathe = animate
        ? 1 + 0.05 * Math.sin(tSec * BREATH_W + nd.phase)
        : 1;
      const glowB = animate
        ? 1 + 0.16 * Math.sin(tSec * BREATH_W + nd.phase + 1.1)
        : 1;
      const sr = p.sr * breathe;
      if (sr < 0.8) {
        continue;
      }
      // ≥0.2 visibility floor even for deep, out-of-focus somas.
      const a = Math.max(0.2, Math.min(1, p.da * focusA));
      const col = nd.kind === "project" ? PROJECT : PERSON;
      // Depth fog: far somas drift toward the field colour (R3 DOF cue)…
      const fog = focus && inSet ? 0 : (1 - p.da) * 0.5;
      const colD = fog > 0.02 ? mix(col, BG_LIFT, fog) : col;
      // …and lose their crisp rim; foreground somas get a hard cell membrane.
      const crisp = clamp((p.da - 0.5) / 0.35, 0, 1);

      // 1) CAPPED halo (cached sprite, additive) — ≤ HALO_F × soma radius,
      //    hue-forward (the near-white sprite core washed dense clusters to
      //    one white clump and erased the person/project encoding — R4).
      //    2-D mode tightens the halo so the flat projection stays crisp.
      const halo = this.haloSprite(nd.kind);
      const hs = sr * HALO_F * 2 * (1 - 0.22 * this.morph);
      ctx.globalCompositeOperation = "lighter";
      ctx.globalAlpha = Math.min(1, a * 0.58 * glowB);
      ctx.drawImage(halo, p.sx - hs / 2, p.sy - hs / 2, hs, hs);
      if (isHover || isSel) {
        // Bloom step-up: the hovered/selected soma visibly flares.
        ctx.globalAlpha = a * 0.42;
        const hs2 = hs * 1.22;
        ctx.drawImage(halo, p.sx - hs2 / 2, p.sy - hs2 / 2, hs2, hs2);
      }

      // 2) Tapered DENDRITE arbors — thin, strongly-curled strokes in the
      //    soma hue, ~half carrying a secondary FORK branch. Kept faint and
      //    irregular: bright straight radials read as lens-flare spikes
      //    ("stars, not neurons" — design-panel R4).
      ctx.globalCompositeOperation = "source-over";
      if (sr > 3 && focusA > 0.5) {
        ctx.lineCap = "round";
        for (const d of nd.dendrites) {
          const ux = Math.cos(d.ang);
          const uy = Math.sin(d.ang);
          const L = d.lenF * sr;
          const x0 = p.sx + ux * sr * 0.92;
          const y0 = p.sy + uy * sr * 0.92;
          const xm = p.sx + ux * (sr + L * 0.55) - uy * d.curl * L * 0.35;
          const ym = p.sy + uy * (sr + L * 0.55) + ux * d.curl * L * 0.35;
          const x2 = p.sx + ux * (sr + L);
          const y2 = p.sy + uy * (sr + L);
          // Four sub-segments along the quadratic, tapering width + alpha.
          let qx = x0;
          let qy = y0;
          for (let s = 0; s < 4; s++) {
            const u = (s + 1) / 4;
            const v = 1 - u;
            const nx = v * v * x0 + 2 * v * u * xm + u * u * x2;
            const ny = v * v * y0 + 2 * v * u * ym + u * u * y2;
            ctx.globalAlpha = a * (0.42 - s * 0.09);
            ctx.lineWidth = Math.max(0.4, sr * (0.12 - s * 0.025));
            ctx.strokeStyle = rgba(colD, 1);
            ctx.beginPath();
            ctx.moveTo(qx, qy);
            ctx.lineTo(nx, ny);
            ctx.stroke();
            // Fork: a short secondary branch peeling off mid-arbor.
            if (d.fork > 0 && u >= d.fork && u - 0.25 < d.fork) {
              const segA = Math.atan2(ny - qy, nx - qx) + d.forkAng;
              const bl = L * 0.42;
              ctx.globalAlpha = a * 0.26;
              ctx.lineWidth = Math.max(0.4, sr * 0.06);
              ctx.beginPath();
              ctx.moveTo(nx, ny);
              ctx.lineTo(nx + Math.cos(segA) * bl, ny + Math.sin(segA) * bl);
              ctx.stroke();
            }
            qx = nx;
            qy = ny;
          }
        }
      }

      // 3) CRISP soma core — a defined cell body (vector, occludes what's
      //    behind): white-hot nucleus → hue body → darker membrane edge.
      const core = ctx.createRadialGradient(
        p.sx - sr * 0.22,
        p.sy - sr * 0.22,
        0,
        p.sx,
        p.sy,
        sr,
      );
      core.addColorStop(0, rgba(mix(colD, WHITE, 0.92), 1));
      core.addColorStop(0.32, rgba(mix(colD, WHITE, 0.45), 1));
      core.addColorStop(0.78, rgba(colD, 1));
      core.addColorStop(1, rgba(mix(colD, BG, 0.45), 1));
      ctx.globalAlpha = a;
      ctx.fillStyle = core;
      ctx.beginPath();
      ctx.arc(p.sx, p.sy, sr, 0, Math.PI * 2);
      ctx.fill();
      // Rim: the crisp cell-membrane edge (fades for far/background somas).
      if (crisp > 0.05) {
        ctx.globalAlpha = a * (0.45 + 0.45 * crisp);
        ctx.strokeStyle = rgba(mix(colD, WHITE, 0.6), 1);
        ctx.lineWidth = Math.max(0.7, sr * 0.09);
        ctx.beginPath();
        ctx.arc(p.sx, p.sy, sr, 0, Math.PI * 2);
        ctx.stroke();
      }

      // 4) Selection marker: a THICK accent-hue ring + a white inner ring +
      //    an animated firing halo — unambiguous even beside a bright
      //    neighbour (R3: the old ring was too faint).
      if (isSel) {
        ctx.globalAlpha = 0.95;
        ctx.strokeStyle = rgba(col, 0.95);
        ctx.lineWidth = 2.6;
        ctx.beginPath();
        ctx.arc(p.sx, p.sy, sr * 1.45 + 3, 0, Math.PI * 2);
        ctx.stroke();
        ctx.globalAlpha = 0.9;
        ctx.strokeStyle = rgba(WHITE, 0.9);
        ctx.lineWidth = 1.2;
        ctx.beginPath();
        ctx.arc(p.sx, p.sy, sr * 1.2 + 1.5, 0, Math.PI * 2);
        ctx.stroke();
        if (!this.reduced) {
          const ringP = (tSec * 0.9 + nd.phase) % 1;
          ctx.globalAlpha = (1 - ringP) * 0.55;
          ctx.strokeStyle = rgba(col, 1);
          ctx.lineWidth = 1.4;
          ctx.beginPath();
          ctx.arc(p.sx, p.sy, sr * (1.55 + ringP * 0.9) + 3, 0, Math.PI * 2);
          ctx.stroke();
        }
      }
    }

    this.drawLabels(ctx, w, h, hover, selId, focus);
    if (hover !== null && !this.pointerDown) {
      this.drawTooltip(ctx, w, h, hover);
    }
    ctx.globalAlpha = 1;
    ctx.globalCompositeOperation = "source-over";
  }

  /**
   * COLLISION-DECLUTTERED labels: candidates are ranked (hovered → selected →
   * focused neighbours → biggest projected somas, i.e. mentions × zoom), each
   * is placed greedily and SKIPPED if its rect would overprint an already
   * placed one — two labels never overlap. Zoom LOD: dollying in raises the
   * always-on label budget. In FOCUS mode the whole neighbourhood is labelled
   * and the top few outside nodes keep faint ghost labels (context, R3).
   * Strong dark halo keeps text readable over glow.
   */
  private drawLabels(
    ctx: CanvasRenderingContext2D,
    w: number,
    h: number,
    hover: string | null,
    selId: string | null,
    focus: Set<string> | null,
  ): void {
    const t = this.theme as Theme;
    const n = this.renderNodes.length;
    if (n === 0) {
      return;
    }
    // Priority queue of candidate indices (deduped as we place).
    const cands: number[] = [];
    const ghost = new Set<number>();
    const push = (i: number): void => {
      if (i >= 0 && !cands.includes(i)) {
        cands.push(i);
      }
    };
    if (hover !== null) {
      push(this.renderNodes.findIndex((x) => x.id === hover));
    }
    if (selId !== null) {
      push(this.renderNodes.findIndex((x) => x.id === selId));
    }
    const bySize = [...this.sortIdx].sort(
      (a, b) => this.proj[b].sr - this.proj[a].sr,
    );
    const zoomK = clamp(this.fitDist / Math.max(1, this.camDist), 1, 3);
    const budget = Math.round(LABEL_TOP * zoomK);
    if (focus) {
      // Focused: label the WHOLE neighbourhood first…
      for (const i of bySize) {
        if (focus.has(this.renderNodes[i].id)) {
          push(i);
        }
      }
      // …then keep faint ghost labels on the biggest outside somas.
      let ghosts = 0;
      for (const i of bySize) {
        if (ghosts >= LABEL_GHOSTS) {
          break;
        }
        if (!focus.has(this.renderNodes[i].id)) {
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
    ctx.font = `600 13px ${t.font}`;
    ctx.textAlign = "center";
    ctx.lineJoin = "round";
    const placed: { x: number; y: number; w: number; h: number }[] = [];
    const collides = (r: { x: number; y: number; w: number; h: number }) =>
      placed.some(
        (o) =>
          r.x < o.x + o.w && r.x + r.w > o.x && r.y < o.y + o.h && r.y + r.h > o.y,
      );

    for (const i of cands) {
      const nd = this.renderNodes[i];
      const p = this.proj[i];
      if (p.sr < 1.4 || p.sx < -40 || p.sx > w + 40 || p.sy < -20 || p.sy > h + 20) {
        continue;
      }
      const isHot = hover === nd.id || selId === nd.id;
      const isGhost = ghost.has(i);
      const fade = clamp((p.da - 0.35) / 0.65, 0, 1);
      // Ghosts at 0.42 — 0.32 sat right at the legibility floor (R4).
      const la = isHot ? 1 : isGhost ? 0.42 : 0.68 + 0.32 * fade;
      const tw = ctx.measureText(nd.name).width;
      // FAN-OUT placement: below → above → right → left of the soma. A
      // spatially tight focused neighbourhood needs the side slots — with
      // only below/above its labels piled up or vanished (design-panel R4).
      const spots: [number, number][] = [
        [p.sx, p.sy + p.sr * 1.2 + 15],
        [p.sx, p.sy - p.sr * 1.2 - 8],
        [p.sx + p.sr * 1.35 + tw / 2 + 7, p.sy + 4],
        [p.sx - p.sr * 1.35 - tw / 2 - 7, p.sy + 4],
      ];
      let lx = 0;
      let ly = 0;
      let rect: { x: number; y: number; w: number; h: number } | null = null;
      for (const [sx, sy] of spots) {
        const cand = { x: sx - tw / 2 - 5, y: sy - 13, w: tw + 10, h: 18 };
        if (!collides(cand)) {
          lx = sx;
          ly = sy;
          rect = cand;
          break;
        }
      }
      if (!rect) {
        continue; // hot labels are pushed first, so they always win
      }
      placed.push(rect);
      ctx.globalAlpha = la;
      ctx.lineWidth = 4;
      ctx.strokeStyle = rgba(BG, 0.92); // dark halo behind the text
      ctx.strokeText(nd.name, lx, ly);
      ctx.fillStyle = isHot
        ? rgba(WHITE, 1)
        : rgba(isGhost ? LABEL_DIM : LABEL, 1);
      ctx.fillText(nd.name, lx, ly);
    }
  }

  /** Hover tooltip pinned by the soma: name + a kind-dot meta line, on an
   *  OPAQUE overlay backplate in the app's card language (tokens). */
  private drawTooltip(
    ctx: CanvasRenderingContext2D,
    w: number,
    h: number,
    hoverId: string,
  ): void {
    const i = this.renderNodes.findIndex((x) => x.id === hoverId);
    if (i < 0) {
      return;
    }
    const t = this.theme as Theme;
    const nd = this.renderNodes[i];
    const p = this.proj[i];
    const kind = nd.kind === "project" ? "Project" : "Person";
    const meta = `${kind} · ${nd.mentionCount} mention${nd.mentionCount === 1 ? "" : "s"} · ${nd.degree} connection${nd.degree === 1 ? "" : "s"}`;
    ctx.font = `600 12.5px ${t.font}`;
    const w1 = ctx.measureText(nd.name).width;
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
    ctx.textAlign = "left";
    ctx.font = `600 12.5px ${t.font}`;
    ctx.fillStyle = rgba(t.textPri, 1);
    ctx.fillText(nd.name, bx + 12, by + 18);
    // Kind-dot in the node's hue, then the meta line.
    ctx.beginPath();
    ctx.arc(bx + 15.5, by + 30.5, 3.5, 0, Math.PI * 2);
    ctx.fillStyle = rgba(nd.kind === "project" ? PROJECT : PERSON, 1);
    ctx.fill();
    ctx.font = `500 11px ${t.font}`;
    ctx.fillStyle = rgba(t.textSec, 1);
    ctx.fillText(meta, bx + 24, by + 34);
    ctx.textAlign = "center";
  }

  /** The view-space x stretch that maps the near-spherical cloud onto the
   *  (usually wide) canvas — clamped so portrait frames aren't squeezed. */
  private stretchX(w: number, h: number): number {
    return clamp(((w - 112) / Math.max(120, h - 116)) * 0.74, 1, 1.55);
  }

  /**
   * Frame the whole node cloud: solve, for the CURRENT rotation, the exact
   * camera distance at which every soma (+ halo margin) fits inside the padded
   * viewport — the layout fills the canvas instead of huddling at the centre.
   */
  private fitCamera(w: number, h: number): void {
    if (this.renderNodes.length === 0) {
      return;
    }
    const f = 1 - this.morph;
    const yaw = this.camYaw * f;
    const pitch = this.camPitch * f;
    const cosY = Math.cos(yaw);
    const sinY = Math.sin(yaw);
    const cosP = Math.cos(pitch);
    const sinP = Math.sin(pitch);
    const stretchX = this.stretchX(w, h);
    const W = Math.max(60, w / 2 - 44);
    const H = Math.max(60, h / 2 - 58); // extra bottom room for labels
    // Rotate once; solve the exact distance per node, then CENTRE the
    // projected bounding box with pan (the cloud is rarely origin-centred).
    const rot = this.renderNodes.map((nd) => {
      const zz = nd.z * f;
      const x1 = nd.x * cosY + zz * sinY;
      const z1 = -nd.x * sinY + zz * cosY;
      return {
        x1: x1 * stretchX,
        y2: nd.y * cosP - z1 * sinP,
        z2: nd.y * sinP + z1 * cosP,
        m: nd.r * 2.2, // halo margin
      };
    });
    let need = MIN_DIST;
    for (const r of rot) {
      need = Math.max(
        need,
        r.z2 + ((Math.abs(r.x1) + r.m) * FOCAL) / W,
        r.z2 + ((Math.abs(r.y2) + r.m) * FOCAL) / H,
      );
    }
    this.camDist = clamp(need, MIN_DIST, MAX_DIST);
    this.fitDist = this.camDist;
    let minX = Infinity;
    let maxX = -Infinity;
    let minY = Infinity;
    let maxY = -Infinity;
    let cxm = 0;
    let cym = 0;
    for (const r of rot) {
      const s = FOCAL / Math.max(60, this.camDist - r.z2);
      minX = Math.min(minX, (r.x1 - r.m) * s);
      maxX = Math.max(maxX, (r.x1 + r.m) * s);
      minY = Math.min(minY, (r.y2 - r.m) * s);
      maxY = Math.max(maxY, (r.y2 + r.m) * s);
      cxm += r.x1 * s;
      cym += r.y2 * s;
    }
    cxm /= rot.length;
    cym /= rot.length;
    // Blend the bbox centre 30% toward the projected MASS centroid: pure
    // bbox centring lets two orphan satellites park the whole connected
    // mass off in a corner of the frame (design-panel R4 composition).
    this.panX = -(0.7 * ((minX + maxX) / 2) + 0.3 * cxm);
    this.panY = -(0.7 * ((minY + maxY) / 2) + 0.3 * cym);
  }

  /** Nearest node under the pointer (uses the last frame's projection). */
  private hitTest(event: PointerEvent): string | null {
    const rect = this.hostRef.nativeElement.getBoundingClientRect();
    const px = event.clientX - rect.left;
    const py = event.clientY - rect.top;
    let best: string | null = null;
    let bestDepth = -Infinity;
    for (let i = 0; i < this.renderNodes.length; i++) {
      const p = this.proj[i];
      if (!p) {
        continue;
      }
      const rr = Math.max(p.sr, 10) + 4;
      const dx = px - p.sx;
      const dy = py - p.sy;
      if (dx * dx + dy * dy <= rr * rr && p.depth > bestDepth) {
        bestDepth = p.depth;
        best = this.renderNodes[i].id;
      }
    }
    return best;
  }

  // ── theme + paint caches ──────────────────────────────────────────────

  /** Read the app tokens the TOOLTIP chrome uses (the scene palette is fixed —
   *  see the module-level palette comment). */
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

  /** Static deep-space backdrop: fixed indigo field + nebula washes in the two
   *  node hues + faint defocused neural filaments + a corner vignette. */
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
    v.addColorStop(0.55, rgba(BG, 1));
    v.addColorStop(1, rgba(mix(BG, BG_LIFT, 0.35), 1));
    g.fillStyle = v;
    g.fillRect(0, 0, w, h);
    const n1 = g.createRadialGradient(
      w * 0.28,
      h * 0.3,
      0,
      w * 0.28,
      h * 0.3,
      Math.max(w, h) * 0.55,
    );
    n1.addColorStop(0, rgba(PERSON, 0.05));
    n1.addColorStop(1, rgba(PERSON, 0));
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
    n2.addColorStop(0, rgba(PROJECT, 0.045));
    n2.addColorStop(1, rgba(PROJECT, 0));
    g.fillStyle = n2;
    g.fillRect(0, 0, w, h);
    // Faint DEFOCUSED FILAMENTS — distant out-of-focus neural strands for
    // atmosphere (deterministic; static, baked into the cached backdrop).
    // Kept BARELY perceptible — at higher alpha these read as (fake) edges
    // and muddy the real synapse encoding (design-panel R4).
    g.lineCap = "round";
    for (let k = 0; k < 5; k++) {
      const fh = djb2(`filament-${k}`);
      const x0 = r01(fh, 1) * w;
      const y0 = r01(fh, 2) * h;
      const x1 = r01(fh, 3) * w;
      const y1 = r01(fh, 4) * h;
      const cx = (x0 + x1) / 2 + (r01(fh, 5) - 0.5) * w * 0.6;
      const cy = (y0 + y1) / 2 + (r01(fh, 6) - 0.5) * h * 0.6;
      const col = k % 2 === 0 ? PERSON : PROJECT;
      for (const [lw, la] of [
        [6, 0.008],
        [1.6, 0.016],
      ] as const) {
        g.strokeStyle = rgba(col, la);
        g.lineWidth = lw;
        g.beginPath();
        g.moveTo(x0, y0);
        g.quadraticCurveTo(cx, cy, x1, y1);
        g.stroke();
      }
    }
    const vg = g.createRadialGradient(
      w / 2,
      h / 2,
      Math.min(w, h) * 0.42,
      w / 2,
      h / 2,
      Math.max(w, h) * 0.78,
    );
    vg.addColorStop(0, "rgba(0,0,0,0)");
    vg.addColorStop(1, "rgba(0,0,0,0.5)");
    g.fillStyle = vg;
    g.fillRect(0, 0, w, h);
    return c;
  }

  /** Cached CAPPED halo sprite per kind — a white-hot centre bleeding through
   *  a saturated hue glow, fully transparent by {@link HALO_F}× the soma
   *  radius. Drawn with `lighter`; the crisp soma core is vector-drawn on top
   *  (never baked into the sprite). */
  private haloSprite(kind: GraphNode["kind"]): HTMLCanvasElement {
    const key = `halo:${kind}`;
    const cached = this.sprites.get(key);
    if (cached) {
      return cached;
    }
    const col = kind === "project" ? PROJECT : PERSON;
    const size = 128; // sprite space: soma radius = size/(2·HALO_F)
    const c = document.createElement("canvas");
    c.width = size;
    c.height = size;
    const g = c.getContext("2d");
    if (g) {
      const half = size / 2;
      const somaStop = 1 / HALO_F; // the soma edge inside the sprite
      // HUE-FORWARD stops — a near-white sprite core made overlapping halos
      // in dense clusters composite (additively) to one white clump and
      // erased the person/project colour encoding (design-panel R4).
      const grad = g.createRadialGradient(half, half, 0, half, half, half);
      grad.addColorStop(0, rgba(mix(col, WHITE, 0.62), 0.6));
      grad.addColorStop(somaStop * 0.55, rgba(mix(col, WHITE, 0.28), 0.5));
      grad.addColorStop(somaStop, rgba(col, 0.42));
      grad.addColorStop(somaStop + (1 - somaStop) * 0.38, rgba(col, 0.13));
      grad.addColorStop(1, rgba(col, 0));
      g.fillStyle = grad;
      g.fillRect(0, 0, size, size);
    }
    this.sprites.set(key, c);
    return c;
  }

  /** Cached hot-white pulse orb sprite (white core → indigo-violet falloff). */
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
      const glow = mix(PERSON, PROJECT, 0.5);
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
