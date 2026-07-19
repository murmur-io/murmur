<!-- Generated 2026-07-19 via /research (murmur-researcher fan-out ×3: layout science / UX patterns / vanilla-canvas techniques). -->
# Research: Full-brain graph visualization redesign — see ALL nodes, no scatter, professional look

> **Addendum (2026-07-19, during implementation).** After the organic per-component
> fix landed and was verified, the user judged the result "constellations, not an
> advanced structure" and pointed at layered neural-network visualisations as the
> aspiration. Honest constraint: a neural net looks dense because it is fully
> connected in layers; Murmur's brain is a genuinely SPARSE knowledge graph (many
> orphan docs/notes) — we cannot fabricate connections that don't exist. So the
> ship added, on top of the organic layout: (1) a LAYERED "neural" layout mode
> (horizontal bands by kind — people→meetings→notes→documents — barycentrically
> ordered to minimise edge crossings), now the DEFAULT, with the organic layout
> kept behind a `Layers | Clusters` toggle; (2) rich GLOWING curved synapses that
> gradient between endpoint node hues; (3) a bounded, reduced-motion-safe animation
> loop with FIRING PULSES travelling src→dst (data flowing down the layers) +
> soma breathing. Both adversarial-verifier and lock-security-reviewer re-passed
> the addition (loop §5-bounded on destroy/hidden/reduced-motion, `layoutLayered`
> deterministic, no leak). The "advanced" feel now comes from STRUCTURE + FLOW,
> not fabricated density.

## TL;DR / Verdict

The graph looks broken because of **three compounding bugs**, not one — and the fix is well-founded, vanilla-TS, no new deps, and largely a **port from our own sibling `neural-scene.directive.ts`** (which already solved ~80% of the "pro but cheap" rendering).

1. **Degree-0 singletons (backend, confirmed).** `build_full_graph` (`storage/graph_store.rs:580-614`) pushes *every* visible entity/meeting/note/document as a node with `degree: 0` **before** any edge is counted. So every orphan document (no wikilink), fresh note, or meeting whose entities co-occur nowhere else is a genuine **1-node connected component**.
2. **Global force-sim flings them + weak gravity (layout).** One global Fruchterman-Reingold over all nodes repels those singletons outward with long-reach inverse-linear `k²/d`, held back only by a per-iteration `xs[i]*0.006` centre pull that is **~40× too weak** (industry per-node positional gravity ≈ 0.1/tick). There is **no connected-component packing**. → the three corner-clusters in the screenshot.
3. **Fit zooms to the blown-out bbox (renderer).** `fit()` (`full-brain-scene.directive.ts:349-372`) scales to `max(|x|+r)`, which the scattered singletons dominate → `scale` collapses toward `MIN_SCALE=0.25` → `p.sr < 7` → **labels never render** → "unlabeled dots."

**Fix:** (A) replace the single global sim with **per-connected-component layout + bounding-box spiral packing** (the technique `fcose`/Graphviz/Gephi use), tiling degree-0 singletons into one compact grid block; (B) **port the neural-scene rendering** — cached halo sprites, collision-decluttered adaptive labels, opaque hover tooltip, batched edges, robust mass-centroid fit; (C) fix the interaction (**click = focus/stay, double-click = open** — today click yanks you to a route), add **Fit + zoom-%**, and **raise the 140 draw cap** so a normal vault shows everything. Skip Barnes-Hut (crossover ~6000 nodes; we're ≤500 and the layout is one-shot off the render thread), skip polyomino-exact packing, skip minimap/hulls for now.

On the user's explicit *"see ALL nodes"* vs. the research caution *"a full global graph past ~200 nodes is a useless hairball"*: **both are satisfied by good layout, not by hiding nodes.** The user's vault is ~155 items — trivially renderable in full. Component-packing + tiling keeps even ~500 nodes compact and legible; the hairball concern only bites pathological 2000-node vaults, where LOD labels + search are the answer. So we raise the cap AND make it legible.

## What we already have (from repo, file:line)

- **The component:** `src/app/features/brain/full-brain-graph/full-brain-graph.component.ts` — `sceneNodes` computed (`:234-388`) is a one-shot deterministic 2D FR: golden-angle spiral seed (`:254`), all-pairs O(n²) repulsion `k²/dist` (`:287-305`), hub-attenuated springs (`:314-317`), weak centre pull `xs[i]*0.006` (`:330-331`), centroid recenter (`:336-348`), 30-pass overlap relax (`:351-378`). `MAX_NODES=140` draw cap (`:41`). Radius `7+(√deg/√maxDeg)·13` → [7,20] sqrt-scaled (`:249-251`) — already best-practice. **Deterministic (no `Math.random`); must stay so** (the `computed` re-runs on every lens toggle).
- **The renderer:** `full-brain-scene.directive.ts` — per-node `createRadialGradient` ×2 per frame (`:501`,`:518`), `fit()` zoom-to-bbox (`:349`), `MIN_SCALE=0.25` (`:63`), primitive label gate `p.sr >= 7` with **no collision test** (`:545-546`), per-edge state changes (`:472-476`), focus-fade only on select (`:141-155`,`:446-463`).
- **The sibling to port from:** `src/app/features/brain/neural-scene.directive.ts` — already ships **cached halo sprites** (`haloSprite` `:1537`), **cached backdrop** (`makeBg` `:1453`), **collision-decluttered fan-out labels + zoom-scaled budget** (`drawLabels` `:1160`, `collides` `:1227`, fan-out `:1248`, budget `LABEL_TOP*zoomK` `:1190`), **opaque hover tooltip** (`drawTooltip` `:1283`), **robust mass-centroid fit** (`fitCamera` `:1350`, 30% centroid blend `:1404-1408`), **bounded reduced-motion-safe loop** (`startLoop`/`stopLoop`/`invalidate` `:595`/`:610`/`:620`, all released in `onDestroy` `:464`), djb2/r01 deterministic hashes (`:150`,`:159`). This is the reference implementation; the full-brain scene is the un-migrated laggard.
- **The backend caps:** `MAX_FULL_GRAPH_PER_KIND=500` (`storage/db.rs:6420`) → up to ~2000 nodes; `MAX_FULL_GRAPH_LINK_EDGES=4000` (`:6428`), `MAX_MENTION_EDGES=2000` (`graph_store.rs:865`). `FullGraphNode` carries `{id,kind,label,date,degree}` (`models.ts:1499`); edges carry `srcKind/dstKind` (`:1533`) — enough to build correct component adjacency and a richer hover card, no backend change.
- **The search pattern to copy:** `graph.component.ts:112-136` — the `query()` signal + filter, proven shape for an in-canvas search box.
- **Colors:** `--graph-entity/-meeting/-note/-document` (`design-tokens/colors.css:66-69` + light overrides `theme-light.css:43-46`); the scene mirrors them as fixed constants (dark field — glow only reads on dark).

## Findings

### A — Layout science (per-component packing is the root-cause fix)

- **Every serious tool lays out each connected component independently, then packs the bounding shapes** — no inter-component repulsion, no wasted space. Graphviz `pack`/`packmode=graph` packs component **bounding boxes** (polyomino degenerates to a rectangle), 8pt margins; `fcose`/`cose-bilkent` pack via polyomino with `desiredAspectRatio=1`, `componentSpacing=80`, and **tile degree-0 nodes into a grid** (`tile:true`, padding 10) instead of force-laying them. [high] (Graphviz packMode docs; cytoscape layout-utilities/fcose/cose-bilkent READMEs)
- **The packing algorithm (Freivalds–Doğrusöz–Kikusts, GD 2001):** sort components largest-first by bbox perimeter; place each at the grid cell minimizing **`max(|x|,|y|)`** → a **square spiral outward from centre**; grid cells shaped `DAR:1` where `DAR = canvasW/canvasH` for aspect-fit; enlarge each box by half-spacing for gaps. O(n²) in components — trivial at our scale. **Bounding-box packing (not full polyomino) captures ~all the benefit** for our data shape (one dominant cluster + many singletons). [high / med on the box≈polyomino gap]
- **Gravity that actually contains components is per-node positional (d3 `forceX/forceY`, default strength 0.1 applied every tick), NOT `forceCenter`** (which only recenters the centre of mass — exactly our weak spot). ForceAtlas2 "strong gravity" `∝ d²` forces very compact layouts. Our `*0.006`-once is the known-broken pattern. **But with component packing, per-node gravity becomes near-irrelevant** — it only tidies each component's own layout; the packer handles inter-component compactness. [high]
- Inverse-**square** repulsion (`1/d²`, Gephi) has shorter reach than our inverse-**linear** (`k²/d`) → less singleton fling; capping repulsion at a `distanceMax` also helps. [med]
- Node sizing: our sqrt radius is correct (area ∝ degree). For a mega-hub that dominates, quasi-log `log(1+deg)/log(1+maxDeg)` is the escape hatch. [high]

### B — UX / interaction (what makes it feel pro, ranked)

- **#1 gap: adaptive collision-avoided labels.** Obsidian's own users beg for "map-style" labels (big nodes labelled when zoomed out, more on zoom-in) instead of its all-or-nothing zoom fade. The algorithm — greedy, largest-first, screen-space rect-occlusion, zoom-gated budget — **is already implemented in our `neural-scene.drawLabels`.** Porting it erases the "unlabeled dots" look. [high]
- **The click-yanks-away bug:** today a node click routes to the item (`onPick` → `router.navigate`), so a user can never *dwell* on a node. Best-in-class: **single click = select + focus its neighbourhood (stay), double-click / a hover-card "Open" = navigate.** Also move the neighbour-fade onto **hover** (today it's only on select). [high]
- **Orientation:** a proper **Fit** affordance + a **zoom-% readout**; an in-canvas **search-to-node** (type → matches glow, rest dim, camera eases to best hit) — the "find a node" the user is really asking for. [high]
- **Strategic:** the global graph is a "poster," the local/focus graph + search is the "tool," past ~200 nodes. Bias future work toward land-on-one-node-and-explore, not bigger hairballs. [high]
- **Defer:** community-detection colouring, convex-hull cluster blobs, minimap — diminishing returns until graphs are routinely huge. [med]

### C — Vanilla-canvas implementation (port the sibling; skip the quadtree)

- **Replace per-node `createRadialGradient` (×2/node/frame, and it leaks) with cached per-kind sprites** (`haloSprite`) + `globalCompositeOperation="lighter"` additive glow; pre-render the backdrop once (`makeBg`). Real win from ~150 nodes, mandatory by 400. [high]
- **Skip Barnes-Hut.** Heer's benchmark: θ=0.5 doesn't beat naive O(n²) until **~6000 points** (tree-build overhead); we're ≤500 and the layout is a one-shot `computed()` off the render thread (~33M ops once at n=500 ≈ a few ms, invisible). Only revisit for a per-frame animated sim or n≫600. [high]
- **Batch edges by (kind, dashed) into one path per style**; hoist `strokeStyle`/`lineWidth`/`setLineDash` out of the per-edge loop (MDN: batch calls, avoid state changes). At 4000 edges this matters. [high]
- **Bounded animated "settle"** is honest under zoneless §5 if it *terminates*: ease node positions seed→final over ~600ms via one rAF chain that stops at t=1, snapped under `prefers-reduced-motion`, released on destroy — the neural-scene loop lifecycle proves the pattern. This is presentation-only (the deterministic final layout is precomputed). [high]
- **Robust fit:** port `fitCamera`'s **30%-mass-centroid blend** so a couple of stray singletons can't park the mass in a corner / collapse the scale. [high]

## Fit with Murmur's constraints

- **No new deps** — everything is algorithmic vanilla TS on canvas (union-find, spiral box-packer, sprite cache, collision labels). ✔ [high]
- **Local-first / SQLite-canonical / lock model** — pure presentation over already-`getFullGraph()`-gated data. Zero egress, zero backend, zero schema change. Raising the draw cap surfaces **more of what the backend already deemed visible** → **no new leak surface**, and it makes the honest "Drawing N of M" disclosure fire *less* (a correctness win). One caveat to respect: if focus-depth ever expands beyond 1 hop on the *drawn* set, an n-hop walk must honestly account for the draw cap. [high]
- **Zoneless §5** — layout stays a pure `computed()` in the component; all DOM-loop concerns (RO, bounded settle rAF, listeners) live in the directive, released in `onDestroy` — the exact shape neural-scene already ships. Determinism preserved (union-find in sorted order, golden-angle seed, `max(|x|,|y|)` spiral — no `Math.random`). ✔ [high]
- **CI / honesty bar** — FE-only, testable headless (Playwright `:4210` + `mock-tauri.js` with a seeded multi-component `FullGraphData`). But "professional look" is a **real-render judgment**: a green `ng build` proves nothing; the deliverable must be verified with screenshots against a realistic multi-component fixture at min/mid/max zoom. [high]

## Options & tradeoffs

- **Option A — Quick win (S):** per-node positional gravity (~0.04–0.06/iter) replacing `*0.006`, softer/`1/d²`-capped repulsion, robust `fit()` (mass-centroid blend / raise `MIN_SCALE`). ~70% of the scatter fix, ships fast, but a global sim still can't fully centralise k singletons. Stopgap.
- **Option B — The real fix (M):** connected-components (union-find) → per-component FR (reuse existing) → tile degree-0 singletons into one grid block → **`max(|x|,|y|)` spiral bounding-box packing**, aspect-aware → per-component shift. Compact, centred, no corner-scatter, aspect-filled. **This is the layout decision.**
- **Option C — Renderer parity (M, do with B):** port `haloSprite`/`makeBg`/`drawLabels`/`drawTooltip`/`fitCamera` + batched edges + hover-mute + zoom-% + Fit + a bounded settle; raise `MAX_NODES→~500`; click=focus/dblclick=open. **This is the "stop looking bare" decision.**
- **Option D — Showcase (L, defer):** in-canvas search-to-node, community colouring, hulls, minimap, polyomino-exact packing. Search is the most valuable of these and a strong fast-follow; the rest are diminishing returns.

## Recommendation & first step

**Ship B + C together** (they touch the same two files and are jointly the redesign the user asked for), with **search (from D) as the immediate fast-follow**. Skip Barnes-Hut, polyomino-exact, minimap/hulls.

**Smallest verifiable first slice / de-risk spike:** implement `packComponents(boxes:{w,h}[], aspect) → {dx,dy}[]` as a standalone pure deterministic function (unit-test: 3 boxes → known offsets), plus the union-find split, then render a **seeded fixture** (one ~30-node cluster + ~40 degree-0 singletons) via the Playwright mock and screenshot at min/mid/max zoom. Success = the drawn node-centre bbox is a small, centred fraction of the canvas (not three corners) and ≥N labels render (scale didn't collapse to `MIN_SCALE`). That same fixture verifies the whole redesign.

## Open questions / what I couldn't verify

- **Real vault component distribution** — the *mechanism* creating singletons is proven (`build_full_graph` adds all nodes at degree 0), but not the live count of isolated components in this vault. If almost everything is connected, Option A alone might suffice — but B is robust either way. (Cheap check: log `components.length` + singleton count on load.)
- **Obsidian's factory force defaults** — parameter names/roles solid; exact default numbers not published (one user's custom config only).
- **box-packing vs polyomino compactness gap on *our* data** — inference (mostly singletons + one dominant cluster → box ≈ polyomino); only a real-render comparison settles it.
- **"Professional" quality + per-frame cost at 400–500 nodes** — inherently a real-render/Playwright-timing judgment on a real Mac, not a unit test. The honest bar: screenshots + a repaint-time measurement, not a green build.

## Sources

**External (fetched):** Graphviz packMode docs; cytoscape.js layout-utilities / fcose / cose-bilkent READMEs; Freivalds–Doğrusöz–Kikusts "Disconnected Graph Layout and the Polyomino Packing Approach" (GD 2001, Bilkent PDF); ForceAtlas2 (Jacomy et al., PLOS ONE 2014) + Gephi `ForceFactory.java`; d3-force `forceX/forceY` position docs + D3-in-Depth force layout; Jeff Heer "The Barnes-Hut Approximation" (~6000-node crossover); MDN "Optimizing canvas" + `createRadialGradient`/`globalCompositeOperation`; d3 `alphaDecay≈0.0228`/`alphaMin=0.001` (300-tick stop); Cytoscape Rendering-Engine LOD (labels <200 nodes, coarse LOD ≥4000); Kumu Focus/SNA (hover-mute, adjustable degree); Obsidian graph-view help + forum (text-fade-threshold, local-graph depth); Sigma.js (search + neighbourhood); d3-hierarchy #86 (radius ∝ √value).

**Code (this repo):** `src/app/features/brain/full-brain-graph/full-brain-graph.component.ts:41,234-388`; `full-brain-scene.directive.ts:63,349-372,472-476,501,518,545-546`; `src/app/features/brain/neural-scene.directive.ts:150,159,595,610,620,1160,1190,1227,1248,1283,1350,1404-1408,1453,1537`; `src-tauri/src/storage/graph_store.rs:567-700,865`; `src-tauri/src/storage/db.rs:6420,6428`; `src/app/core/models.ts:1499-1567`; `src/app/features/graph/graph/graph.component.ts:112-136`; `src/design-tokens/colors.css:66-69`.
