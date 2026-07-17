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
 * origin-centred) — the directive only projects + paints it, never lays out.
 */
export interface FullSceneNode {
  id: string;
  kind: FullGraphNodeKind;
  label: string;
  /** Radius in world units (degree-scaled by the component). */
  r: number;
  x: number;
  y: number;
}

/** A laid-out edge whose endpoints both survived the lens filter. */
export interface FullSceneEdge {
  /** `${src}::${dst}::${kind}` — stable identity. */
  key: string;
  src: string;
  dst: string;
  kind: FullGraphEdgeKind;
  /** `true` = un-accepted semantic suggestion → drawn DASHED. */
  suggested: boolean;
}

type Rgb = [number, number, number];

/* ── THE SCENE PALETTE — a FIXED dark field, independent of the page theme ──
 * Same rationale as neural-scene.directive.ts: glow/tint only reads on a dark
 * surface, so the canvas is its own artwork field even under the light theme
 * (design review R3). The four node hues MIRROR the DOM tokens
 * `--graph-entity/-meeting/-note/-document` (whose values the chips + legend
 * dots consume) — kept in sync by intent, exactly like the neural-scene
 * PERSON/PROJECT constants mirror the legend dots. */
const BG: Rgb = [8, 9, 20];
const BG_LIFT: Rgb = [17, 19, 40];
const NODE_COLORS: Record<FullGraphNodeKind, Rgb> = {
  entity: [91, 189, 255], // #5bbdff azure
  meeting: [255, 157, 92], // #ff9d5c amber
  note: [76, 224, 160], // #4ce0a0 mint
  document: [200, 111, 242], // #c86ff2 orchid
};
const LABEL: Rgb = [216, 222, 242];
const WHITE: Rgb = [255, 255, 255];

const MIN_SCALE = 0.25;
const MAX_SCALE = 4;

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

/**
 * The FULL-BRAIN scene renderer — a canvas-drawn, pan/zoom multi-kind graph over
 * the typed nodes/edges the {@link FullBrainGraphComponent} lays out: per-kind
 * colored somas with labels, per-kind styled edges (co-occurrence/mention thin,
 * wikilink/companion solid accent, semantic — suggested drawn DASHED), hover +
 * click-through, on a fixed deep-indigo field.
 *
 * ZONELESS RULE (.claude/rules/angular-zoneless.md §5): a bare rAF/timer or a
 * DOM observer in a COMPONENT is banned — every DOM-loop concern (the
 * ResizeObserver, the reduced-motion + visibilitychange + color-scheme
 * listeners, the invalidate-on-demand paint) lives HERE in a directive and is
 * released in `DestroyRef.onDestroy()`. There is NO continuous animation loop:
 * the scene is a still that repaints on input / camera / hover change via
 * `invalidate()` (a single one-shot `requestAnimationFrame`), so it costs
 * nothing at rest and needs no reduced-motion branch beyond skipping repaints
 * while hidden.
 *
 * The component owns the deterministic Fruchterman-Reingold LAYOUT (a pure
 * `computed()`); this directive owns only projection, paint, and pointer input.
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

  /** Click: a node id, or null for empty space (the component owns navigation). */
  readonly nodePick = output<string | null>();
  /** Hover enter/leave over a node (the component drives an aria-live hint). */
  readonly nodeHover = output<string | null>();

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
      if (e.src === sel) {
        set.add(e.dst);
      } else if (e.dst === sel) {
        set.add(e.src);
      }
    }
    return set;
  });

  // camera (world → screen: screen = center + pan + world * scale)
  private scale = 1;
  private panX = 0;
  private panY = 0;
  private fitted = false;

  // pointer
  private pointerDown = false;
  private dragMoved = false;
  private downX = 0;
  private downY = 0;
  private lastPX = 0;
  private lastPY = 0;

  // projected screen positions (index-aligned with sceneNodes())
  private proj: { sx: number; sy: number; sr: number }[] = [];

  private cssW = 0;
  private cssH = 0;
  private oneShotId: number | null = null;
  private ctx: CanvasRenderingContext2D | null;

  private readonly ro = new ResizeObserver((entries) => {
    const rect = entries[entries.length - 1]?.contentRect;
    if (!rect || rect.width < 2 || rect.height < 2) {
      return;
    }
    const wasZero = this.cssW < 2 || this.cssH < 2;
    this.cssW = rect.width;
    this.cssH = rect.height;
    if (wasZero) {
      this.fitted = false; // (re)fit once we have a real size
    }
    this.invalidate();
  });
  private readonly onVisibility = (): void => {
    if (!document.hidden) {
      this.invalidate();
    }
  };
  private readonly scheme = window.matchMedia("(prefers-color-scheme: light)");
  private readonly onScheme = (): void => this.invalidate();

  constructor() {
    this.ctx = this.hostRef.nativeElement.getContext("2d");
    this.ro.observe(this.hostRef.nativeElement);
    document.addEventListener("visibilitychange", this.onVisibility);
    this.scheme.addEventListener("change", this.onScheme);

    // Refit + repaint when the laid-out graph changes; a signal write inside a
    // tracked effect is allowed since Angular 19. The hover read is untracked so
    // hovering doesn't refit.
    effect(() => {
      const nodes = this.sceneNodes();
      this.sceneEdges();
      this.fitted = false;
      const hover = untracked(() => this._hoverId());
      if (hover !== null && !nodes.some((n) => n.id === hover)) {
        this._hoverId.set(null);
        this.nodeHover.emit(null);
      }
      this.invalidate();
    });

    // Repaint on selection/hover change (neighbourhood dimming).
    effect(() => {
      this.selectedId();
      this._hoverId();
      this.invalidate();
    });

    this.destroyRef.onDestroy(() => {
      if (this.oneShotId !== null) {
        cancelAnimationFrame(this.oneShotId);
        this.oneShotId = null;
      }
      this.ro.disconnect();
      document.removeEventListener("visibilitychange", this.onVisibility);
      this.scheme.removeEventListener("change", this.onScheme);
    });
  }

  // ── public camera API (the component's toolbar drives these) ──────────

  zoomBy(factor: number): void {
    const old = this.scale;
    this.scale = clamp(old * factor, MIN_SCALE, MAX_SCALE);
    const k = this.scale / old;
    this.panX *= k;
    this.panY *= k;
    this.invalidate();
  }

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
    this.invalidate();
  }

  // ── paint (invalidate-on-demand, no continuous loop) ──────────────────

  /** One-shot repaint; coalesces multiple invalidations into one frame. */
  private invalidate(): void {
    if (this.oneShotId !== null || document.hidden) {
      return;
    }
    this.oneShotId = requestAnimationFrame(() => {
      this.oneShotId = null;
      this.render();
    });
  }

  /** Fit the laid-out cloud into the padded viewport (once per graph/resize). */
  private fit(): void {
    const nodes = this.sceneNodes();
    if (nodes.length === 0 || this.cssW < 4 || this.cssH < 4) {
      this.scale = 1;
      this.panX = 0;
      this.panY = 0;
      return;
    }
    let maxX = 1;
    let maxY = 1;
    for (const n of nodes) {
      maxX = Math.max(maxX, Math.abs(n.x) + n.r);
      maxY = Math.max(maxY, Math.abs(n.y) + n.r);
    }
    const padW = this.cssW / 2 - 40;
    const padH = this.cssH / 2 - 40;
    this.scale = clamp(
      Math.min(padW / maxX, padH / maxY),
      MIN_SCALE,
      MAX_SCALE,
    );
    this.panX = 0;
    this.panY = 0;
  }

  private render(): void {
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
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

    if (!this.fitted) {
      this.fit();
      this.fitted = true;
    }

    // ── backdrop (deep-indigo field + gentle vignette) ──
    const bg = ctx.createLinearGradient(0, 0, 0, h);
    bg.addColorStop(0, rgba(BG_LIFT, 1));
    bg.addColorStop(0.6, rgba(BG, 1));
    bg.addColorStop(1, rgba(mix(BG, BG_LIFT, 0.35), 1));
    ctx.fillStyle = bg;
    ctx.fillRect(0, 0, w, h);
    const vg = ctx.createRadialGradient(
      w / 2,
      h / 2,
      Math.min(w, h) * 0.4,
      w / 2,
      h / 2,
      Math.max(w, h) * 0.78,
    );
    vg.addColorStop(0, "rgba(0,0,0,0)");
    vg.addColorStop(1, "rgba(0,0,0,0.45)");
    ctx.fillStyle = vg;
    ctx.fillRect(0, 0, w, h);

    const nodes = this.sceneNodes();
    const n = nodes.length;
    if (n === 0) {
      return;
    }

    const cx = w / 2 + this.panX;
    const cy = h / 2 + this.panY;
    const s = this.scale;
    const idIndex = new Map(nodes.map((nd, i) => [nd.id, i]));
    this.proj = nodes.map((nd) => ({
      sx: cx + nd.x * s,
      sy: cy + nd.y * s,
      sr: Math.max(2.5, nd.r * s),
    }));

    const hover = this._hoverId();
    const selId = this.selectedId();
    const focus = this.neighborSet();

    // ── edges (per-kind style; suggested = dashed) ──
    ctx.lineCap = "round";
    for (const e of this.sceneEdges()) {
      const a = idIndex.get(e.src);
      const b = idIndex.get(e.dst);
      if (a === undefined || b === undefined) {
        continue;
      }
      const A = this.proj[a];
      const B = this.proj[b];
      const inFocus =
        !focus || (focus.has(e.src) && focus.has(e.dst));
      const touchesSel =
        selId !== null && (e.src === selId || e.dst === selId);
      const hoverHit =
        hover !== null && (e.src === hover || e.dst === hover);

      const style = this.edgeStyle(e.kind);
      let alpha = style.alpha;
      let width = style.width;
      if (!focus) {
        // no selection — everything at rest strength
      } else if (inFocus) {
        alpha = Math.min(0.95, alpha * 1.8);
        width += 0.4;
      } else {
        alpha *= 0.22; // out-of-neighbourhood ghost
      }
      if (touchesSel) {
        alpha = Math.min(1, alpha * 1.4);
        width += 0.6;
      }
      if (hoverHit) {
        alpha = Math.min(1, alpha * 1.5);
      }

      ctx.globalAlpha = alpha;
      ctx.strokeStyle = rgba(style.color, 1);
      ctx.lineWidth = width;
      ctx.setLineDash(e.suggested ? [5, 5] : []);
      ctx.beginPath();
      ctx.moveTo(A.sx, A.sy);
      ctx.lineTo(B.sx, B.sy);
      ctx.stroke();
    }
    ctx.setLineDash([]);

    // ── nodes (per-kind color; label when big/hovered/focused) ──
    const t = getComputedStyle(canvas);
    const font =
      t.getPropertyValue("--font-sans").trim() || "system-ui, sans-serif";
    ctx.font = `600 12px ${font}`;
    ctx.textAlign = "center";
    ctx.lineJoin = "round";
    for (let i = 0; i < n; i++) {
      const nd = nodes[i];
      const p = this.proj[i];
      const inSet = !focus || focus.has(nd.id);
      const a = inSet ? 1 : 0.28;
      const isSel = selId === nd.id;
      const isHover = hover === nd.id;
      const col = NODE_COLORS[nd.kind];

      // soft halo
      ctx.globalAlpha = a * 0.5;
      const halo = ctx.createRadialGradient(
        p.sx,
        p.sy,
        0,
        p.sx,
        p.sy,
        p.sr * 2.4,
      );
      halo.addColorStop(0, rgba(col, 0.4));
      halo.addColorStop(1, rgba(col, 0));
      ctx.fillStyle = halo;
      ctx.beginPath();
      ctx.arc(p.sx, p.sy, p.sr * 2.4, 0, Math.PI * 2);
      ctx.fill();

      // core
      ctx.globalAlpha = a;
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

      // label — always for the focused set / hovered / selected, else only
      // reasonably large somas (keeps dense graphs legible).
      const showLabel =
        isSel || isHover || (focus ? inSet : p.sr >= 7);
      if (showLabel && p.sr >= 3) {
        const label =
          nd.label.length > 26 ? nd.label.slice(0, 25) + "…" : nd.label;
        const ly = p.sy + p.sr + 13;
        ctx.globalAlpha = isSel || isHover ? 1 : a * 0.85;
        ctx.lineWidth = 3;
        ctx.strokeStyle = rgba(BG, 0.9);
        ctx.strokeText(label, p.sx, ly);
        ctx.fillStyle = rgba(isSel || isHover ? WHITE : LABEL, 1);
        ctx.fillText(label, p.sx, ly);
      }
    }
    ctx.globalAlpha = 1;
  }

  /** Per-edge-kind stroke color + base alpha/width (design encoding). */
  private edgeStyle(kind: FullGraphEdgeKind): {
    color: Rgb;
    alpha: number;
    width: number;
  } {
    switch (kind) {
      case "co_occurrence":
        return { color: [120, 130, 170], alpha: 0.28, width: 1 };
      case "mention":
        return { color: [150, 160, 210], alpha: 0.34, width: 1.1 };
      case "wikilink":
        return { color: [110, 118, 255], alpha: 0.55, width: 1.6 };
      case "companion":
        return { color: [76, 224, 160], alpha: 0.55, width: 1.6 };
      case "semantic":
        return { color: [200, 111, 242], alpha: 0.5, width: 1.4 };
    }
  }

  /** Nearest node under the pointer (uses the last frame's projection). */
  private hitTest(event: PointerEvent): string | null {
    const rect = this.hostRef.nativeElement.getBoundingClientRect();
    const px = event.clientX - rect.left;
    const py = event.clientY - rect.top;
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
}
