import {
  ChangeDetectionStrategy,
  Component,
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
  TilePaletteComponent,
  type TileChoice,
} from "../tile-palette/tile-palette.component";
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
    TilePaletteComponent,
  ],
  templateUrl: "./dashboard-view.component.html",
  styleUrl: "./dashboard-view.component.scss",
})
export class DashboardViewComponent {
  private readonly service = inject(DashboardsService);
  private readonly ipc = inject(IpcService);
  private readonly router = inject(Router);
  private readonly route = inject(ActivatedRoute);

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

  readonly tiles = computed<ResolvedTile[]>(() => this.board()?.tiles ?? []);
  readonly isEmpty = computed(() => this.tiles().length === 0);
  readonly sealedCount = computed(
    () => this.tiles().filter((t) => t.data.kind === "locked").length,
  );

  readonly paletteOpen = signal(false);
  readonly editing = signal(false);

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

  openPalette(): void {
    this.paletteOpen.set(true);
  }

  closePalette(): void {
    this.paletteOpen.set(false);
  }

  async addTile(choice: TileChoice): Promise<void> {
    this.paletteOpen.set(false);
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
    const config = this.service.parseConfig(tile.config);
    try {
      const sources = await this.ipc.getDashboardSources(this.id());
      if (sources.length === 0) return;
      const result = await this.ipc.askVault(question, [], undefined, sources);
      await this.service.updateTile(tile.id, {
        config: {
          ...config,
          question,
          answer: result.answer,
          answeredAt: new Date().toISOString(),
          // Record WHICH sources produced this answer. The backend gates the
          // cached answer against them, so once any of those folders is sealed
          // the paraphrase stops being returned instead of outliving its source.
          answerSources: sources,
        },
      });
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
