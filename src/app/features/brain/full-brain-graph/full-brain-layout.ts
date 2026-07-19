import type { FullGraphNodeKind } from "../../../core/models";
import type { FullSceneNode } from "./full-brain-scene.directive";

/**
 * THE FULL-BRAIN LAYOUT ENGINE — a pure, deterministic function that turns the
 * lens-filtered (already draw-capped) typed nodes + edges into origin-centred
 * world positions for the scene to project.
 *
 * WHY THIS EXISTS (the 2026-07-19 redesign): the previous layout ran ONE global
 * Fruchterman-Reingold over every node. `build_full_graph` emits every visible
 * entity/meeting/note/document as a node — many with `degree: 0` (an orphan doc
 * with no wikilink, a fresh note, a meeting whose entities co-occur nowhere) —
 * i.e. genuine 1-node connected components. A single global sim REPELS those
 * singletons to the corners (long-reach `k²/d` repulsion, near-zero centre pull)
 * and the camera then fits the blown-out bounding box → everything shrinks below
 * the label threshold → the "scattered unlabeled dots" bug.
 *
 * THE FIX (per-component layout + packing — the technique fcose/Graphviz/Gephi
 * use): detect connected components (union-find), lay out EACH ONE independently
 * with a local FR at a FIXED ideal edge length (so a 2-node component stays small
 * instead of spanning the world), TILE all degree-0 singletons into ONE compact
 * grid block, then SHELF-PACK every component's bounding box into a target aspect
 * ratio and centre the whole cloud on the origin. No inter-component repulsion,
 * no wasted space, no corner-scatter — and it stays fully DETERMINISTIC (no
 * `Math.random`; a golden-angle seed keyed by index, union-find in index order),
 * which the caller's `computed()` requires (it re-runs on every lens toggle).
 *
 * Everything here is O(n²) at worst over ≤ a few hundred nodes and runs ONCE per
 * graph/lens change off the render thread — no Barnes-Hut quadtree needed (its
 * build overhead doesn't pay off until ~thousands of nodes).
 */

/** Ideal edge length (world units) — the natural spacing between two linked
 *  nodes. FIXED per component (not `√(WORLD²/n)`) so small components stay
 *  physically small and pack tightly instead of ballooning. */
const IDEAL_EDGE = 92;
/** Min / max soma radius (world units), sqrt(degree)-scaled between them. */
const MIN_R = 6;
const MAX_R = 18;
/** Clear gap (world units) enforced between any two somas after layout. */
const NODE_GAP = 14;
/** Gap (world units) padded around every component before packing. */
const COMPONENT_SPACING = 46;
/** Extra gap between tiled singleton cells. */
const TILE_GAP = 14;
/** Pack toward this width:height ratio; the camera then fits the real viewport. */
const TARGET_ASPECT = 1.6;
/** Half-width (in grid cells) of the packing spiral lattice. */
const PACK_R = 84;
/**
 * A deterministic square-spiral of integer grid offsets, sorted center-out with
 * cost `max(|gx|, |gy|·aspect)` so packing grows WIDER than tall (fills the
 * usually-wide canvas). Precomputed ONCE at module load (data-independent) so
 * `packBoxes` never re-sorts. This is the Freivalds–Doğrusöz–Kikusts placement
 * order — each component lands at the free cell nearest the centre.
 */
const SORTED_LATTICE: readonly (readonly [number, number])[] = (() => {
  const pts: [number, number][] = [];
  for (let gx = -PACK_R; gx <= PACK_R; gx++) {
    for (let gy = -PACK_R; gy <= PACK_R; gy++) {
      pts.push([gx, gy]);
    }
  }
  const cost = (p: [number, number]): number =>
    Math.max(Math.abs(p[0]), Math.abs(p[1]) * TARGET_ASPECT);
  pts.sort((a, b) => {
    const ca = cost(a);
    const cb = cost(b);
    if (ca !== cb) return ca - cb;
    const ma = Math.abs(a[0]) + Math.abs(a[1]);
    const mb = Math.abs(b[0]) + Math.abs(b[1]);
    if (ma !== mb) return ma - mb;
    // Deterministic angular tiebreak so the same data always packs identically.
    return (
      Math.atan2(a[1], a[0]) - Math.atan2(b[1], b[0]) ||
      a[0] - b[0] ||
      a[1] - b[1]
    );
  });
  return pts;
})();
/** FR iterations for a small component; scaled DOWN for large ones (they cost
 *  O(n²) per iter and converge sooner). */
const MAX_ITERS = 260;

/** One node the layout consumes — the display fields carried through untouched
 *  plus the `degree` that drives radius, seeding order and (downstream) labels. */
export type FbLayoutNode = Omit<FullSceneNode, "r" | "x" | "y">;

/** The one edge shape the layout needs: endpoints matched by `(kind, id)` so a
 *  cross-kind id collision can never wire the wrong nodes together. */
export interface FbLayoutEdge {
  src: string;
  dst: string;
  srcKind: FullGraphNodeKind;
  dstKind: FullGraphNodeKind;
}

interface Box {
  /** Half-extent (world units), INCLUDING the member soma radii. */
  halfW: number;
  halfH: number;
  /** Global node indices this box owns (its positions already origin-local). */
  members: number[];
}

function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v;
}

/** Coerce degree to a finite, non-negative number (a single non-finite degree
 *  would otherwise poison `maxDeg` → every radius/position to NaN). Not reachable
 *  via the typed backend, but keeps the pure layout total on any input. */
function degOf(x: FbLayoutNode): number {
  return Number.isFinite(x.degree) ? Math.max(0, x.degree) : 0;
}

/** Per-node soma radius, sqrt(degree)-scaled between MIN_R and MAX_R. */
function computeRadii(nodes: FbLayoutNode[]): number[] {
  const sqMaxDeg = Math.sqrt(Math.max(1, ...nodes.map(degOf)));
  return nodes.map((x) => MIN_R + (Math.sqrt(degOf(x)) / sqMaxDeg) * (MAX_R - MIN_R));
}

/** Edges as index pairs into `nodes`, matched by (kind:id), self-loops dropped. */
function edgeIndexPairs(
  nodes: FbLayoutNode[],
  edges: FbLayoutEdge[],
): [number, number][] {
  const index = new Map<string, number>();
  for (let i = 0; i < nodes.length; i++) {
    index.set(`${nodes[i].kind}:${nodes[i].id}`, i);
  }
  const pairs: [number, number][] = [];
  for (const e of edges) {
    const a = index.get(`${e.srcKind}:${e.src}`);
    const b = index.get(`${e.dstKind}:${e.dst}`);
    if (a === undefined || b === undefined || a === b) {
      continue;
    }
    pairs.push([a, b]);
  }
  return pairs;
}

/**
 * Lay the whole brain out. Returns one placed node per input node (same fields,
 * plus `r`/`x`/`y`), origin-centred. Empty in → empty out.
 */
export function layoutFullBrain(
  inputNodes: FbLayoutNode[],
  inputEdges: FbLayoutEdge[],
): FullSceneNode[] {
  const n = inputNodes.length;
  if (n === 0) {
    return [];
  }

  // ── radii (area ∝ degree ⇒ radius ∝ √degree) ──
  const radii = computeRadii(inputNodes);

  // ── adjacency by (kind:id) — collision-safe endpoint matching ──
  const edgePairs = edgeIndexPairs(inputNodes, inputEdges);

  // ── connected components (union-find; lower root wins ⇒ deterministic) ──
  const parent = new Int32Array(n);
  for (let i = 0; i < n; i++) {
    parent[i] = i;
  }
  const find = (i: number): number => {
    let r = i;
    while (parent[r] !== r) {
      parent[r] = parent[parent[r]];
      r = parent[r];
    }
    return r;
  };
  for (const [a, b] of edgePairs) {
    const ra = find(a);
    const rb = find(b);
    if (ra !== rb) {
      parent[Math.max(ra, rb)] = Math.min(ra, rb);
    }
  }
  // Group nodes by root, preserving ascending-index order within each group.
  const groups = new Map<number, number[]>();
  for (let i = 0; i < n; i++) {
    const r = find(i);
    const g = groups.get(r);
    if (g) {
      g.push(i);
    } else {
      groups.set(r, [i]);
    }
  }

  const xs = new Float64Array(n);
  const ys = new Float64Array(n);

  // Split into real clusters (≥2) and degree-isolated singletons.
  const clusters: number[][] = [];
  const singletons: number[] = [];
  for (const members of groups.values()) {
    if (members.length === 1) {
      singletons.push(members[0]);
    } else {
      clusters.push(members);
    }
  }

  const boxes: Box[] = [];

  // ── lay out each cluster locally (origin-centred), record its bbox ──
  for (const members of clusters) {
    layoutCluster(members, edgePairs, radii, xs, ys);
    boxes.push(bboxOf(members, radii, xs, ys));
  }

  // ── tile every singleton into ONE compact grid block ──
  if (singletons.length > 0) {
    boxes.push(tileSingletons(singletons, radii, xs, ys));
  }

  // ── spiral-pack the boxes center-out, centred on the origin ──
  const offsets = packBoxes(boxes);
  for (let bi = 0; bi < boxes.length; bi++) {
    const { dx, dy } = offsets[bi];
    for (const i of boxes[bi].members) {
      xs[i] += dx;
      ys[i] += dy;
    }
  }

  return inputNodes.map((nd, i) => ({
    ...nd,
    r: radii[i],
    x: xs[i],
    y: ys[i],
  }));
}

/**
 * LAYERED "neural-net" layout — nodes in horizontal BANDS by kind (people/
 * projects → meetings → notes → documents, top to bottom), ordered left-to-right
 * within each band by a barycentric sweep that minimises edge crossings (a
 * connected node drifts over the nodes it links to, so cross-band edges read as
 * clean vertical flows — the "neural net" look). Bands are WIDE not tall, so it
 * fills the usually-wide canvas. Fully deterministic. Empty in → empty out.
 */
export function layoutLayered(
  inputNodes: FbLayoutNode[],
  inputEdges: FbLayoutEdge[],
): FullSceneNode[] {
  const n = inputNodes.length;
  if (n === 0) {
    return [];
  }
  const radii = computeRadii(inputNodes);

  // Fixed semantic top→bottom order; only kinds actually present get a band.
  const KIND_ORDER: FullGraphNodeKind[] = [
    "entity",
    "meeting",
    "note",
    "document",
  ];
  const bandOf = new Map<FullGraphNodeKind, number>();
  const bands: number[][] = [];
  for (const k of KIND_ORDER) {
    if (inputNodes.some((nd) => nd.kind === k)) {
      bandOf.set(k, bands.length);
      bands.push([]);
    }
  }
  for (let i = 0; i < n; i++) {
    bands[bandOf.get(inputNodes[i].kind) as number].push(i);
  }

  const adj: number[][] = inputNodes.map(() => []);
  for (const [a, b] of edgeIndexPairs(inputNodes, inputEdges)) {
    adj[a].push(b);
    adj[b].push(a);
  }

  // Seed each band by degree desc (id tiebreak).
  for (const band of bands) {
    band.sort(
      (a, b) =>
        degOf(inputNodes[b]) - degOf(inputNodes[a]) ||
        (inputNodes[a].id < inputNodes[b].id ? -1 : 1),
    );
  }
  // lane[idx] = position within its band (the crossing-reduction coordinate).
  const lane = new Float64Array(n);
  for (const band of bands) {
    band.forEach((idx, r) => (lane[idx] = r));
  }

  // Barycentric sweeps: each node drifts toward the mean lane of its neighbours,
  // so connected nodes stack over one another and edges stop crossing.
  for (let iter = 0; iter < 14; iter++) {
    for (const band of bands) {
      const bary = band.map((idx) => {
        let sum = 0;
        let cnt = 0;
        for (const nb of adj[idx]) {
          sum += lane[nb];
          cnt++;
        }
        return cnt > 0 ? sum / cnt : lane[idx];
      });
      const order = band
        .map((_, k) => k)
        .sort((x, y) => bary[x] - bary[y] || band[x] - band[y]);
      const reordered = order.map((k) => band[k]);
      for (let r = 0; r < reordered.length; r++) {
        band[r] = reordered[r];
        lane[reordered[r]] = r;
      }
    }
  }

  // Final coords: bands stacked vertically, laid out left→right within each band,
  // both axes centred on the origin.
  const LANE_GAP = 46;
  const BAND_GAP = 200;
  const xs = new Float64Array(n);
  const ys = new Float64Array(n);
  const totalH = (bands.length - 1) * BAND_GAP;
  for (let bi = 0; bi < bands.length; bi++) {
    const band = bands[bi];
    const bandW = (band.length - 1) * LANE_GAP;
    for (let r = 0; r < band.length; r++) {
      xs[band[r]] = r * LANE_GAP - bandW / 2;
      ys[band[r]] = bi * BAND_GAP - totalH / 2;
    }
  }

  return inputNodes.map((nd, i) => ({ ...nd, r: radii[i], x: xs[i], y: ys[i] }));
}

/**
 * Fruchterman-Reingold for ONE connected component, at a FIXED ideal edge length
 * so components stay size-proportionate. Golden-angle seed (deterministic),
 * pairwise repulsion + hub-attenuated springs + a gentle per-node centre pull,
 * then an overlap-relaxation pass. Writes final origin-local coords into the
 * global `xs`/`ys` at the members' indices.
 */
function layoutCluster(
  members: number[],
  allEdges: [number, number][],
  radii: number[],
  xs: Float64Array,
  ys: Float64Array,
): void {
  const m = members.length;
  const local = new Map<number, number>();
  for (let l = 0; l < m; l++) {
    local.set(members[l], l);
  }
  const lr = members.map((g) => radii[g]);

  // Internal edges as local index pairs + local degree.
  const edges: [number, number][] = [];
  const degree = new Int32Array(m);
  for (const [a, b] of allEdges) {
    const la = local.get(a);
    const lb = local.get(b);
    if (la === undefined || lb === undefined) {
      continue;
    }
    edges.push([la, lb]);
    degree[la]++;
    degree[lb]++;
  }

  const lx = new Float64Array(m);
  const ly = new Float64Array(m);
  // SEED: golden-angle (phyllotaxis) spiral keyed by index → identical every run.
  const golden = Math.PI * (3 - Math.sqrt(5));
  const seedR = IDEAL_EDGE * Math.sqrt(m) * 0.5;
  for (let i = 0; i < m; i++) {
    const rr = seedR * Math.sqrt((i + 0.5) / m);
    const ang = i * golden;
    lx[i] = rr * Math.cos(ang);
    ly[i] = rr * Math.sin(ang);
  }

  const k = IDEAL_EDGE;
  const k2 = k * k;
  const iters = clamp(Math.round(2600 / Math.sqrt(m)), 90, MAX_ITERS);
  let temp = k * 1.6;
  const cool = temp / (iters + 1);
  const dxs = new Float64Array(m);
  const dys = new Float64Array(m);

  for (let iter = 0; iter < iters; iter++) {
    dxs.fill(0);
    dys.fill(0);
    // repulsion (all pairs)
    for (let i = 0; i < m; i++) {
      for (let j = i + 1; j < m; j++) {
        let dx = lx[i] - lx[j];
        let dy = ly[i] - ly[j];
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
    // springs (hub-attenuated so a hub doesn't crush its neighbourhood)
    for (const [a, b] of edges) {
      const dx = lx[a] - lx[b];
      const dy = ly[a] - ly[b];
      const dist = Math.sqrt(dx * dx + dy * dy) || 0.01;
      const hub = Math.sqrt(Math.min(degree[a] || 1, degree[b] || 1));
      const force = ((dist * dist) / k / hub) * 0.9;
      const fx = (dx / dist) * force;
      const fy = (dy / dist) * force;
      dxs[a] -= fx;
      dys[a] -= fy;
      dxs[b] += fx;
      dys[b] += fy;
    }
    // integrate + a gentle per-node centre pull (keeps the component compact)
    for (let i = 0; i < m; i++) {
      const dl = Math.sqrt(dxs[i] * dxs[i] + dys[i] * dys[i]) || 1;
      const step = Math.min(dl, temp);
      lx[i] += (dxs[i] / dl) * step;
      ly[i] += (dys[i] / dl) * step;
      lx[i] -= lx[i] * 0.03;
      ly[i] -= ly[i] * 0.03;
    }
    temp = Math.max(0, temp - cool);
  }

  // recentre on the centroid
  let mx = 0;
  let my = 0;
  for (let i = 0; i < m; i++) {
    mx += lx[i];
    my += ly[i];
  }
  mx /= m;
  my /= m;
  for (let i = 0; i < m; i++) {
    lx[i] -= mx;
    ly[i] -= my;
  }

  // overlap relaxation — push any two somas apart until clear
  for (let pass = 0; pass < 30; pass++) {
    let moved = false;
    for (let i = 0; i < m; i++) {
      for (let j = i + 1; j < m; j++) {
        let dx = lx[i] - lx[j];
        let dy = ly[i] - ly[j];
        let dist = Math.sqrt(dx * dx + dy * dy);
        const need = lr[i] + lr[j] + NODE_GAP;
        if (dist >= need) {
          continue;
        }
        if (dist < 0.01) {
          dx = ((i * 31 + j) % 7) - 3 + 0.5;
          dy = ((i * 17 + j) % 5) - 2 + 0.5;
          dist = Math.sqrt(dx * dx + dy * dy);
        }
        const push = (need - dist) / 2 / dist;
        lx[i] += dx * push;
        ly[i] += dy * push;
        lx[j] -= dx * push;
        ly[j] -= dy * push;
        moved = true;
      }
    }
    if (!moved) {
      break;
    }
  }

  for (let i = 0; i < m; i++) {
    xs[members[i]] = lx[i];
    ys[members[i]] = ly[i];
  }
}

/** Tile the degree-isolated singletons into one compact, origin-centred grid. */
function tileSingletons(
  singletons: number[],
  radii: number[],
  xs: Float64Array,
  ys: Float64Array,
): Box {
  const count = singletons.length;
  let r = MIN_R;
  for (const i of singletons) {
    r = Math.max(r, radii[i]);
  }
  const cell = 2 * r + TILE_GAP;
  const cols = Math.max(1, Math.round(Math.sqrt(count * TARGET_ASPECT)));
  const rows = Math.ceil(count / cols);
  const blockW = cols * cell;
  const blockH = rows * cell;
  for (let kk = 0; kk < count; kk++) {
    const col = kk % cols;
    const row = Math.floor(kk / cols);
    xs[singletons[kk]] = (col + 0.5) * cell - blockW / 2;
    ys[singletons[kk]] = (row + 0.5) * cell - blockH / 2;
  }
  return { halfW: blockW / 2, halfH: blockH / 2, members: singletons };
}

/** Bounding half-extents of a component's placed somas (radii included). */
function bboxOf(
  members: number[],
  radii: number[],
  xs: Float64Array,
  ys: Float64Array,
): Box {
  let hw = 1;
  let hh = 1;
  for (const i of members) {
    hw = Math.max(hw, Math.abs(xs[i]) + radii[i]);
    hh = Math.max(hh, Math.abs(ys[i]) + radii[i]);
  }
  return { halfW: hw, halfH: hh, members };
}

/**
 * SPIRAL-PACK the component boxes center-out (Freivalds–Doğrusöz–Kikusts): place
 * each box, largest-first, at the free lattice cell nearest the centre (the cost
 * is `max(|x|, |y|·aspect)` — see {@link SORTED_LATTICE}), so the dominant cluster
 * anchors the middle and every smaller component fills a GAP around it rather than
 * trailing off in a shelf row. Then centre the packed cloud on the origin. Returns
 * one `{dx,dy}` shift per box (index-aligned with `boxes`). Fully deterministic;
 * no two boxes overlap (half-extents include {@link COMPONENT_SPACING}).
 */
function packBoxes(boxes: Box[]): { dx: number; dy: number }[] {
  const nb = boxes.length;
  if (nb === 0) {
    return [];
  }
  // Padded half-extents (the spacing gap is baked into the overlap test).
  const hw = boxes.map((b) => b.halfW + COMPONENT_SPACING / 2);
  const hh = boxes.map((b) => b.halfH + COMPONENT_SPACING / 2);
  if (nb === 1) {
    return [{ dx: 0, dy: 0 }];
  }

  // Largest-first (area desc, index tiebreak) — anchors the composition centre.
  const order = boxes
    .map((_, i) => i)
    .sort((a, b) => hw[b] * hh[b] - hw[a] * hh[a] || a - b);

  // Grid step = the smallest box's min half-extent (clamped): fine enough to nest
  // tightly, coarse enough that the ±PACK_R lattice spans the whole layout.
  let smallest = Infinity;
  for (let i = 0; i < nb; i++) {
    smallest = Math.min(smallest, Math.min(hw[i], hh[i]));
  }
  const step = clamp(smallest, 20, 70);

  const placed: { cx: number; cy: number; hw: number; hh: number }[] = [];
  const centre = new Array<{ x: number; y: number }>(nb);
  for (const idx of order) {
    let cx = 0;
    let cy = 0;
    if (placed.length > 0) {
      for (const [gx, gy] of SORTED_LATTICE) {
        const tx = gx * step;
        const ty = gy * step;
        let ok = true;
        for (const p of placed) {
          if (
            Math.abs(tx - p.cx) < hw[idx] + p.hw &&
            Math.abs(ty - p.cy) < hh[idx] + p.hh
          ) {
            ok = false;
            break;
          }
        }
        if (ok) {
          cx = tx;
          cy = ty;
          break;
        }
      }
    }
    centre[idx] = { x: cx, y: cy };
    placed.push({ cx, cy, hw: hw[idx], hh: hh[idx] });
  }

  // Centre the whole packed cloud on the origin.
  let minX = Infinity;
  let maxX = -Infinity;
  let minY = Infinity;
  let maxY = -Infinity;
  for (let i = 0; i < nb; i++) {
    minX = Math.min(minX, centre[i].x - hw[i]);
    maxX = Math.max(maxX, centre[i].x + hw[i]);
    minY = Math.min(minY, centre[i].y - hh[i]);
    maxY = Math.max(maxY, centre[i].y + hh[i]);
  }
  const mx = (minX + maxX) / 2;
  const my = (minY + maxY) / 2;
  return centre.map((c) => ({ dx: c.x - mx, dy: c.y - my }));
}
