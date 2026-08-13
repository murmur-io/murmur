import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  computed,
  effect,
  inject,
  signal,
  untracked,
  viewChild,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { ActivatedRoute, Router } from "@angular/router";
import { IpcService } from "../../../core/ipc.service";
import { AskHistoryPrivacyBarrierService } from "../../../core/ask-history-privacy-barrier.service";
import type {
  DashboardDetail,
  ResolvedTile,
  SourceRef,
} from "../../../core/models";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";
import { MurEmptyStateComponent } from "../../../design-system/empty-state/empty-state.component";
import { MurIconComponent } from "../../../design-system/icon/icon.component";
import { MurSpinnerComponent } from "../../../design-system/spinner/spinner.component";
import { MarkdownComponent } from "../../../shared/markdown/markdown.component";
import {
  DashboardsService,
  splitLeadingEmoji,
} from "../../../services/dashboards.service";
import { FoldersService } from "../../../services/folders.service";
import {
  TilePaletteService,
  type TileChoice,
} from "../../../services/tile-palette.service";
import { DashboardComposeComponent } from "../dashboard-compose/dashboard-compose.component";
import { DashboardReadComponent } from "../dashboard-read/dashboard-read.component";
import { projectDashboard, type DashboardLens } from "../dashboard-projection";

interface BoardTurn {
  id: number;
  role: "user" | "assistant" | "error";
  text: string;
  retry?: string;
}

const SUGGESTIONS = [
  "What needs attention?",
  "Which commitments are still open?",
  "Summarize the readable material on this board.",
];

const LENSES: readonly { id: DashboardLens; label: string }[] = [
  { id: "brief", label: "Brief" },
  { id: "overview", label: "Overview" },
  { id: "commitments", label: "Commitments" },
  { id: "sources", label: "Sources" },
  { id: "people", label: "People" },
];

@Component({
  selector: "app-dashboard-view",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    MurEmptyStateComponent,
    MurIconComponent,
    MurSpinnerComponent,
    DashboardComposeComponent,
    DashboardReadComponent,
    MarkdownComponent,
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
  private readonly errors = inject(ErrorCopyService);
  private readonly folders = inject(FoldersService);
  private readonly privacyBarrier = inject(AskHistoryPrivacyBarrierService);
  private removePrivacyInvalidator: (() => void) | null = null;

  constructor() {
    this.removePrivacyInvalidator = this.privacyBarrier.registerInvalidator(
      () => this.invalidateAndRefresh(),
    );
    void this.privacyBarrier.ensureReady();
    inject(DestroyRef).onDestroy(() => {
      this.removePrivacyInvalidator?.();
      this.removePrivacyInvalidator = null;
      if (this.palette.open()) this.palette.dismiss();
    });
  }

  private readonly params = toSignal(this.route.paramMap);
  readonly id = computed(() => this.params()?.get("id") ?? "");
  readonly board = signal<DashboardDetail | null>(null);
  readonly loading = signal(false);
  readonly boardUnavailable = signal(false);
  readonly privacyReady = this.privacyBarrier.ready;
  readonly privacyError = this.privacyBarrier.error;
  private readonly mutationError = signal<string | null>(null);
  readonly error = computed(() => this.mutationError() ?? this.service.error());
  readonly paletteOpen = this.palette.open;
  readonly mode = signal<"read" | "compose">("read");
  readonly lens = signal<DashboardLens>("brief");
  readonly lenses = LENSES;
  readonly privacyRefreshing = signal(false);

  private readonly serverTiles = computed<ResolvedTile[]>(() =>
    this.privacyRefreshing() ? [] : (this.board()?.tiles ?? []),
  );
  private readonly orderOverride = signal<string[] | null>(null);
  readonly tiles = computed<ResolvedTile[]>(() => {
    const rows = this.serverTiles();
    const order = this.orderOverride();
    if (!order) return rows;
    const byId = new Map(rows.map((tile) => [tile.id, tile]));
    const ordered = order
      .map((id) => byId.get(id))
      .filter((tile): tile is ResolvedTile => tile !== undefined);
    for (const tile of rows) {
      if (!order.includes(tile.id)) ordered.push(tile);
    }
    return ordered;
  });
  readonly isEmpty = computed(() => this.tiles().length === 0);
  readonly projection = computed(() => projectDashboard(this.tiles()));
  readonly duplicates = computed<ReadonlyMap<string, string>>(() => {
    const seen = new Map<string, string>();
    const duplicates = new Map<string, string>();
    for (const tile of this.tiles()) {
      if (
        tile.data.kind === "locked" ||
        tile.data.kind === "missing" ||
        tile.data.kind === "unconfigured"
      ) {
        continue;
      }
      const key = JSON.stringify(tile.data);
      const first = seen.get(key);
      if (first === undefined) seen.set(key, this.tileHeading(tile));
      else duplicates.set(tile.id, first);
    }
    return duplicates;
  });

  readonly renaming = signal(false);
  readonly renameDraft = signal("");
  private readonly renameField =
    viewChild<ElementRef<HTMLInputElement>>("renameField");
  private readonly _focusRename = effect(() => {
    this.renameField()?.nativeElement.select();
  });

  readonly composeBusy = signal(false);

  readonly turns = signal<BoardTurn[]>([]);
  readonly asking = signal(false);
  readonly draft = signal("");
  readonly sourceCount = computed(
    () => this.projection().readableMaterialCount,
  );
  readonly askOpen = signal(false);
  readonly suggestions = SUGGESTIONS;
  private askToken = 0;
  private readonly conversationId = signal<string | null>(null);
  private nextTurnId = 1;
  private refreshToken = 0;
  private answerRefreshToken = 0;
  readonly answerRefreshingTileId = signal<string | null>(null);
  private folderVisibilityStamp: string | null = null;

  private readonly _resetOnBoardChange = effect(() => {
    this.id();
    untracked(() => {
      this.askToken += 1;
      this.answerRefreshToken += 1;
      this.answerRefreshingTileId.set(null);
      this.conversationId.set(null);
      this.turns.set([]);
      this.draft.set("");
      this.asking.set(false);
      this.askOpen.set(false);
      this.mode.set("read");
      this.lens.set("brief");
      this.composeBusy.set(false);
    });
  });

  private readonly _load = effect(() => {
    const id = this.id();
    const privacyReady = this.privacyReady();
    const privacyError = this.privacyError();
    if (!id) return;
    untracked(() => {
      if (privacyReady) {
        void this.refresh(id);
      } else {
        // Listener setup can fail while an initial gated read is already in
        // flight. Orphan it synchronously so its late plaintext response cannot
        // become admissible after the barrier has moved to error.
        this.refreshToken += 1;
        this.board.set(null);
        this.loading.set(privacyError === null);
        this.privacyRefreshing.set(privacyError === null);
        this.boardUnavailable.set(privacyError !== null);
      }
    });
  });

  /**
   * The process-wide privacy barrier owns the monotonic lock-authority boundary.
   * The tree stamp remains a secondary refresh for exposure changes such as unlock.
   */
  private readonly _regateOnFolderChange = effect(() => {
    const stamp = this.folderStamp(this.folders.tree());
    const previousStamp = this.folderVisibilityStamp;
    this.folderVisibilityStamp = stamp;
    if (previousStamp === null || previousStamp === stamp) return;

    untracked(() => {
      this.invalidateAndRefresh();
    });
  });

  private invalidateAndRefresh(): void {
    this.askToken += 1;
    this.answerRefreshToken += 1;
    this.answerRefreshingTileId.set(null);
    this.conversationId.set(null);
    this.refreshToken += 1;
    this.turns.set([]);
    this.draft.set("");
    this.asking.set(false);
    this.askOpen.set(false);
    this.renaming.set(false);
    this.renameDraft.set("");
    this.mode.set("read");
    this.lens.set("brief");
    this.composeBusy.set(false);
    this.board.set(null);
    this.orderOverride.set(null);
    if (this.palette.open()) this.palette.dismiss();
    const id = this.id();
    if (id && this.privacyReady()) {
      void this.refresh(id);
    } else {
      this.loading.set(false);
      this.privacyRefreshing.set(false);
      this.boardUnavailable.set(this.privacyError() !== null);
    }
  }

  private folderStamp(
    nodes: readonly {
      id: string;
      locked: boolean;
      unlocked: boolean;
      children?: unknown[];
    }[],
  ): string {
    const values: string[] = [];
    const visit = (items: typeof nodes): void => {
      for (const node of items) {
        values.push(
          `${node.id}:${node.locked ? 1 : 0}:${node.unlocked ? 1 : 0}`,
        );
        visit((node.children ?? []) as typeof nodes);
      }
    };
    visit(nodes);
    return values.sort().join("|");
  }

  private async refresh(id: string): Promise<void> {
    const token = ++this.refreshToken;
    this.board.set(null);
    this.boardUnavailable.set(false);
    this.privacyRefreshing.set(true);
    this.loading.set(true);
    try {
      const detail = await this.ipc.getDashboard(id);
      if (
        token !== this.refreshToken ||
        this.id() !== id ||
        !this.privacyReady()
      ) {
        return;
      }
      if (detail === null) {
        this.boardUnavailable.set(true);
        return;
      }
      this.board.set(detail);
      this.boardUnavailable.set(false);
    } catch {
      if (
        token !== this.refreshToken ||
        this.id() !== id ||
        !this.privacyReady()
      ) {
        return;
      }
      this.board.set(null);
      this.boardUnavailable.set(true);
    } finally {
      if (
        token === this.refreshToken &&
        this.id() === id &&
        this.privacyReady()
      ) {
        this.loading.set(false);
        this.privacyRefreshing.set(false);
      }
    }
  }

  async retryLoad(): Promise<void> {
    const id = this.id();
    if (!id) return;
    this.board.set(null);
    this.boardUnavailable.set(false);
    this.loading.set(true);
    this.privacyRefreshing.set(true);
    if (await this.privacyBarrier.ensureReady()) {
      await this.refresh(id);
    } else if (this.id() === id) {
      this.loading.set(false);
      this.privacyRefreshing.set(false);
      this.boardUnavailable.set(true);
    }
  }

  selectLens(lens: DashboardLens): void {
    this.lens.set(lens);
  }

  duplicateOf(tile: ResolvedTile): string | null {
    return this.duplicates().get(tile.id) ?? null;
  }

  private tileHeading(tile: ResolvedTile): string {
    if (tile.title?.trim()) return tile.title.trim();
    const data = tile.data;
    switch (data.kind) {
      case "note":
      case "meeting":
      case "document":
        return data.title;
      case "person":
        return data.name;
      case "promises":
        return "Promises";
      case "reminders":
        return "Reminders";
      case "livingAnswer":
        return data.question || "Living answer";
      default:
        return "Derived view";
    }
  }

  enterCompose(): void {
    this.askOpen.set(false);
    this.mode.set("compose");
  }

  leaveCompose(): void {
    this.mode.set("read");
  }

  startRename(): void {
    const board = this.board();
    if (!board) return;
    this.renameDraft.set(
      board.emoji ? `${board.emoji} ${board.title}` : board.title,
    );
    this.renaming.set(true);
  }

  onRenameInput(event: Event): void {
    this.renameDraft.set((event.target as HTMLInputElement).value);
  }

  cancelRename(): void {
    this.renaming.set(false);
  }

  async commitRename(): Promise<void> {
    const typed = this.renameDraft().trim();
    const board = this.board();
    if (!typed || !board) return;
    this.renaming.set(false);
    const { emoji, title } = splitLeadingEmoji(typed);
    if (title === board.title && (emoji ?? null) === (board.emoji ?? null))
      return;
    this.resetAskContext();
    await this.service.update(board.id, { title, emoji: emoji ?? "" });
    await this.refresh(board.id);
  }

  async moveTile(tile: ResolvedTile, delta: -1 | 1): Promise<void> {
    const index = this.tiles().findIndex(
      (candidate) => candidate.id === tile.id,
    );
    await this.moveTo(tile.id, index + delta);
  }

  async reorderTile(event: {
    tileId: string;
    targetId: string;
  }): Promise<void> {
    const destination = this.tiles().findIndex(
      (tile) => tile.id === event.targetId,
    );
    await this.moveTo(event.tileId, destination);
  }

  private async moveTo(tileId: string, destination: number): Promise<void> {
    const ids = this.tiles().map((tile) => tile.id);
    const from = ids.indexOf(tileId);
    if (
      from < 0 ||
      destination < 0 ||
      destination >= ids.length ||
      from === destination
    )
      return;
    this.resetAskContext();
    ids.splice(destination, 0, ...ids.splice(from, 1));
    const previous = this.tiles().map((tile) => tile.id);
    const token = ++this.refreshToken;
    this.orderOverride.set(ids);
    this.composeBusy.set(true);
    this.mutationError.set(null);
    try {
      await this.ipc.reorderDashboardTiles(this.id(), ids);
      if (token !== this.refreshToken || !this.privacyReady()) return;
      // Keep the optimistic canonical order mounted so keyboard focus and the
      // compact list never disappear during a slow persistence round trip.
      this.board.update((board) =>
        board
          ? {
              ...board,
              tiles: ids
                .map((id) => board.tiles.find((tile) => tile.id === id))
                .filter((tile): tile is ResolvedTile => tile !== undefined),
            }
          : board,
      );
    } catch {
      if (token !== this.refreshToken || !this.privacyReady()) return;
      this.orderOverride.set(previous);
      this.mutationError.set(
        "Couldn’t save the new board order. The previous order was restored.",
      );
    } finally {
      if (token === this.refreshToken) {
        this.orderOverride.set(null);
        this.composeBusy.set(false);
      }
    }
  }

  back(): void {
    void this.router.navigate(["/dashboards"]);
  }

  async openPalette(event?: Event): Promise<void> {
    if (!this.privacyReady()) return;
    const explicit = event?.currentTarget;
    const invoker =
      explicit instanceof HTMLElement
        ? explicit
        : document.activeElement instanceof HTMLElement &&
            document.activeElement !== document.body
          ? document.activeElement
          : null;
    const choice = await this.palette.request();
    if (choice) await this.addTile(choice);
    if (invoker?.isConnected) invoker.focus();
  }

  togglePalette(event?: Event): void {
    if (this.paletteOpen()) this.palette.dismiss();
    else void this.openPalette(event);
  }

  async addTile(choice: TileChoice): Promise<void> {
    if (!this.privacyReady()) return;
    this.resetAskContext();
    await this.service.addTile(this.id(), choice.kind, {
      refId: choice.refId,
      title: choice.title,
      config: choice.config,
    });
    await this.refresh(this.id());
  }

  async removeTile(tile: ResolvedTile): Promise<void> {
    if (!this.privacyReady() || this.composeBusy()) return;
    const before = this.board();
    if (!before) return;
    this.resetAskContext();
    const token = ++this.refreshToken;
    this.composeBusy.set(true);
    this.mutationError.set(null);
    this.board.set({
      ...before,
      tiles: before.tiles.filter((candidate) => candidate.id !== tile.id),
    });
    try {
      const removed = await this.ipc.deleteDashboardTile(tile.id);
      if (!removed) throw new Error("Tile was not removed");
    } catch {
      if (
        token === this.refreshToken &&
        this.id() === before.id &&
        this.privacyReady()
      ) {
        this.board.set(before);
        this.mutationError.set(
          "Couldn’t remove this board item. It was restored.",
        );
      }
    } finally {
      if (token === this.refreshToken) this.composeBusy.set(false);
    }
  }

  openSource(source: SourceRef): void {
    switch (source.kind) {
      case "meeting":
        void this.router.navigate(["/meeting", source.id]);
        break;
      case "note":
        void this.router.navigate(["/notes", source.id]);
        break;
      default:
        void this.router.navigate(["/brain"]);
    }
  }

  async refreshLivingAnswer(answer: {
    tileId: string;
    question: string;
  }): Promise<void> {
    const board = this.board();
    const question = answer.question.trim();
    const tile = board?.tiles.find(
      (candidate) => candidate.id === answer.tileId,
    );
    if (
      !board ||
      !this.privacyReady() ||
      this.answerRefreshingTileId() !== null ||
      !question ||
      tile?.data.kind !== "livingAnswer" ||
      tile.data.withheld ||
      tile.data.question.trim() !== question
    ) {
      return;
    }

    this.resetAskContext();
    const boardId = board.id;
    const refreshWitness = this.refreshToken;
    const token = ++this.answerRefreshToken;
    const owns = (): boolean =>
      token === this.answerRefreshToken &&
      refreshWitness === this.refreshToken &&
      this.id() === boardId &&
      this.privacyReady();
    this.answerRefreshingTileId.set(tile.id);
    this.mutationError.set(null);

    try {
      const refreshed = await this.ipc.refreshDashboardAnswer(
        boardId,
        tile.id,
        question,
      );
      if (!owns()) return;
      if (
        refreshed.kind !== "livingAnswer" ||
        refreshed.withheld ||
        refreshed.question.trim() !== question ||
        !refreshed.answer?.trim()
      ) {
        throw new Error("The refreshed answer was not readable.");
      }

      const current = this.board()?.tiles.find(
        (candidate) => candidate.id === tile.id,
      );
      if (
        current?.data.kind !== "livingAnswer" ||
        current.data.withheld ||
        current.data.question.trim() !== question
      ) {
        return;
      }
      this.board.update((detail) =>
        detail?.id === boardId
          ? {
              ...detail,
              tiles: detail.tiles.map((candidate) =>
                candidate.id === tile.id
                  ? { ...candidate, data: refreshed }
                  : candidate,
              ),
            }
          : detail,
      );

      const latest = await this.ipc.getDashboard(boardId);
      if (!owns()) return;
      if (latest === null) {
        throw new Error("The refreshed board is no longer available.");
      }
      this.board.set(latest);
    } catch (error) {
      if (!owns()) return;
      this.mutationError.set(this.errors.humanize(error, "generic"));
    } finally {
      if (token === this.answerRefreshToken) {
        this.answerRefreshingTileId.set(null);
      }
    }
  }

  openAsk(): void {
    if (!this.privacyReady()) return;
    this.askOpen.set(true);
  }

  closeAsk(): void {
    this.askOpen.set(false);
  }

  onDraft(event: Event): void {
    this.draft.set((event.target as HTMLInputElement).value);
  }

  askSuggestion(text: string): void {
    this.draft.set(text);
    void this.ask();
  }

  async ask(): Promise<void> {
    if (!this.privacyReady()) return;
    const question = this.draft().trim();
    if (!question || this.asking()) return;
    const conversationId = this.conversationId() ?? undefined;
    this.draft.set("");
    this.turns.update((turns) => [
      ...turns,
      { id: this.nextTurnId++, role: "user", text: question },
    ]);
    this.asking.set(true);
    const boardId = this.id();
    const token = ++this.askToken;
    const current = () => this.askToken === token;
    const owns = () => current() && this.id() === boardId;
    try {
      const result = await this.ipc.askVaultPersisted(
        { kind: "vault" },
        question,
        conversationId,
        undefined,
        undefined,
        boardId,
      );
      if (!owns()) return;
      this.conversationId.set(result.conversationId ?? null);
      this.turns.update((turns) => [
        ...turns,
        { id: this.nextTurnId++, role: "assistant", text: result.answer },
      ]);
    } catch (error) {
      if (!owns()) return;
      this.conversationId.set(null);
      this.turns.set([
        {
          id: this.nextTurnId++,
          role: "error",
          text: this.errors.humanize(error, "generic"),
          retry: question,
        },
      ]);
    } finally {
      if (current()) this.asking.set(false);
    }
  }

  private resetAskContext(): void {
    this.askToken += 1;
    this.answerRefreshToken += 1;
    this.answerRefreshingTileId.set(null);
    this.conversationId.set(null);
    this.turns.set([]);
    this.draft.set("");
    this.asking.set(false);
  }

  retry(turn: BoardTurn): void {
    if (!turn.retry) return;
    this.turns.update((turns) =>
      turns.filter((candidate) => candidate.id !== turn.id),
    );
    this.draft.set(turn.retry);
    void this.ask();
  }
}
