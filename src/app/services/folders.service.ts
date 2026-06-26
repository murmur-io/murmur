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
   */
  async create(name: string, parentId: string | null = null): Promise<Folder> {
    this._error.set(null);
    try {
      const folder = await this.ipc.createFolder(name, parentId);
      await this.load();
      return folder;
    } catch (e) {
      this._error.set(String(e));
      throw e;
    }
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
