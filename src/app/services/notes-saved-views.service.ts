import { Injectable, computed, inject, signal } from "@angular/core";
import { IpcService } from "../core/ipc.service";
import {
  parseViewConfig,
  type SavedView,
  type ViewConfig,
} from "../core/models";

/** localStorage key holding the last active NOTES saved-view id (null = List default). */
const ACTIVE_VIEW_KEY = "murmur.savedViews.notes.activeId";

/**
 * Root-persisted signal store for the NOTES-list SAVED VIEWS roster + the
 * currently-active view id — the Notes-surface twin of {@link
 * import("./saved-views.service").SavedViewsService} (ported 2026-07-14 so
 * Notes has the same view switcher Meetings does). The backend is the source of
 * truth (`scope="notes"`); every mutating op resolves, then we reload the
 * roster into the signal.
 *
 * `providedIn: 'root'` on PURPOSE (angular-zoneless §8, list stale-while-
 * revalidate): the Notes list route is destroyed+recreated on every
 * navigate-away-and-back, so a component-local `signal<SavedView[]>([])` would
 * be wiped to empty on every return — the view switcher would flash empty until
 * the refetch lands. A root instance outlives the component, so the SAME roster
 * (and last-known active view) survives the remount.
 *
 * SECURITY NOTE: a saved view is PURELY presentational — it re-presents the
 * already-gated note/org rows the pane already holds; it stores no note content
 * and never unmasks a locked row (that stays the notes-view-engine + template's
 * job). It carries no action-summaries (unlike the meetings twin — notes have no
 * open/done action counts).
 */
@Injectable({ providedIn: "root" })
export class NotesSavedViewsService {
  private readonly ipc = inject(IpcService);

  private readonly _views = signal<SavedView[]>([]);
  private readonly _loading = signal(false);
  private readonly _error = signal<string | null>(null);
  private readonly _activeViewId = signal<string | null>(readActiveId());

  /** The saved-view roster for the notes scope, in `sortOrder`. */
  readonly views = this._views.asReadonly();
  /** True while the roster is being (re)loaded. */
  readonly loading = this._loading.asReadonly();
  /** Non-null when the last op failed (cleared at the start of the next op). */
  readonly error = this._error.asReadonly();
  /** The active saved-view id (null ⇒ the plain List default). Persisted across restarts. */
  readonly activeViewId = this._activeViewId.asReadonly();

  /**
   * The active {@link SavedView}, or null when no view is selected OR the
   * persisted id no longer exists — in which case the notes list falls back to
   * its List default, never a dangling reference.
   */
  readonly activeView = computed<SavedView | null>(() => {
    const id = this._activeViewId();
    if (id === null) {
      return null;
    }
    return this._views().find((v) => v.id === id) ?? null;
  });

  /**
   * (Re)load the notes saved-view roster. Safe to call repeatedly; a failure is
   * captured into `error` (the stale roster self-heals on the next load) rather
   * than thrown, so a transient failure never blanks the switcher.
   */
  async load(): Promise<void> {
    this._loading.set(true);
    this._error.set(null);
    try {
      const views = await this.ipc.listSavedViews("notes");
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

  /** The parsed {@link ViewConfig} of a saved view (never throws — a corrupt config degrades). */
  configOf(view: SavedView): ViewConfig {
    return parseViewConfig(view.config);
  }

  /**
   * Create a new saved Table view (the caller passes the seed config). The
   * CREATE is the only failure that rejects; the roster refresh is best-effort.
   * Returns the persisted row and makes it active.
   */
  async create(name: string, config: string): Promise<SavedView> {
    this._error.set(null);
    const draft: SavedView = {
      id: "", // empty id ⇒ "create"; the backend assigns id + timestamps.
      scope: "notes",
      name,
      layout: "table",
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
   * Persist a full saved view (rename or a config edit — the caller passes the
   * complete, updated row). The UPSERT is the only failure that rejects.
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

  /**
   * Permanently delete a saved view. If the deleted view was active we fall
   * back to the List default, then refresh the roster.
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
