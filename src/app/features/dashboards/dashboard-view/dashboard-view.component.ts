import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  effect,
  inject,
  signal,
  untracked,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { ActivatedRoute, Router } from "@angular/router";
import { IpcService } from "../../../core/ipc.service";
import { DashboardsService } from "../../../services/dashboards.service";
import { MurEmptyStateComponent } from "../../../design-system/empty-state/empty-state.component";
import { MurIconComponent } from "../../../design-system/icon/icon.component";
import { MurSpinnerComponent } from "../../../design-system/spinner/spinner.component";
import { DashboardTileComponent } from "../dashboard-tile/dashboard-tile.component";
import {
  TilePaletteService,
  type TileChoice,
} from "../../../services/tile-palette.service";
import type { ResolvedTile, SourceRef } from "../../../core/models";

/** One exchange in the board-scoped Ask column. */
interface BoardTurn {
  role: "user" | "assistant";
  text: string;
  /** Tile ids this answer was grounded in (assistant turns only). */
  citedTiles?: string[];
}

const SUGGESTIONS = [
  "What's most likely to go wrong here?",
  "Who owes me something on this board?",
  "What changed since last week?",
];

/**
 * `/dashboards/:id` — one board.
 *
 * Two halves: the tile canvas, and **Ask this board** — which is the whole
 * point of the feature. Ask reuses the SHIPPED `ask_vault(explicitSources: …)`
 * path with the board's own visible sources (`getDashboardSources`), so:
 *   * retrieval is pinned to exactly what the user composed (no vault-wide
 *     wandering), and
 *   * there is no new AI command, no new egress surface, and no new redaction
 *     seam — the existing consent/redaction/ledger path applies verbatim.
 *
 * Sealed sources never enter that list (the backend filters them), so a
 * board-scoped Ask can't retrieve from a locked folder even when the board
 * shows a redacted tile for it.
 */
@Component({
  selector: "app-dashboard-view",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    MurEmptyStateComponent,
    MurIconComponent,
    MurSpinnerComponent,
    DashboardTileComponent,
  ],
  templateUrl: "./dashboard-view.component.html",
  styleUrl: "./dashboard-view.component.scss",
})
export class DashboardViewComponent {
  private readonly service = inject(DashboardsService);
  private readonly ipc = inject(IpcService);
  private readonly router = inject(Router);
  private readonly route = inject(ActivatedRoute);
  private readonly palette = inject(TilePaletteService);

  constructor() {
    // The palette is app-wide, so leaving the board must not strand it on screen
    // over whatever route comes next.
    inject(DestroyRef).onDestroy(() => {
      if (this.palette.open()) this.palette.dismiss();
    });
  }

  /**
   * The board id, as a SIGNAL off the router's own paramMap — not
   * `snapshot.paramMap` in `ngOnInit`. `/dashboards/:id` is not in
   * `TabRouteReuseStrategy`'s scope today, but if it ever were, a snapshot read
   * would silently stop re-firing when navigating board→board. Reading the
   * observable keeps the load correct under either strategy.
   */
  private readonly params = toSignal(this.route.paramMap);
  readonly id = computed(() => this.params()?.get("id") ?? "");

  readonly board = this.service.board;
  readonly loading = this.service.boardLoading;
  readonly error = this.service.error;

  private readonly serverTiles = computed<ResolvedTile[]>(
    () => this.board()?.tiles ?? [],
  );
  /** Server order, unless a drag is mid-flight and we are showing its result. */
  readonly tiles = computed<ResolvedTile[]>(() => {
    const rows = this.serverTiles();
    const order = this.orderOverride();
    if (!order) return rows;
    const byId = new Map(rows.map((t) => [t.id, t]));
    const out = order.map((id) => byId.get(id)).filter((t): t is ResolvedTile => !!t);
    // Anything the override does not mention (a tile added meanwhile) keeps its
    // place at the end rather than vanishing.
    for (const t of rows) if (!order.includes(t.id)) out.push(t);
    return out;
  });
  readonly isEmpty = computed(() => this.tiles().length === 0);
  readonly sealedCount = computed(
    () => this.tiles().filter((t) => t.data.kind === "locked").length,
  );

  /**
   * Tile id → the heading of the EARLIER tile it duplicates.
   *
   * Two tiles that resolve to the same payload render the same rows twice, which
   * is what made a nine-tile board read as a duplicated one. The cause is a
   * missing parameter rather than user error — `tile-palette` never writes
   * `config.owner`, so every Promise ledger resolves to the same global list —
   * so the second tile becomes a back-reference and the fix costs no backend.
   *
   * Keyed on the RESOLVED payload, not on `(kind, refId, config)`: two tiles can
   * be configured differently and still resolve identically, and it is the
   * on-screen repetition that the user sees.
   */
  readonly duplicates = computed<ReadonlyMap<string, string>>(() => {
    const seen = new Map<string, string>();
    const out = new Map<string, string>();
    for (const t of this.tiles()) {
      // A sealed tile carries no fields at all, so every sealed tile would look
      // like every other one. Redaction is not duplication — never collapse them.
      if (t.data.kind === "locked") continue;
      const key = JSON.stringify(t.data);
      const first = seen.get(key);
      if (first === undefined) seen.set(key, this.headingOf(t));
      else out.set(t.id, first);
    }
    return out;
  });

  duplicateOf(tile: ResolvedTile): string | null {
    return this.duplicates().get(tile.id) ?? null;
  }

  /** The label a back-reference points at — the user-visible name of the original. */
  private headingOf(tile: ResolvedTile): string {
    if (tile.title && tile.title.trim()) return tile.title.trim();
    const d = tile.data;
    if (d.kind === "note" || d.kind === "meeting" || d.kind === "document") return d.title;
    if (d.kind === "person") return d.name;
    if (d.kind === "promises") return d.owner ? `Promises · ${d.owner}` : "Promises";
    return d.kind;
  }

  /** Owned by the root service — the palette itself is rendered by `app-shell`. */
  readonly paletteOpen = this.palette.open;
  readonly editing = signal(false);

  // ── drag-to-reorder (Arrange mode) ─────────────────────────────────────────
  //
  // Native HTML5 drag events, deliberately: no new dependency, no DOM observer,
  // and the drop target is the tile itself so the grid needs no hit-testing.
  // The order is applied OPTIMISTICALLY to a local override so the board does not
  // jump while the backend write is in flight; the reload then replaces it.
  readonly draggingId = signal<string | null>(null);
  readonly dropTargetId = signal<string | null>(null);
  private readonly orderOverride = signal<string[] | null>(null);

  onDragStart(tile: ResolvedTile, event: DragEvent): void {
    if (!this.editing()) return;
    this.draggingId.set(tile.id);
    event.dataTransfer?.setData("text/plain", tile.id);
    if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
  }

  onDragOver(tile: ResolvedTile, event: DragEvent): void {
    if (!this.editing() || !this.draggingId()) return;
    // Without preventDefault the browser refuses the drop outright.
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
    if (this.dropTargetId() !== tile.id) this.dropTargetId.set(tile.id);
  }

  onDragEnd(): void {
    this.draggingId.set(null);
    this.dropTargetId.set(null);
  }

  async onDrop(target: ResolvedTile, event: DragEvent): Promise<void> {
    event.preventDefault();
    const sourceId = this.draggingId();
    this.draggingId.set(null);
    this.dropTargetId.set(null);
    if (!sourceId || sourceId === target.id) return;

    const ids = this.tiles().map((t) => t.id);
    const from = ids.indexOf(sourceId);
    const to = ids.indexOf(target.id);
    if (from < 0 || to < 0) return;
    ids.splice(to, 0, ...ids.splice(from, 1));

    this.orderOverride.set(ids);
    try {
      await this.service.reorderTiles(this.id(), ids);
    } finally {
      // The reload inside reorderTiles is authoritative; drop the override so a
      // rejected write cannot leave the UI showing an order the backend refused.
      this.orderOverride.set(null);
    }
  }

  // ── board Ask ──────────────────────────────────────────────────────────────
  readonly turns = signal<BoardTurn[]>([]);
  readonly asking = signal(false);
  readonly draft = signal("");
  readonly sourceCount = signal(0);
  readonly suggestions = SUGGESTIONS;

  /** Tile id → 1-based citation index for the LAST answer. */
  readonly citations = computed(() => {
    const last = [...this.turns()].reverse().find((t) => t.role === "assistant");
    const map = new Map<string, number>();
    last?.citedTiles?.forEach((tileId, i) => map.set(tileId, i + 1));
    return map;
  });

  /**
   * Load the board whenever the route id changes, and drop any previous Ask
   * thread — a conversation scoped to one board must never carry into another.
   */
  private readonly _load = effect(() => {
    const id = this.id();
    if (!id) return;
    // `untracked` is load-bearing, not defensive. `refresh()` runs SYNCHRONOUSLY
    // up to its first await, and `DashboardsService.loadBoard` reads `board()`
    // in there (to decide whether to clear the stale board). Without this the
    // effect would take a dependency on `board`, which the same call then
    // writes — an endless reload loop that pins the view on "Loading this
    // board…" forever. The e2e spec for an empty board is the regression guard.
    untracked(() => {
      this.turns.set([]);
      void this.refresh(id);
    });
  });

  private async refresh(id: string): Promise<void> {
    await this.service.loadBoard(id);
    // Same stale-result discipline as the service: a late source-count for the
    // board we just navigated AWAY from must not overwrite the current one.
    try {
      const count = (await this.ipc.getDashboardSources(id)).length;
      if (this.id() === id) this.sourceCount.set(count);
    } catch {
      if (this.id() === id) this.sourceCount.set(0);
    }
  }

  citationFor(tile: ResolvedTile): number {
    return this.citations().get(tile.id) ?? 0;
  }

  back(): void {
    void this.router.navigate(["/dashboards"]);
  }

  toggleEditing(): void {
    this.editing.update((v) => !v);
  }

  /** Open the palette and add whatever the user picked (nothing if dismissed). */
  async openPalette(): Promise<void> {
    const choice = await this.palette.request();
    if (choice) await this.addTile(choice);
  }

  /**
   * The trigger is a TOGGLE, and its label follows the state ("Add tile" ⇄ "Close").
   *
   * That is ordinary UX — a control that opens a modal should close it — but it is
   * also the cheapest possible signal that the click landed at all. If the label
   * flips and no palette appears, the fault is presentation; if the label does not
   * flip, the click never reached the handler. Without it those two failures look
   * identical from the outside, which is what made the first report hard to place.
   */
  togglePalette(): void {
    if (this.paletteOpen()) this.palette.dismiss();
    else void this.openPalette();
  }

  async addTile(choice: TileChoice): Promise<void> {
    await this.service.addTile(this.id(), choice.kind, {
      refId: choice.refId,
      title: choice.title,
      config: choice.config,
    });
    this.sourceCount.set((await this.ipc.getDashboardSources(this.id())).length);
  }

  async removeTile(tile: ResolvedTile): Promise<void> {
    await this.service.removeTile(tile.id);
    this.sourceCount.set((await this.ipc.getDashboardSources(this.id())).length);
  }

  async widen(tile: ResolvedTile): Promise<void> {
    await this.service.updateTile(tile.id, { span: Math.min(12, tile.span + 1) });
  }

  async narrow(tile: ResolvedTile): Promise<void> {
    await this.service.updateTile(tile.id, { span: Math.max(3, tile.span - 1) });
  }

  /** Click-through from a tile row to its source. */
  openSource(source: SourceRef): void {
    switch (source.kind) {
      case "meeting":
        void this.router.navigate(["/meeting", source.id]);
        break;
      case "note":
        void this.router.navigate(["/notes", source.id]);
        break;
      default:
        // Documents have no dedicated route; the brain view owns them.
        void this.router.navigate(["/brain"]);
    }
  }

  onDraft(event: Event): void {
    this.draft.set((event.target as HTMLInputElement).value);
  }

  askSuggestion(text: string): void {
    this.draft.set(text);
    void this.ask();
  }

  /**
   * Ask the board. The answer is grounded in the board's VISIBLE sources only;
   * tiles whose source the answer cited light up and number themselves.
   */
  async ask(): Promise<void> {
    const question = this.draft().trim();
    if (!question || this.asking()) return;
    this.draft.set("");
    this.turns.update((t) => [...t, { role: "user", text: question }]);
    this.asking.set(true);
    try {
      const sources = await this.ipc.getDashboardSources(this.id());
      this.sourceCount.set(sources.length);
      if (sources.length === 0) {
        this.turns.update((t) => [
          ...t,
          {
            role: "assistant",
            text: "This board has no readable sources yet — add a note, recording or document tile (or unlock a sealed one) and ask again.",
          },
        ]);
        return;
      }
      const result = await this.ipc.askVault(question, [], undefined, sources);
      this.turns.update((t) => [
        ...t,
        {
          role: "assistant",
          text: result.answer,
          citedTiles: this.tilesForSources(result.sources.map((s) => s.meetingId)),
        },
      ]);
    } catch (e) {
      this.turns.update((t) => [
        ...t,
        { role: "assistant", text: this.errorText(e) },
      ]);
    } finally {
      this.asking.set(false);
    }
  }

  /** Map the answer's source ids back onto the tiles that carry them. */
  private tilesForSources(sourceIds: string[]): string[] {
    const wanted = new Set(sourceIds);
    return this.tiles()
      .filter((t) => t.refId && wanted.has(t.refId))
      .map((t) => t.id);
  }

  /** Re-run a Living-answer tile's pinned question and persist the result. */
  async refreshAnswer(tile: ResolvedTile): Promise<void> {
    if (tile.data.kind !== "livingAnswer") return;
    const question = tile.data.question.trim();
    if (!question) return;
    try {
      const sources = await this.ipc.getDashboardSources(this.id());
      if (sources.length === 0) return;
      const result = await this.ipc.askVault(question, [], undefined, sources);
      // The BACKEND persists this, so it can stamp the readable-folder snapshot
      // that gates the cached answer. Writing it from here through the generic
      // tile update is what left the cache un-gateable.
      await this.ipc.setDashboardAnswer(tile.id, question, result.answer);
      await this.service.loadBoard(this.id());
    } catch {
      // The service surfaces the error banner; a failed re-answer leaves the
      // previous answer intact rather than blanking the tile.
    }
  }

  private errorText(e: unknown): string {
    if (typeof e === "string") return e;
    if (e && typeof e === "object") {
      const v = Object.values(e as Record<string, unknown>)[0];
      if (typeof v === "string") return v;
    }
    return "That didn't work. Try again.";
  }
}
