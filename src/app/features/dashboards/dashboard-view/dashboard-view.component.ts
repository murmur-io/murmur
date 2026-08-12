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
  ChatTurn,
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
import { DashboardTileComponent } from "../dashboard-tile/dashboard-tile.component";
import { DashboardReadComponent } from "../dashboard-read/dashboard-read.component";
import {
  projectDashboard,
  type DashboardLens,
} from "../dashboard-projection";

interface BoardTurn {
  id: number;
  role: "user" | "assistant" | "error";
  text: string;
  retry?: string;
}

const HISTORY_TURNS = 12;
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
    DashboardTileComponent,
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
    this.removePrivacyInvalidator = this.privacyBarrier.registerInvalidator(() =>
      this.invalidateAndRefresh(),
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
  readonly error = this.service.error;
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
  private readonly renameField = viewChild<ElementRef<HTMLInputElement>>("renameField");
  private readonly _focusRename = effect(() => {
    this.renameField()?.nativeElement.select();
  });

  readonly draggingId = signal<string | null>(null);
  readonly dropTargetId = signal<string | null>(null);

  readonly turns = signal<BoardTurn[]>([]);
  readonly asking = signal(false);
  readonly draft = signal("");
  readonly sourceCount = signal(0);
  readonly askOpen = signal(false);
  readonly suggestions = SUGGESTIONS;
  private askToken = 0;
  private nextTurnId = 1;
  private refreshToken = 0;
  private folderVisibilityStamp: string | null = null;

  private readonly _resetOnBoardChange = effect(() => {
    this.id();
    untracked(() => {
      this.askToken += 1;
      this.turns.set([]);
      this.draft.set("");
      this.asking.set(false);
      this.askOpen.set(false);
      this.mode.set("read");
      this.lens.set("brief");
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
        this.sourceCount.set(0);
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
    this.refreshToken += 1;
    this.turns.set([]);
    this.draft.set("");
    this.asking.set(false);
    this.askOpen.set(false);
    this.renaming.set(false);
    this.renameDraft.set("");
    this.mode.set("read");
    this.lens.set("brief");
    this.sourceCount.set(0);
    this.board.set(null);
    this.orderOverride.set(null);
    this.onDragEnd();
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
        values.push(`${node.id}:${node.locked ? 1 : 0}:${node.unlocked ? 1 : 0}`);
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
      const [detail, sources] = await Promise.all([
        this.ipc.getDashboard(id),
        this.ipc.getDashboardSources(id),
      ]);
      if (
        token !== this.refreshToken ||
        this.id() !== id ||
        !this.privacyReady()
      ) {
        return;
      }
      if (detail === null) {
        this.sourceCount.set(0);
        this.boardUnavailable.set(true);
        return;
      }
      this.board.set(detail);
      this.sourceCount.set(sources.length);
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
      this.sourceCount.set(0);
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
    this.draggingId.set(null);
    this.dropTargetId.set(null);
    this.mode.set("read");
  }

  startRename(): void {
    const board = this.board();
    if (!board) return;
    this.renameDraft.set(board.emoji ? `${board.emoji} ${board.title}` : board.title);
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
    if (title === board.title && (emoji ?? null) === (board.emoji ?? null)) return;
    await this.service.update(board.id, { title, emoji: emoji ?? "" });
    await this.refresh(board.id);
  }

  onDragStart(tile: ResolvedTile, event: DragEvent): void {
    if (this.mode() !== "compose") return;
    this.draggingId.set(tile.id);
    event.dataTransfer?.setData("text/plain", tile.id);
    if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
  }

  onDragOver(tile: ResolvedTile, event: DragEvent): void {
    if (this.mode() !== "compose" || !this.draggingId()) return;
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
    this.dropTargetId.set(tile.id);
  }

  onDragLeave(tile: ResolvedTile): void {
    if (this.dropTargetId() === tile.id) this.dropTargetId.set(null);
  }

  onDragEnd(): void {
    this.draggingId.set(null);
    this.dropTargetId.set(null);
  }

  async onDrop(target: ResolvedTile, event: DragEvent): Promise<void> {
    event.preventDefault();
    const sourceId = this.draggingId();
    this.onDragEnd();
    if (!sourceId || sourceId === target.id) return;
    await this.moveTo(sourceId, this.tiles().findIndex((tile) => tile.id === target.id));
  }

  async moveTile(tile: ResolvedTile, delta: -1 | 1): Promise<void> {
    const index = this.tiles().findIndex((candidate) => candidate.id === tile.id);
    await this.moveTo(tile.id, index + delta);
  }

  private async moveTo(tileId: string, destination: number): Promise<void> {
    const ids = this.tiles().map((tile) => tile.id);
    const from = ids.indexOf(tileId);
    if (from < 0 || destination < 0 || destination >= ids.length || from === destination) return;
    ids.splice(destination, 0, ...ids.splice(from, 1));
    this.orderOverride.set(ids);
    try {
      await this.service.reorderTiles(this.id(), ids);
      await this.refresh(this.id());
    } finally {
      this.orderOverride.set(null);
    }
  }

  back(): void {
    void this.router.navigate(["/dashboards"]);
  }

  async openPalette(event?: Event): Promise<void> {
    if (!this.privacyReady()) return;
    const explicit = event?.currentTarget;
    const invoker = explicit instanceof HTMLElement
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
    await this.service.addTile(this.id(), choice.kind, {
      refId: choice.refId,
      title: choice.title,
      config: choice.config,
    });
    await this.refresh(this.id());
  }

  async removeTile(tile: ResolvedTile): Promise<void> {
    if (!this.privacyReady()) return;
    await this.service.removeTile(tile.id);
    await this.refresh(this.id());
  }

  async widen(tile: ResolvedTile): Promise<void> {
    if (!this.privacyReady()) return;
    await this.service.updateTile(tile.id, { span: Math.min(12, tile.span + 1) });
    await this.refresh(this.id());
  }

  async narrow(tile: ResolvedTile): Promise<void> {
    if (!this.privacyReady()) return;
    await this.service.updateTile(tile.id, { span: Math.max(3, tile.span - 1) });
    await this.refresh(this.id());
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
      const sources = await this.ipc.getDashboardSources(boardId);
      if (!owns()) return;
      this.sourceCount.set(sources.length);
      if (sources.length === 0 && this.tiles().length === 0) {
        this.turns.update((turns) => [
          ...turns,
          {
            id: this.nextTurnId++,
            role: "assistant",
            text: "This board has no readable sources yet — add a note, recording or document, or unlock a sealed source, and ask again.",
          },
        ]);
        return;
      }
      const result = await this.ipc.askVault(
        question,
        this.askHistory(),
        undefined,
        sources,
        undefined,
        boardId,
      );
      if (!owns()) return;
      this.turns.update((turns) => [
        ...turns,
        { id: this.nextTurnId++, role: "assistant", text: result.answer },
      ]);
    } catch (error) {
      if (!owns()) return;
      this.turns.update((turns) => [
        ...turns,
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

  private askHistory(): ChatTurn[] {
    return this.turns()
      .filter((turn) => turn.role !== "error")
      .slice(-HISTORY_TURNS)
      .map((turn) => ({
        role: turn.role as "user" | "assistant",
        content: turn.text,
      }));
  }

  retry(turn: BoardTurn): void {
    if (!turn.retry) return;
    this.turns.update((turns) => turns.filter((candidate) => candidate.id !== turn.id));
    this.draft.set(turn.retry);
    void this.ask();
  }

}
