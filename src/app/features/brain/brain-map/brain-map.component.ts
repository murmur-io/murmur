import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
  signal,
  viewChild,
} from "@angular/core";
import type { GraphData } from "../../../core/models";
import {
  NeuralSceneDirective,
  type SceneEdge,
  type SceneNode,
} from "../neural-scene.directive";

/** Cap on rendered nodes — the strongest top-K by mention count. Big graphs
 *  cluster down to this so the layout + draw calls stay bounded. */
const MAX_NODES = 60;
/** Fixed force-directed iteration count — run ONCE, synchronously, no loop. */
const ITERATIONS = 240;
/** Logical world scale (world units). The camera fits to the laid-out cloud. */
const WORLD = 1000;
/** The laid-out cloud is kept inside this radius so zoom clamps stay sane. */
const MAX_CLOUD_R = 470;

/**
 * The NEURAL BRAIN MAP — `get_graph()` rendered as a living neural scene:
 * glowing neuron somas with dendrites, curved firing synapses, a 3-D orbit
 * camera (default) with a flat 2-D mode. No graph library, no new dependency.
 *
 * SPLIT OF RESPONSIBILITY (zoneless):
 * - THIS COMPONENT owns the pure data derivations — the top-{@link MAX_NODES}
 *   cap, the DETERMINISTIC one-shot 3-D layout (a `computed()`, no simulation
 *   loop, no `Math.random`), selection state + keyboard a11y, and the DOM
 *   toolbar chrome.
 * - {@link NeuralSceneDirective} owns every DOM-loop concern (rAF loop,
 *   ResizeObserver, reduced-motion + visibility listeners) per
 *   `.claude/rules/angular-zoneless.md` §5 — such loops are banned in a
 *   component, so the renderer is a directive on the `<canvas>`.
 *
 * LAYOUT: nodes are seeded on a Fibonacci sphere keyed by their sort index
 * (most-mentioned first, name tiebreak — identical ordering to the old SVG
 * map), then a fixed {@link ITERATIONS}-iteration 3-D Fruchterman-Reingold
 * pass runs synchronously (pairwise repulsion + log-weighted edge springs +
 * a gentle centre pull), then the cloud is recentred and NORMALISED onto a
 * brain-ish world ellipsoid (x widest, y×0.72, z×0.85 — so the layout always
 * fills the frame), followed by an overlap-relaxation pass so somas never
 * fuse. Same data → identical layout, every render.
 */
@Component({
  selector: "app-brain-map",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [NeuralSceneDirective],
  templateUrl: "./brain-map.component.html",
  styleUrl: "./brain-map.component.scss",
})
export class BrainMapComponent {
  /** The graph to visualise. Re-laying out when it changes (pure derivation). */
  readonly data = input<GraphData | null>(null);

  /** The focused node id, or null. Same toggle semantics as the old SVG map:
   *  click the selected node again = deselect; empty click clears. */
  readonly selectedId = signal<string | null>(null);
  /** Hovered node id (mirrored from the scene; tooltip is canvas-drawn). */
  readonly hoverId = signal<string | null>(null);
  /** Camera mode — 3-D orbit is the DEFAULT; 2-D is the flat fallback. */
  readonly mode = signal<"3d" | "2d">("3d");
  /** aria-live announcement for selection changes (event-driven, not derived —
   *  "Selection cleared" must announce once, not on every recompute). */
  readonly announcement = signal("");

  private readonly scene = viewChild(NeuralSceneDirective);

  /**
   * The capped, laid-out nodes — a PURE function of {@link data}. Top-K by
   * mention count (name tiebreak), Fibonacci-sphere seed, fixed-iteration 3-D
   * force pass, ellipsoid normalisation, overlap relaxation. Deterministic:
   * no `Math.random`, coincident points get hash-style index nudges.
   */
  protected readonly sceneNodes = computed<SceneNode[]>(() => {
    const d = this.data();
    if (!d || d.nodes.length === 0) {
      return [];
    }

    // Deterministic order: most-mentioned first, name as a stable tiebreak
    // (identical to the old SVG map — the parent's cap copy relies on it).
    const ordered = [...d.nodes].sort(
      (a, b) => b.mentionCount - a.mentionCount || a.name.localeCompare(b.name),
    );
    const nodes = ordered.slice(0, MAX_NODES);
    const n = nodes.length;
    const idIndex = new Map(nodes.map((x, i) => [x.id, i]));

    // Mention-count → soma radius (8…26 world units), sqrt-scaled.
    const maxM = Math.max(1, ...nodes.map((x) => x.mentionCount));
    const radii = nodes.map(
      (x) => 8 + (Math.sqrt(x.mentionCount) / Math.sqrt(maxM)) * 18,
    );

    // SEED: Fibonacci sphere keyed by sort index → identical every render.
    const golden = Math.PI * (3 - Math.sqrt(5));
    const seedR = WORLD * 0.3;
    const xs = new Float64Array(n);
    const ys = new Float64Array(n);
    const zs = new Float64Array(n);
    for (let i = 0; i < n; i++) {
      const v = n === 1 ? 0 : 1 - (2 * (i + 0.5)) / n;
      const ring = Math.sqrt(Math.max(0, 1 - v * v));
      const ang = i * golden;
      xs[i] = seedR * ring * Math.cos(ang);
      ys[i] = seedR * v;
      zs[i] = seedR * ring * Math.sin(ang);
    }

    // Only edges whose endpoints both survived the cap participate.
    const edges = d.edges.filter(
      (e) => idIndex.has(e.source) && idIndex.has(e.target),
    );

    // Degree (connection count): attenuates hub springs below and rides along
    // for tooltips + announcements.
    const degree = new Map<string, number>();
    for (const e of edges) {
      degree.set(e.source, (degree.get(e.source) ?? 0) + 1);
      degree.set(e.target, (degree.get(e.target) ?? 0) + 1);
    }

    // 3-D Fruchterman-Reingold. k (ideal spacing) is tuned UP vs the old flat
    // map so the cloud SPREADS instead of clumping into one glowing blob.
    const k = Math.cbrt((WORLD * WORLD * WORLD) / Math.max(1, n)) * 1.15;
    const k2 = k * k;
    let temp = WORLD * 0.16;
    const cool = temp / (ITERATIONS + 1);
    const dx3 = new Float64Array(n);
    const dy3 = new Float64Array(n);
    const dz3 = new Float64Array(n);

    for (let iter = 0; iter < ITERATIONS; iter++) {
      dx3.fill(0);
      dy3.fill(0);
      dz3.fill(0);

      // Pairwise repulsion (n ≤ MAX_NODES → O(n²) is bounded).
      for (let i = 0; i < n; i++) {
        for (let j = i + 1; j < n; j++) {
          let dx = xs[i] - xs[j];
          let dy = ys[i] - ys[j];
          let dz = zs[i] - zs[j];
          let d2 = dx * dx + dy * dy + dz * dz;
          if (d2 < 0.01) {
            // Deterministic nudge for coincident points (no Math.random).
            dx = ((i * 31 + j) % 7) - 3 + 0.5;
            dy = ((i * 17 + j) % 5) - 2 + 0.5;
            dz = ((i * 13 + j) % 3) - 1 + 0.5;
            d2 = dx * dx + dy * dy + dz * dz;
          }
          const dist = Math.sqrt(d2);
          const force = k2 / dist;
          const fx = (dx / dist) * force;
          const fy = (dy / dist) * force;
          const fz = (dz / dist) * force;
          dx3[i] += fx;
          dy3[i] += fy;
          dz3[i] += fz;
          dx3[j] -= fx;
          dy3[j] -= fy;
          dz3[j] -= fz;
        }
      }

      // Edge springs, log-weighted by co-occurrence and ATTENUATED at hubs
      // (÷√min-degree): un-attenuated springs let high-degree nodes crush
      // their whole neighbourhood into one caterpillar clump (R3).
      for (const e of edges) {
        const i = idIndex.get(e.source) as number;
        const j = idIndex.get(e.target) as number;
        const dx = xs[i] - xs[j];
        const dy = ys[i] - ys[j];
        const dz = zs[i] - zs[j];
        const dist = Math.sqrt(dx * dx + dy * dy + dz * dz) || 0.01;
        const w = 1 + Math.log2(1 + e.weight);
        const hub = Math.sqrt(
          Math.min(degree.get(e.source) ?? 1, degree.get(e.target) ?? 1),
        );
        const force = ((dist * dist) / k) * (w / hub) * 0.85;
        const fx = (dx / dist) * force;
        const fy = (dy / dist) * force;
        const fz = (dz / dist) * force;
        dx3[i] -= fx;
        dy3[i] -= fy;
        dz3[i] -= fz;
        dx3[j] += fx;
        dy3[j] += fy;
        dz3[j] += fz;
      }

      // Apply, capped by the cooling temperature; gentle centre pull keeps
      // disconnected components from drifting away (kept SMALL — a strong
      // pull is what collapsed the old layout into a central blob).
      for (let i = 0; i < n; i++) {
        const dl =
          Math.sqrt(dx3[i] * dx3[i] + dy3[i] * dy3[i] + dz3[i] * dz3[i]) || 1;
        const step = Math.min(dl, temp);
        xs[i] += (dx3[i] / dl) * step;
        ys[i] += (dy3[i] / dl) * step;
        zs[i] += (dz3[i] / dl) * step;
        xs[i] -= xs[i] * 0.004;
        ys[i] -= ys[i] * 0.004;
        zs[i] -= zs[i] * 0.004;
      }
      temp = Math.max(0, temp - cool);
    }

    // FILL THE WORLD (R3 "the graph huddles in 20% of the canvas"): the FR
    // equilibrium collapses dense components into a tight blob the camera then
    // over-frames — and no affine per-axis rescale can spread a blob relative
    // to its satellites. So remap RADIALLY: recentre on the centroid, keep
    // each node's DIRECTION (the FR angular structure — clusters stay in
    // their sector, hubs stay centremost) but reassign its DISTANCE by rank
    // onto a uniform-density ball profile (r ∝ ∛rank), then squash into the
    // brain-ish ellipsoid (y ×0.72, z ×0.85). The cloud now fills the world
    // evenly for ANY graph shape. Fully deterministic.
    let mx = 0;
    let my = 0;
    let mz = 0;
    for (let i = 0; i < n; i++) {
      mx += xs[i];
      my += ys[i];
      mz += zs[i];
    }
    mx /= n;
    my /= n;
    mz /= n;
    for (let i = 0; i < n; i++) {
      xs[i] -= mx;
      ys[i] -= my;
      zs[i] -= mz;
    }
    // PCA-ALIGN (R4 "the cloud reads as a centre column"): whatever shape the
    // FR pass settles into, rotate it so its WIDEST principal axis lands on
    // world-x (screen-horizontal) and the second-widest on world-z (the orbit
    // plane) — the camera always faces the cloud's broadest silhouette.
    // Deterministic power iteration on the 3×3 covariance; no Math.random.
    if (n > 2) {
      const c = [
        [0, 0, 0],
        [0, 0, 0],
        [0, 0, 0],
      ];
      for (let i = 0; i < n; i++) {
        c[0][0] += xs[i] * xs[i];
        c[0][1] += xs[i] * ys[i];
        c[0][2] += xs[i] * zs[i];
        c[1][1] += ys[i] * ys[i];
        c[1][2] += ys[i] * zs[i];
        c[2][2] += zs[i] * zs[i];
      }
      c[1][0] = c[0][1];
      c[2][0] = c[0][2];
      c[2][1] = c[1][2];
      const mul = (v: number[]): number[] => [
        c[0][0] * v[0] + c[0][1] * v[1] + c[0][2] * v[2],
        c[1][0] * v[0] + c[1][1] * v[1] + c[1][2] * v[2],
        c[2][0] * v[0] + c[2][1] * v[1] + c[2][2] * v[2],
      ];
      const norm = (v: number[]): number =>
        Math.hypot(v[0], v[1], v[2]) || 1;
      const power = (seed: number[], ortho: number[] | null): number[] => {
        let v = [...seed];
        for (let it = 0; it < 40; it++) {
          if (ortho) {
            const d = v[0] * ortho[0] + v[1] * ortho[1] + v[2] * ortho[2];
            v = [v[0] - d * ortho[0], v[1] - d * ortho[1], v[2] - d * ortho[2]];
          }
          const m = mul(v);
          const l = norm(m);
          if (l < 1e-9) {
            break;
          }
          v = [m[0] / l, m[1] / l, m[2] / l];
        }
        const l = norm(v);
        return [v[0] / l, v[1] / l, v[2] / l];
      };
      const v1 = power([1, 0.6, 0.3], null);
      let v2 = power([0.3, 1, 0.6], v1);
      const d12 = v2[0] * v1[0] + v2[1] * v1[1] + v2[2] * v1[2];
      v2 = [v2[0] - d12 * v1[0], v2[1] - d12 * v1[1], v2[2] - d12 * v1[2]];
      const l2 = norm(v2);
      v2 = [v2[0] / l2, v2[1] / l2, v2[2] / l2];
      const v3 = [
        v1[1] * v2[2] - v1[2] * v2[1],
        v1[2] * v2[0] - v1[0] * v2[2],
        v1[0] * v2[1] - v1[1] * v2[0],
      ];
      for (let i = 0; i < n; i++) {
        const px = xs[i];
        const py = ys[i];
        const pz = zs[i];
        xs[i] = px * v1[0] + py * v1[1] + pz * v1[2]; // widest → screen-x
        ys[i] = px * v3[0] + py * v3[1] + pz * v3[2]; // thinnest → vertical
        zs[i] = px * v2[0] + py * v2[1] + pz * v2[2]; // 2nd → orbit depth
      }
    }

    if (n > 1) {
      const dists = Array.from({ length: n }, (_, i) =>
        Math.hypot(xs[i], ys[i], zs[i]),
      );
      const order = Array.from({ length: n }, (_, i) => i).sort(
        (a, b) => dists[a] - dists[b] || a - b,
      );
      for (let rank = 0; rank < n; rank++) {
        const i = order[rank];
        // Orphans/leaves (degree ≤ 1) are pulled 22% inward — un-compressed
        // they stake out the frame corners and the connected mass huddles in
        // the leftover space (design-panel R4 composition finding).
        const deg = degree.get(nodes[i].id) ?? 0;
        const target =
          MAX_CLOUD_R * Math.cbrt((rank + 0.5) / n) * (deg <= 1 ? 0.78 : 1);
        if (dists[i] < 1) {
          // Centroid-coincident node: deterministic Fibonacci direction.
          const v = 1 - (2 * (i + 0.5)) / n;
          const ring = Math.sqrt(Math.max(0, 1 - v * v));
          const ang = i * golden;
          xs[i] = target * ring * Math.cos(ang);
          ys[i] = target * v;
          zs[i] = target * ring * Math.sin(ang);
        } else {
          const s = target / dists[i];
          xs[i] *= s;
          ys[i] *= s;
          zs[i] *= s;
        }
      }
      // PER-AXIS FILL (R4 "the cloud huddles in a centre column"): the rank
      // remap preserves the FR *directions*, and those often cluster in one
      // angular sector — leaving the cloud thin along an axis no radial map
      // can widen. Normalise each axis's max extent onto the brain ellipsoid
      // (x widest, y ×0.72, z ×0.85) so the bounding box always FILLS the
      // frame; affine per axis, so clusters keep their relative structure.
      let ex = 1;
      let ey = 1;
      let ez = 1;
      for (let i = 0; i < n; i++) {
        ex = Math.max(ex, Math.abs(xs[i]));
        ey = Math.max(ey, Math.abs(ys[i]));
        ez = Math.max(ez, Math.abs(zs[i]));
      }
      const fx = MAX_CLOUD_R / ex;
      const fy = (MAX_CLOUD_R * 0.72) / ey;
      const fz = (MAX_CLOUD_R * 0.85) / ez;
      for (let i = 0; i < n; i++) {
        xs[i] *= fx;
        ys[i] *= fy;
        zs[i] *= fz;
      }
    }

    // OVERLAP RELAXATION: push any two somas apart until they keep a
    // size-scaled clearance — no two neurons can fuse into one smear.
    for (let pass = 0; pass < 40; pass++) {
      let moved = false;
      for (let i = 0; i < n; i++) {
        for (let j = i + 1; j < n; j++) {
          let dx = xs[i] - xs[j];
          let dy = ys[i] - ys[j];
          let dz = zs[i] - zs[j];
          let dist = Math.sqrt(dx * dx + dy * dy + dz * dz);
          // Clearance scales with the SOMA SIZES: two big neurons carry big
          // halos (2.6× r each), so a fixed gap let the hottest pair fuse
          // into one blown-out clump (design-panel R4 washout finding).
          const need = (radii[i] + radii[j]) * 2.1 + 26;
          if (dist >= need) {
            continue;
          }
          if (dist < 0.01) {
            dx = ((i * 31 + j) % 7) - 3 + 0.5;
            dy = ((i * 17 + j) % 5) - 2 + 0.5;
            dz = ((i * 13 + j) % 3) - 1 + 0.5;
            dist = Math.sqrt(dx * dx + dy * dy + dz * dz);
          }
          const push = (need - dist) / 2 / dist;
          xs[i] += dx * push;
          ys[i] += dy * push;
          zs[i] += dz * push;
          xs[j] -= dx * push;
          ys[j] -= dy * push;
          zs[j] -= dz * push;
          moved = true;
        }
      }
      if (!moved) {
        break;
      }
    }

    // Keep the cloud inside MAX_CLOUD_R so the zoom clamps stay meaningful.
    let bound = 1;
    for (let i = 0; i < n; i++) {
      bound = Math.max(
        bound,
        Math.sqrt(xs[i] * xs[i] + ys[i] * ys[i] + zs[i] * zs[i]) + radii[i],
      );
    }
    const scale = bound > MAX_CLOUD_R ? MAX_CLOUD_R / bound : 1;

    return nodes.map((node, i) => ({
      id: node.id,
      name: node.name,
      kind: node.kind,
      mentionCount: node.mentionCount,
      degree: degree.get(node.id) ?? 0,
      x: xs[i] * scale,
      y: ys[i] * scale,
      z: zs[i] * scale,
      r: radii[i],
    }));
  });

  /** Edges between surviving nodes, with a stable key + raw weight. */
  protected readonly sceneEdges = computed<SceneEdge[]>(() => {
    const d = this.data();
    if (!d) {
      return [];
    }
    const ids = new Set(this.sceneNodes().map((p) => p.id));
    const out: SceneEdge[] = [];
    for (const e of d.edges) {
      if (ids.has(e.source) && ids.has(e.target)) {
        out.push({
          key: `${e.source}::${e.target}`,
          source: e.source,
          target: e.target,
          weight: e.weight,
        });
      }
    }
    return out;
  });

  protected readonly ariaLabel = computed(() => {
    const n = this.sceneNodes().length;
    if (n === 0) {
      return "Neural brain map — empty.";
    }
    const e = this.sceneEdges().length;
    return (
      `Neural brain map of ${n} ${n === 1 ? "entity" : "entities"} and ` +
      `${e} ${e === 1 ? "connection" : "connections"}. Arrow keys cycle ` +
      `entities, Enter toggles selection, Escape clears.`
    );
  });

  protected readonly hint = computed(() =>
    this.mode() === "3d"
      ? "Drag to orbit · scroll to zoom · click a neuron to focus its connections."
      : "Drag to pan · scroll to zoom · click a neuron to focus its connections.",
  );

  // ── interaction ────────────────────────────────────────────────────────

  /** Scene click: a node id toggles, empty space (null) clears. */
  protected onPick(id: string | null): void {
    if (id === null) {
      this.clearSelection();
    } else if (this.selectedId() === id) {
      this.clearSelection();
    } else {
      this.select(id);
    }
  }

  /** Keyboard a11y: arrows cycle in mention-rank order, Enter/Space toggles,
   *  Escape clears. Rank order = sceneNodes order (already sorted). */
  protected onCanvasKeydown(event: KeyboardEvent): void {
    const nodes = this.sceneNodes();
    if (nodes.length === 0) {
      return;
    }
    if (event.key === "ArrowRight" || event.key === "ArrowLeft") {
      event.preventDefault();
      const cur = this.selectedId();
      const idx = cur ? nodes.findIndex((x) => x.id === cur) : -1;
      const next =
        event.key === "ArrowRight"
          ? (idx + 1) % nodes.length
          : idx <= 0
            ? nodes.length - 1
            : idx - 1;
      this.select(nodes[next].id);
    } else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      if (this.selectedId()) {
        this.clearSelection();
      } else {
        this.select(nodes[0].id);
      }
    } else if (event.key === "Escape" && this.selectedId()) {
      this.clearSelection();
    }
  }

  private select(id: string): void {
    this.selectedId.set(id);
    const node = this.sceneNodes().find((x) => x.id === id);
    if (node) {
      const kind = node.kind === "project" ? "Project" : "Person";
      const m =
        node.mentionCount === 1 ? "1 mention" : `${node.mentionCount} mentions`;
      const c =
        node.degree === 1 ? "1 connection" : `${node.degree} connections`;
      this.announcement.set(`${node.name} — ${kind}, ${m}, ${c}.`);
    }
  }

  private clearSelection(): void {
    if (this.selectedId() !== null) {
      this.selectedId.set(null);
      this.announcement.set("Selection cleared.");
    }
  }

  protected setMode(mode: "3d" | "2d"): void {
    this.mode.set(mode);
  }

  protected zoomIn(): void {
    this.scene()?.zoomBy(0.8);
  }

  protected zoomOut(): void {
    this.scene()?.zoomBy(1.25);
  }

  /** Reset = re-fit the camera + clear the `touched` flag so auto-fit (and the
   *  cinematic auto-rotate) own the view again — the old map's semantics. */
  protected resetView(): void {
    this.scene()?.resetView();
  }
}
