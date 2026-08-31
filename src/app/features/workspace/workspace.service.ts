import {
  DestroyRef,
  Injectable,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";

import { AskHistoryPrivacyBarrierService } from "../../core/ask-history-privacy-barrier.service";
import { IpcService } from "../../core/ipc.service";
import type { ContainerNode, ItemKind, ItemPage } from "../../core/models";
import type { DraggableKind } from "../folders/note-drag.service";

/** Storage key for persisted container expansion. */
const EXPANDED_CONTAINERS_KEY = "murmur.workspace.expandedContainers";
const UNFILED_EXPANDED_KEY = "murmur.workspace.unfiledExpanded";

/**
 * The workspace container forest, owned at the ROOT so it outlives the sidebar.
 *
 * The sidebar is destroyed and recreated on every navigation, so a
 * component-local signal would empty the tree on each return and the user would
 * watch it repopulate — the 2026-07-12 incident, which is why
 * `.claude/rules/angular-zoneless.md` §8 requires list-backing state to live in a
 * `providedIn: "root"` service. The reload below is still unconditional; what
 * changes is that the previous rows survive it.
 */
@Injectable({ providedIn: "root" })
export class WorkspaceService {
  private readonly ipc = inject(IpcService);
  private readonly privacyBarrier = inject(AskHistoryPrivacyBarrierService);
  private readonly destroyRef = inject(DestroyRef);

  private readonly _forest = signal<ContainerNode[]>([]);
  /** Projects, each with its folders and per-kind item groups. */
  readonly forest = this._forest.asReadonly();

  private readonly _unfiledRecordings = signal<ItemPage>({
    kind: "meeting",
    items: [],
    total: 0,
  });
  /** Newest recordings that do not belong to any lockable container. */
  readonly unfiledRecordings = this._unfiledRecordings.asReadonly();

  private readonly _unfiledExpanded = signal(
    readStoredBoolean(UNFILED_EXPANDED_KEY, true),
  );
  /** Persisted disclosure state for the recording inbox. */
  readonly unfiledExpanded = this._unfiledExpanded.asReadonly();

  private readonly _loading = signal(false);
  readonly loading = this._loading.asReadonly();

  private readonly _error = signal<string | null>(null);
  readonly error = this._error.asReadonly();

  /**
   * True only before the FIRST load resolves. The template gates its spinner on
   * this AND emptiness, so a return visit shows cached rows instantly while the
   * reload replaces them underneath.
   */
  readonly forestEmpty = computed(() => this._forest().length === 0);

  /** Nothing the Workspaces tree can currently render. */
  readonly workspaceEmpty = computed(
    () => this._forest().length === 0 && this._unfiledRecordings().total === 0,
  );

  private readonly _expandedContainers = signal<ReadonlySet<string>>(
    readStoredSet(EXPANDED_CONTAINERS_KEY),
  );

  /**
   * Invalidates an older gated read when lock authority changes underneath it.
   * Without this token, a pre-lock response can land after the synchronous scrub
   * and restore titles that the renderer is no longer allowed to retain.
   */
  private loadGeneration = 0;

  private readonly _loaded = signal(false);
  /**
   * Whether a forest read has actually ANSWERED at least once.
   *
   * Emptiness is NOT a substitute: an empty forest is a legitimate result, so a
   * caller guarding on `workspaceEmpty()` cannot tell "nobody has read it yet"
   * from "there is nothing there" and re-reads every time. With the sidebar now
   * mounting the tree twice AND the recording-destination picker asking as well,
   * that turned one boot read into several.
   *
   * Set only on the success path, deliberately: a privacy-refused read and a
   * failed one both leave the cache as-is, so they must stay retryable rather
   * than pinning this flag true with nothing loaded.
   */
  readonly loaded = this._loaded.asReadonly();

  constructor() {
    const unregister = this.privacyBarrier.registerInvalidator(() => {
      this.scrubAndReload();
    });
    this.destroyRef.onDestroy(unregister);
    effect(() => {
      if (this.ipc.workspaceMutationRevision() > 0) {
        void this.reload();
      }
    });
  }

  private ensureLoadedInFlight: Promise<void> | null = null;

  /**
   * Load the forest unless it is already loaded, coalescing concurrent callers.
   *
   * The sidebar mounts `app-workspace-tree` TWICE — once for the user's own
   * Workspaces, once for what an org shared with them — and both constructors
   * run before the first `reload()` resolves. An emptiness check alone therefore
   * let each instance issue its own `list_workspace_tree`, doubling the boot
   * read. Oracle: `recording-placement.spec.ts`'s "an empty destination forest
   * loads once, stays calm, and retries only on request".
   *
   * A DELIBERATE refresh still goes through `reload()` directly, so it is never
   * folded into an in-flight load and always re-reads.
   */
  ensureLoaded(): Promise<void> {
    if (this._loaded()) {
      return Promise.resolve();
    }
    this.ensureLoadedInFlight ??= this.reload().finally(() => {
      this.ensureLoadedInFlight = null;
    });
    return this.ensureLoadedInFlight;
  }

  /** Reload the whole forest. Safe to call repeatedly; the last write wins. */
  async reload(): Promise<void> {
    const generation = ++this.loadGeneration;
    this._loading.set(true);
    try {
      // Tauri events are not replayed. Refuse content-bearing hierarchy reads
      // until the shared privacy listeners have acknowledged registration.
      const privacyReady = await this.privacyBarrier.ensureReady();
      if (generation !== this.loadGeneration) {
        return;
      }
      if (!privacyReady) {
        this._forest.set([]);
        this._unfiledRecordings.set({ kind: "meeting", items: [], total: 0 });
        this._error.set("Workspace is unavailable securely right now");
        return;
      }
      const [forest, unfiledRecordings] = await Promise.all([
        this.ipc.listWorkspaceTree(),
        this.ipc.listContainerItems(null, "meeting", 0, 8),
      ]);
      if (generation !== this.loadGeneration) {
        return;
      }
      this._forest.set(forest);
      this._unfiledRecordings.set(unfiledRecordings);
      this._error.set(null);
      this._loaded.set(true);
    } catch (error) {
      if (generation !== this.loadGeneration) {
        return;
      }
      // Keep whatever is cached: a failed refresh must not blank a tree the user
      // is navigating. A privacy transition clears that cache synchronously in
      // `scrubAndReload`, so this fallback can retain only still-authorized rows.
      this._error.set(messageOf(error));
    } finally {
      if (generation === this.loadGeneration) {
        this._loading.set(false);
      }
    }
  }

  /**
   * Lock/move/delete authority changed. Drop every cached title synchronously,
   * invalidate any pre-transition response, then repair from the canonical gated
   * reader. If that refetch fails, the empty privacy-safe state remains visible.
   */
  private scrubAndReload(): void {
    ++this.loadGeneration;
    this._forest.set([]);
    this._unfiledRecordings.set({ kind: "meeting", items: [], total: 0 });
    this._loaded.set(false);
    void this.reload();
  }

  /** One container's own metadata (never its contents); `null` when unknown. */
  getContainer(id: string): Promise<ContainerNode | null> {
    return this.ipc.getContainer(id);
  }

  /** One page of a container's items of one kind — what "see all" pages through. */
  listItems(
    containerId: string | null,
    kind: ItemKind,
    offset: number,
    limit: number,
  ): Promise<ItemPage> {
    return this.ipc.listContainerItems(containerId, kind, offset, limit);
  }

  /**
   * Create a note inside a container and return its id.
   *
   * Notes, folders and dashboards can be created into a container. Tasks cannot, and that is
   * not an oversight: a task belongs to an ORGANIZATION, so creating one needs an org and an
   * assignee that a container cannot supply. A task reaches a container by being FILED into one
   * afterwards ({@link fileTask}), which is the same shape a meeting uses and for the same
   * reason — the thing exists first, the placement is a second, separate decision.
   */
  async createNote(containerId: string, title: string): Promise<string> {
    const id = await this.ipc.createNote(containerId, title);
    await this.reload();
    return id;
  }

  /** Create a peer top-level Workspace, then refresh the cached forest. */
  async createSpace(name: string): Promise<string> {
    const space = await this.ipc.createSpace(name);
    await this.reload();
    return space.id;
  }

  /**
   * Create the namespace-correct child folder, then refresh the tree.
   *
   * Meeting and authored-note containers share one visual hierarchy but retain distinct backend
   * creation commands. Passing the canonical parent metadata keeps the menu honest; the backend
   * still validates it independently.
   */
  async createFolder(
    container: Pick<ContainerNode, "id" | "kind">,
    name: string,
  ): Promise<string> {
    const folder =
      container.kind === "note"
        ? await this.ipc.createNoteFolder(name, container.id)
        : await this.ipc.createFolder(name, container.id);
    await this.reload();
    return folder.id;
  }

  /** Create a dashboard inside a container and return its id. */
  async createDashboard(containerId: string, title: string): Promise<string> {
    const board = await this.ipc.createDashboardIn(title, containerId);
    await this.reload();
    return board.id;
  }

  /** File an existing task into a container (or unfile it with `null`). */
  async fileTask(taskId: string, containerId: string | null): Promise<void> {
    await this.ipc.setTaskContainer(taskId, containerId);
    await this.reload();
  }

  /**
   * Move an item into a container.
   *
   * Dispatches to the EXISTING per-kind movers rather than reimplementing the
   * transition. Those carry what matters and must not be bypassed: an open target
   * moves the vault `.md`; a target that is sealed and session-unlocked seals the
   * item on arrival so plaintext never lands inside a sealed container; a target
   * that is sealed and not unlocked is refused.
   */
  async moveItem(kind: DraggableKind, id: string, containerId: string): Promise<void> {
    switch (kind) {
      case "meeting":
        await this.ipc.moveNote(id, containerId);
        break;
      case "dashboard":
        await this.ipc.moveDashboardToContainer(id, containerId);
        break;
      case "task":
        await this.ipc.setTaskContainer(id, containerId);
        break;
      default:
        await this.ipc.moveNoteDoc(id, containerId);
    }
    await this.reload();
  }

  // ── expansion state, persisted per container and per type group ────────────

  isContainerExpanded(id: string): boolean {
    return this._expandedContainers().has(id);
  }

  toggleContainer(id: string): void {
    this._expandedContainers.set(
      toggled(this._expandedContainers(), id, EXPANDED_CONTAINERS_KEY),
    );
  }

  toggleUnfiled(): void {
    const next = !this._unfiledExpanded();
    this._unfiledExpanded.set(next);
    writeStoredBoolean(UNFILED_EXPANDED_KEY, next);
  }

}

function toggled(
  current: ReadonlySet<string>,
  key: string,
  storageKey: string,
): ReadonlySet<string> {
  const next = new Set(current);
  if (!next.delete(key)) {
    next.add(key);
  }
  writeStoredSet(storageKey, next);
  return next;
}

/**
 * `localStorage` throws outright in some contexts (a private window, site data
 * blocked, a thumbnail capture), so every read and write is guarded and the
 * absence of a stored value is a normal starting state, not an error.
 */
function readStoredSet(key: string): ReadonlySet<string> {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) {
      return new Set();
    }
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed)
      ? new Set(parsed.filter((v): v is string => typeof v === "string"))
      : new Set();
  } catch {
    return new Set();
  }
}

function writeStoredSet(key: string, value: ReadonlySet<string>): void {
  try {
    localStorage.setItem(key, JSON.stringify([...value]));
  } catch {
    // A remembered expansion is a convenience; losing it must never break the tree.
  }
}

function readStoredBoolean(key: string, fallback: boolean): boolean {
  try {
    const raw = localStorage.getItem(key);
    return raw === null ? fallback : raw === "true";
  } catch {
    return fallback;
  }
}

function writeStoredBoolean(key: string, value: boolean): void {
  try {
    localStorage.setItem(key, String(value));
  } catch {
    // A remembered disclosure is a convenience; losing it must never break the tree.
  }
}

function messageOf(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }
  if (error && typeof error === "object" && "message" in error) {
    return String((error as { message: unknown }).message);
  }
  return "Could not load the workspace";
}
