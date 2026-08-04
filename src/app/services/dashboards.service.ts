import { Injectable, computed, inject, signal } from "@angular/core";
import { IpcService } from "../core/ipc.service";
import type {
  Dashboard,
  DashboardDetail,
  DashboardSummary,
  DashboardTint,
  TileConfig,
  TileKind,
} from "../core/models";

/**
 * Root signal store for the Dashboards section.
 *
 * `providedIn: "root"` is load-bearing, not incidental: `/dashboards` is a LIST
 * route, so its component is destroyed and recreated on every navigate-away-
 * and-back (angular-zoneless.md §8). A component-local signal would be wiped to
 * `[]` each time and the list would flash empty until the refetch resolved.
 * Living here, the last-known rows survive the remount, the template shows them
 * instantly, and the (still unconditional) reload replaces them underneath.
 *
 * The backend owns the truth: every mutation resolves, then we reload the
 * affected list into signals — we never optimistically toggle a flag the
 * backend owns.
 */
@Injectable({ providedIn: "root" })
export class DashboardsService {
  private readonly ipc = inject(IpcService);

  private readonly _boards = signal<DashboardSummary[]>([]);
  private readonly _loading = signal(false);
  private readonly _error = signal<string | null>(null);

  /** The open board (survives a remount of the board route, same rationale). */
  private readonly _board = signal<DashboardDetail | null>(null);
  private readonly _boardLoading = signal(false);

  readonly boards = this._boards.asReadonly();
  readonly loading = this._loading.asReadonly();
  readonly error = this._error.asReadonly();
  readonly board = this._board.asReadonly();
  readonly boardLoading = this._boardLoading.asReadonly();

  readonly pinned = computed(() => this._boards().filter((b) => b.pinned));
  readonly unpinned = computed(() => this._boards().filter((b) => !b.pinned));
  readonly isEmpty = computed(() => this._boards().length === 0);

  async load(): Promise<void> {
    this._loading.set(true);
    try {
      this._boards.set(await this.ipc.listDashboards());
      this._error.set(null);
    } catch (e) {
      this._error.set(this.message(e));
    } finally {
      this._loading.set(false);
    }
  }

  /**
   * The board id whose load is currently authoritative. Every `loadBoard` call
   * claims it, and a response only lands if it still holds the claim.
   */
  private wantedBoardId: string | null = null;

  /**
   * Load ONE board.
   *
   * STALE-RESULT GUARD (mandatory, not defensive): navigating board A → board B
   * while A's `getDashboard` is still in flight would otherwise let A's late
   * response overwrite B — rendering an unrelated board on B's route. The same
   * class of bug the entity-detail effect guards against.
   */
  async loadBoard(id: string): Promise<void> {
    this.wantedBoardId = id;
    if (this._board()?.id !== id) this._board.set(null);
    this._boardLoading.set(true);
    try {
      const detail = await this.ipc.getDashboard(id);
      if (this.wantedBoardId !== id) return; // a newer board won the race
      this._board.set(detail);
      this._error.set(null);
    } catch (e) {
      if (this.wantedBoardId !== id) return;
      this._error.set(this.message(e));
    } finally {
      if (this.wantedBoardId === id) this._boardLoading.set(false);
    }
  }

  /** Re-resolve the OPEN board (after a tile mutation). */
  private async reloadOpenBoard(): Promise<void> {
    const id = this._board()?.id;
    if (id) await this.loadBoard(id);
  }

  /**
   * Apply a committed span locally — the backend already accepted it, so this is
   * reconciliation, not an optimistic guess that could diverge.
   */
  private patchTileSpan(tileId: string, span: number): void {
    this._board.update((board) => {
      if (!board) return board;
      const i = board.tiles.findIndex((t) => t.id === tileId);
      if (i < 0 || board.tiles[i].span === span) return board;
      const tiles = [...board.tiles];
      tiles[i] = { ...tiles[i], span };
      return { ...board, tiles };
    });
  }

  async create(
    title: string,
    emoji?: string,
    tint?: DashboardTint,
  ): Promise<Dashboard | null> {
    try {
      const created = await this.ipc.createDashboard(title, emoji, tint);
      await this.load();
      return created;
    } catch (e) {
      this._error.set(this.message(e));
      return null;
    }
  }

  async update(
    id: string,
    patch: {
      title?: string;
      emoji?: string;
      tint?: DashboardTint;
      pinned?: boolean;
    },
  ): Promise<void> {
    try {
      await this.ipc.updateDashboard(id, patch);
      await this.load();
      if (this._board()?.id === id) await this.reloadOpenBoard();
    } catch (e) {
      this._error.set(this.message(e));
    }
  }

  async remove(id: string): Promise<void> {
    try {
      await this.ipc.deleteDashboard(id);
      if (this._board()?.id === id) this._board.set(null);
      await this.load();
    } catch (e) {
      this._error.set(this.message(e));
    }
  }

  async addTile(
    dashboardId: string,
    kind: TileKind,
    opts?: { refId?: string; title?: string; span?: number; config?: TileConfig },
  ): Promise<void> {
    try {
      await this.ipc.addDashboardTile(dashboardId, kind, {
        refId: opts?.refId,
        title: opts?.title,
        span: opts?.span,
        config: opts?.config ? JSON.stringify(opts.config) : undefined,
      });
      await this.reloadOpenBoard();
      await this.load();
    } catch (e) {
      this._error.set(this.message(e));
    }
  }

  async updateTile(
    id: string,
    patch: { title?: string; span?: number; config?: TileConfig },
  ): Promise<void> {
    try {
      await this.ipc.updateDashboardTile(id, {
        title: patch.title,
        span: patch.span,
        config: patch.config ? JSON.stringify(patch.config) : undefined,
      });
      // A SPAN-only change is pure layout: it cannot alter a single gated payload,
      // so patch it in place instead of re-resolving the board. `get_dashboard`
      // re-runs every tile's gated reader, and a Promise ledger costs a full-vault
      // note scan — so one chevron click was paying for the whole board twice over,
      // and a board with two ledgers paid for that scan twice again.
      //
      // Anything else (title, config) CAN change what resolves, so it still reloads.
      const spanOnly =
        patch.span !== undefined && patch.title === undefined && patch.config === undefined;
      if (spanOnly) this.patchTileSpan(id, patch.span!);
      else await this.reloadOpenBoard();
    } catch (e) {
      this._error.set(this.message(e));
    }
  }

  async removeTile(id: string): Promise<void> {
    try {
      await this.ipc.deleteDashboardTile(id);
      await this.reloadOpenBoard();
      await this.load();
    } catch (e) {
      this._error.set(this.message(e));
    }
  }

  async reorderTiles(dashboardId: string, tileIds: string[]): Promise<void> {
    try {
      await this.ipc.reorderDashboardTiles(dashboardId, tileIds);
      await this.reloadOpenBoard();
    } catch (e) {
      this._error.set(this.message(e));
    }
  }

  clearError(): void {
    this._error.set(null);
  }

  /** Parse a tile's persisted `config` JSON, tolerating absent/garbage blobs. */
  parseConfig(raw: string | null): TileConfig {
    if (!raw) return {};
    try {
      const parsed: unknown = JSON.parse(raw);
      return parsed && typeof parsed === "object" ? (parsed as TileConfig) : {};
    } catch {
      return {};
    }
  }

  private message(e: unknown): string {
    if (typeof e === "string") return e;
    if (e && typeof e === "object") {
      const v = Object.values(e as Record<string, unknown>)[0];
      if (typeof v === "string") return v;
    }
    return "Something went wrong.";
  }
}
