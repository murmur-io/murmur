import { Injectable, computed, inject, signal } from "@angular/core";
import { IpcService } from "../core/ipc.service";
import type { Folder, FolderNode } from "../core/models";

/**
 * A folder's privacy exposure, derived from its lock flags:
 *  - `"open"`    — not sealed; markdown lives in the vault as plaintext.
 *  - `"locked"`  — sealed (encrypted) on disk; markdown blanked, not visible.
 *  - `"session"` — sealed on disk BUT session-unlocked (decrypted into markdown
 *                  for this session only; re-seals on relock / screen-share).
 */
export type FolderExposure = "open" | "locked" | "session";

/**
 * Signal store for the folder tree + the per-folder lock lifecycle (Stage C).
 *
 * The backend is the source of truth for lock state: every mutating op resolves,
 * then we `load()` the fresh tree so `locked` / `unlocked` always mirror disk +
 * session reality (we never optimistically toggle a flag the backend owns). A
 * session-"unlocked" folder is simply a `FolderNode` with `locked && unlocked`.
 */
@Injectable({ providedIn: "root" })
export class FoldersService {
  private readonly ipc = inject(IpcService);

  private readonly _tree = signal<FolderNode[]>([]);
  private readonly _loading = signal(false);
  private readonly _error = signal<string | null>(null);

  /** The folder forest (roots → children), as last returned by the backend. */
  readonly tree = this._tree.asReadonly();
  /** True while the tree is being (re)loaded from the backend. */
  readonly loading = this._loading.asReadonly();
  /** Non-null when the last op failed (cleared at the start of the next op). */
  readonly error = this._error.asReadonly();

  /** Flattened view of every node in the forest, for counts/lookups. */
  private readonly allNodes = computed(() => this.flatten(this._tree()));

  /**
   * How many sealed folders are session-unlocked right now (`locked && unlocked`)
   * — the count of folders currently exposing plaintext to this session, e.g.
   * for a "Lock all" affordance or a privacy indicator.
   */
  readonly unlockedCount = computed(
    () => this.allNodes().filter((n) => n.locked && n.unlocked).length,
  );

  /** Whether the forest has any sealed folders at all. */
  readonly hasLockedFolders = computed(() =>
    this.allNodes().some((n) => n.locked),
  );

  /** Derive a folder's three-state privacy exposure from its lock flags. */
  exposureOf(node: FolderNode): FolderExposure {
    if (!node.locked) {
      return "open";
    }
    return node.unlocked ? "session" : "locked";
  }

  /** (Re)load the folder tree from the backend. Safe to call repeatedly. */
  async load(): Promise<void> {
    this._loading.set(true);
    this._error.set(null);
    try {
      this._tree.set(await this.ipc.listFolders());
    } catch (e) {
      this._error.set(String(e));
    } finally {
      this._loading.set(false);
    }
  }

  /**
   * Create a folder under `parentId` (null = vault root). The IPC returns the new
   * `Folder` row (no counts/children), so we reload the tree to fold it in with
   * the rest of the forest rather than splice a partial node. The created
   * `Folder` is returned so the caller can select/highlight it once it appears.
   *
   * BUGFIX (false "couldn't create" toast): the create + the follow-on tree
   * refresh are SEPARATE outcomes. Once `createFolder` resolves the folder IS in
   * the DB — a subsequent `load()` refresh failure must NEVER turn a real success
   * into a thrown failure (the caller then shows a danger toast while the folder
   * actually exists and appears after the next refresh). So `createFolder`'s
   * error is the only one that rejects this method; a `load()` error is swallowed
   * here (it already records `error` internally and a stale tree self-heals on the
   * next `load()`). The created `Folder` is still returned so the caller can
   * select + highlight it.
   */
  async create(name: string, parentId: string | null = null): Promise<Folder> {
    this._error.set(null);
    // The CREATE is the operation whose failure means "couldn't create" — let it reject.
    let folder: Folder;
    try {
      folder = await this.ipc.createFolder(name, parentId);
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
    // The folder now exists. The refresh is best-effort: a refresh error is logged into `error`
    // (via load()) but must NOT be re-thrown — a successful create can never surface as a failure.
    await this.load();
    return folder;
  }

  /**
   * Rename a folder (display name + on-disk vault subdir + governed paths). A LOCKED folder rename is
   * metadata-only (never touches sealed content). Reloads the tree on success. Same split-outcome
   * rule as {@link create}: the RENAME is the only failure that rejects; a follow-on refresh failure
   * is swallowed (the rename already happened).
   */
  async rename(folderId: string, newName: string): Promise<Folder> {
    this._error.set(null);
    let folder: Folder;
    try {
      folder = await this.ipc.renameFolder(folderId, newName);
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
    await this.load();
    return folder;
  }

  /**
   * Delete a folder. Its notes move to the vault root; the folder row + empty subdir are removed.
   * Rejects when the folder is sealed-and-not-session-unlocked, or still has subfolders — the caller
   * surfaces that as a friendly message. Reloads the tree on success (split-outcome: the DELETE is
   * the only failure that rejects; a follow-on refresh failure is swallowed).
   */
  async delete(folderId: string): Promise<void> {
    this._error.set(null);
    try {
      await this.ipc.deleteFolder(folderId);
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
    await this.load();
  }

  /** Move a note into `folderId` (null = vault root); refreshes per-folder counts. */
  async moveNote(meetingId: string, folderId: string | null): Promise<void> {
    this._error.set(null);
    try {
      await this.ipc.moveNote(meetingId, folderId);
      await this.load();
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
  }

  /** Seal a folder (encrypt its notes, blank markdown, remove the vault .md). */
  async lock(folderId: string): Promise<void> {
    this._error.set(null);
    try {
      await this.ipc.lockFolder(folderId);
      await this.load();
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
  }

  /** Session-unlock a sealed folder (decrypt into markdown for this session). */
  async unlock(folderId: string): Promise<void> {
    this._error.set(null);
    try {
      await this.ipc.unlockFolder(folderId);
      await this.load();
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
  }

  /** Re-seal a single session-unlocked folder (stays locked on disk). */
  async relock(folderId: string): Promise<void> {
    this._error.set(null);
    try {
      await this.ipc.relockFolder(folderId);
      await this.load();
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
  }

  /** Re-seal ALL session-unlocked folders + zeroize the cached key. */
  async relockAll(): Promise<void> {
    this._error.set(null);
    try {
      await this.ipc.relockAll();
      await this.load();
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
  }

  /** Permanently remove a folder's lock (decrypt + re-export plaintext to vault). */
  async removeLock(folderId: string): Promise<void> {
    this._error.set(null);
    try {
      await this.ipc.removeLock(folderId);
      await this.load();
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
  }

  /** Depth-first flatten of the forest into a single node list. */
  private flatten(nodes: FolderNode[]): FolderNode[] {
    const out: FolderNode[] = [];
    const walk = (list: FolderNode[]): void => {
      for (const node of list) {
        out.push(node);
        // Defensive: tolerate a node that omits `children` (older backend).
        if (node.children?.length) {
          walk(node.children);
        }
      }
    };
    walk(nodes);
    return out;
  }
}
