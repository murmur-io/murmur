import { Injectable, computed, inject, signal } from "@angular/core";
import { IpcService } from "../core/ipc.service";
import {
  parseViewConfig,
  type MeetingActionSummary,
  type SavedView,
  type ViewConfig,
} from "../core/models";

/** localStorage key holding the last active meetings saved-view id (null = List default). */
const ACTIVE_VIEW_KEY = "murmur.savedViews.meetings.activeId";

/**
 * Root-persisted signal store for Feature B — the meetings-list SAVED VIEWS
 * roster + the currently-active view id. Mirrors {@link NotesService}'s shape:
 * the backend is the source of truth; every mutating op resolves, then we
 * reload the roster into the signal (we never optimistically toggle a flag the
 * backend owns).
 *
 * `providedIn: 'root'` on PURPOSE (angular-zoneless §8, list stale-while-
 * revalidate): the meetings list route is destroyed+recreated on every
 * navigate-away-and-back, so a component-local `signal<SavedView[]>([])` would
 * be wiped to empty on every return — the view switcher would flash empty
 * until the refetch lands. A root instance outlives the component, so the
 * SAME roster (and last-known active view) survives the remount; the
 * component just re-injects it.
 *
 * SCOPE: this feature only ships the `"meetings"` scope. The service is
 * hard-typed to it (a notes-list / calendar scope is deferred), so callers
 * can't accidentally load a scope the backend doesn't yet serve here.
 *
 * SECURITY NOTE: a saved view is PURELY presentational — it re-presents the
 * already-gated `Meeting[]` the backend returned; it holds no meeting content
 * and never unmasks a locked row (that stays the ViewEngine + template's job).
 */
@Injectable({ providedIn: "root" })
export class SavedViewsService {
  private readonly ipc = inject(IpcService);

  private readonly _views = signal<SavedView[]>([]);
  private readonly _loading = signal(false);
  private readonly _error = signal<string | null>(null);
  private readonly _activeViewId = signal<string | null>(readActiveId());
  private readonly _actionSummaries = signal<MeetingActionSummary[]>([]);

  /** The saved-view roster for the meetings scope, in `sortOrder` (last-known survives a remount). */
  readonly views = this._views.asReadonly();
  /** True while the roster is being (re)loaded. */
  readonly loading = this._loading.asReadonly();
  /** Non-null when the last op failed (cleared at the start of the next op). */
  readonly error = this._error.asReadonly();
  /** The active saved-view id (null ⇒ the plain List default). Persisted across restarts. */
  readonly activeViewId = this._activeViewId.asReadonly();
  /**
   * Per-meeting open/done action counts feeding the Table/Board views. Also
   * root-persisted (survives the list route's remount, like the roster) — a
   * sealed-and-not-session-unlocked meeting is omitted server-side, so a locked
   * row simply carries no summary (no leak).
   */
  readonly actionSummaries = this._actionSummaries.asReadonly();

  /**
   * The active {@link SavedView}, or null when no view is selected OR the
   * persisted id no longer exists (e.g. deleted on another window) — in which
   * case the meetings list falls back to its List default, never a dangling
   * reference.
   */
  readonly activeView = computed<SavedView | null>(() => {
    const id = this._activeViewId();
    if (id === null) {
      return null;
    }
    return this._views().find((v) => v.id === id) ?? null;
  });

  /**
   * (Re)load the meetings saved-view roster. Safe to call repeatedly; a failure
   * is captured into `error` (the stale roster self-heals on the next load)
   * rather than thrown, so a transient failure never blanks the switcher.
   */
  async load(): Promise<void> {
    this._loading.set(true);
    this._error.set(null);
    try {
      const views = await this.ipc.listSavedViews("meetings");
      this._views.set([...views].sort((a, b) => a.sortOrder - b.sortOrder));
      // If the persisted active id vanished server-side, drop back to List.
      const active = this._activeViewId();
      if (active !== null && !views.some((v) => v.id === active)) {
        this.setActiveView(null);
      }
    } catch (e) {
      this._error.set(String(e));
    } finally {
      this._loading.set(false);
    }
  }

  /**
   * (Re)load the per-meeting action summaries. Safe to call repeatedly; a
   * failure leaves the last-known counts standing (never blanks a view). The
   * counts are stored raw — the {@link import("./view-engine").ViewEngine}
   * merges them per row.
   */
  async loadActionSummaries(): Promise<void> {
    try {
      this._actionSummaries.set(await this.ipc.listMeetingActionSummaries());
    } catch {
      // Stale counts self-heal on the next successful load.
    }
  }

  /**
   * The parsed {@link ViewConfig} of a saved view — a thin, safe wrapper over
   * {@link parseViewConfig} (never throws; a corrupt config degrades to the
   * default). Kept here so both the switcher and the list read config the same
   * way.
   */
  configOf(view: SavedView): ViewConfig {
    return parseViewConfig(view.config);
  }

  /**
   * Create a new saved view. The layout + config are the caller's (a fresh
   * view seeds a default config client-side). The CREATE is the only failure
   * that rejects; the follow-on roster refresh is best-effort (mirrors
   * {@link NotesService.create}). Returns the persisted row and makes it active.
   */
  async create(
    name: string,
    layout: "table" | "board",
    config: string,
  ): Promise<SavedView> {
    this._error.set(null);
    const draft: SavedView = {
      // The backend assigns the real id/timestamps; empty id ⇒ "create".
      id: "",
      scope: "meetings",
      name,
      layout,
      config,
      sortOrder: this._views().length,
      createdAt: "",
      updatedAt: "",
    };
    let saved: SavedView;
    try {
      saved = await this.ipc.upsertSavedView(draft);
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
    await this.load();
    this.setActiveView(saved.id);
    return saved;
  }

  /**
   * Persist a full saved view (rename, layout swap, or a config edit — the
   * caller passes the complete, updated row). The UPSERT is the only failure
   * that rejects; the roster refresh is swallowed. Returns the persisted row.
   */
  async save(view: SavedView): Promise<SavedView> {
    this._error.set(null);
    let saved: SavedView;
    try {
      saved = await this.ipc.upsertSavedView(view);
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
    await this.load();
    return saved;
  }

  /** Rename a saved view (thin convenience over {@link save}). */
  async rename(id: string, name: string): Promise<void> {
    const view = this._views().find((v) => v.id === id);
    if (!view) {
      return;
    }
    await this.save({ ...view, name });
  }

  /**
   * Permanently delete a saved view. The DELETE is the only failure that
   * rejects; the roster is then refreshed. If the deleted view was active we
   * fall back to the List default.
   */
  async delete(id: string): Promise<void> {
    this._error.set(null);
    try {
      await this.ipc.deleteSavedView(id);
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
    if (this._activeViewId() === id) {
      this.setActiveView(null);
    }
    await this.load();
  }

  /**
   * Persist a new left-to-right ordering of the saved views. Optimistically
   * reorders the local roster so the switcher updates at once, then confirms
   * server-side and reloads (the backend is truth).
   */
  async reorder(orderedIds: string[]): Promise<void> {
    this._error.set(null);
    const byId = new Map(this._views().map((v) => [v.id, v]));
    const optimistic = orderedIds
      .map((id) => byId.get(id))
      .filter((v): v is SavedView => v !== undefined);
    this._views.set(optimistic);
    try {
      await this.ipc.reorderSavedViews("meetings", orderedIds);
    } catch (e) {
      this._error.set(String(e));
    }
    await this.load();
  }

  /**
   * Select the active saved view (null ⇒ the plain List default). Persisted to
   * localStorage so returning to the app restores the last view.
   */
  setActiveView(id: string | null): void {
    this._activeViewId.set(id);
    try {
      if (id === null) {
        localStorage.removeItem(ACTIVE_VIEW_KEY);
      } else {
        localStorage.setItem(ACTIVE_VIEW_KEY, id);
      }
    } catch {
      // localStorage unavailable (private mode / disabled) — the in-memory
      // signal still drives the current session; persistence is best-effort.
    }
  }
}

/** Read the persisted active-view id (best-effort; null on any storage failure). */
function readActiveId(): string | null {
  try {
    return localStorage.getItem(ACTIVE_VIEW_KEY);
  } catch {
    return null;
  }
}
