import { Injectable, inject, signal } from "@angular/core";
import { IpcService } from "../core/ipc.service";
import type {
  NoteFolder,
  NoteSummary,
  OrganizeMove,
  OrganizePlan,
} from "../core/models";

/**
 * Signal store for the Notes section — the note list + the note-kind folder tree,
 * mirroring {@link FoldersService}'s shape (the backend is the source of truth;
 * every mutating op resolves, then we reload the affected list into signals — we
 * never optimistically toggle a flag the backend owns).
 *
 * Notes are `documents(kind='note')` behind the scenes; note-folders are
 * `folders(kind='note')`. Lock/seal is folder-id-keyed and reuses the existing
 * folder lock commands — this store owns only the note + note-folder LISTS, not
 * the lock lifecycle (that stays in {@link FoldersService}).
 */
@Injectable({ providedIn: "root" })
export class NotesService {
  private readonly ipc = inject(IpcService);

  private readonly _notes = signal<NoteSummary[]>([]);
  private readonly _noteFolders = signal<NoteFolder[]>([]);
  private readonly _loading = signal(false);
  private readonly _foldersLoading = signal(false);
  private readonly _error = signal<string | null>(null);

  /** The note list, as last returned by the backend (gated — masked rows included). */
  readonly notes = this._notes.asReadonly();
  /** The note-kind folder list (`kind='note'` only). */
  readonly noteFolders = this._noteFolders.asReadonly();
  /** True while the note list is being (re)loaded. */
  readonly loading = this._loading.asReadonly();
  /** True while the note-folder list is being (re)loaded. */
  readonly foldersLoading = this._foldersLoading.asReadonly();
  /** Non-null when the last op failed (cleared at the start of the next op). */
  readonly error = this._error.asReadonly();

  /**
   * (Re)load the note list for `folderId` (null ⇒ all visible notes). Safe to call
   * repeatedly; an error is captured into `error` (the stale list self-heals on
   * the next load) rather than thrown, so a transient failure never blanks the UI.
   */
  async loadNotes(folderId: string | null = null): Promise<void> {
    this._loading.set(true);
    this._error.set(null);
    try {
      this._notes.set(await this.ipc.listNotes(folderId));
    } catch (e) {
      this._error.set(String(e));
    } finally {
      this._loading.set(false);
    }
  }

  /** (Re)load the note-kind folder list. */
  async loadFolders(): Promise<void> {
    this._foldersLoading.set(true);
    this._error.set(null);
    try {
      this._noteFolders.set(await this.ipc.listNoteFolders());
    } catch (e) {
      this._error.set(String(e));
    } finally {
      this._foldersLoading.set(false);
    }
  }

  /**
   * Create an empty note in `folderId` (null ⇒ the default "Notes" folder) and
   * return its id so the caller can navigate to `/notes/:id`. The CREATE is the
   * operation whose failure means "couldn't create" — it rejects; the follow-on
   * list refresh is best-effort (a refresh error is captured in `error`, never
   * turns a real create into a thrown failure — mirrors {@link FoldersService}).
   */
  async create(folderId: string | null, title = "Untitled"): Promise<string> {
    this._error.set(null);
    let id: string;
    try {
      id = await this.ipc.createNote(folderId, title);
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
    await this.loadNotes(folderId);
    return id;
  }

  /**
   * Persist a note's title + FULL markdown (re-index + re-export backend-side).
   * The UPDATE is the only failure that rejects; the list refresh is swallowed.
   */
  async update(id: string, title: string, markdown: string): Promise<void> {
    this._error.set(null);
    try {
      await this.ipc.updateNoteDoc(id, title, markdown);
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
    await this.loadNotes();
  }

  /** Move a note into `folderId` (re-exports under the new path). Reloads the list. */
  async move(id: string, folderId: string): Promise<void> {
    this._error.set(null);
    try {
      await this.ipc.moveNoteDoc(id, folderId);
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
    await this.loadNotes();
  }

  /**
   * Permanently delete a note. The DELETE is the only failure that rejects; we
   * prune the row from the local list at once (no full reload needed).
   */
  async remove(id: string): Promise<void> {
    this._error.set(null);
    try {
      await this.ipc.deleteNote(id);
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
    this._notes.update((list) => list.filter((n) => n.id !== id));
  }

  /**
   * Create a note-kind folder under `parentId` (null ⇒ the Notes root). Returns the
   * new {@link NoteFolder}. Same split-outcome rule as {@link create}: the CREATE
   * is the only failure that rejects; the follow-on folder refresh is swallowed.
   */
  async createFolder(
    name: string,
    parentId: string | null = null,
  ): Promise<NoteFolder> {
    this._error.set(null);
    let folder: NoteFolder;
    try {
      folder = await this.ipc.createNoteFolder(name, parentId);
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
    await this.loadFolders();
    return folder;
  }

  /**
   * Rename a note-kind folder. The RENAME is the only failure that rejects; the
   * follow-on folder refresh is swallowed (split-outcome, like {@link createFolder}).
   */
  async renameFolder(id: string, name: string): Promise<void> {
    this._error.set(null);
    try {
      await this.ipc.renameNoteFolder(id, name);
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
    await this.loadFolders();
  }

  /**
   * Delete a note-kind folder (its notes move to the default note-folder). The
   * DELETE is the only failure that rejects; the folder + note lists are then
   * refreshed (both may change — notes reparent).
   */
  async deleteFolder(id: string): Promise<void> {
    this._error.set(null);
    try {
      await this.ipc.deleteNoteFolder(id);
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
    await this.loadFolders();
  }

  /**
   * Auto-organize STEP 1 — ask the backend to propose per-note folder moves for
   * `folderId` (null ⇒ all notes). Returns the plan for a confirm-before-apply
   * review; nothing moves yet. Rejects so the caller can surface + reset its
   * loading state.
   */
  async planOrganize(folderId: string | null): Promise<OrganizePlan> {
    this._error.set(null);
    try {
      return await this.ipc.planOrganizeNotes(folderId);
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
  }

  /**
   * Auto-organize STEP 2 — apply the (user-reviewed) selected moves: the backend
   * creates needed folders + moves the notes. The APPLY is the only failure that
   * rejects; both the note + folder lists are then refreshed.
   */
  async applyOrganize(moves: OrganizeMove[]): Promise<void> {
    this._error.set(null);
    try {
      await this.ipc.applyOrganizePlan({ moves });
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
    await Promise.allSettled([this.loadFolders(), this.loadNotes()]);
  }
}
