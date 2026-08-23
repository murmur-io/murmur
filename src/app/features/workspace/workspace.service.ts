import { Injectable, computed, inject, signal } from "@angular/core";

import { IpcService } from "../../core/ipc.service";
import type { ContainerNode, ItemKind, ItemPage } from "../../core/models";

/** Storage keys for the two persisted expansion sets. */
const EXPANDED_CONTAINERS_KEY = "murmur.workspace.expandedContainers";
const EXPANDED_GROUPS_KEY = "murmur.workspace.expandedGroups";

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

  private readonly _forest = signal<ContainerNode[]>([]);
  /** Projects, each with its folders and per-kind item groups. */
  readonly forest = this._forest.asReadonly();

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

  private readonly _expandedContainers = signal<ReadonlySet<string>>(
    readStoredSet(EXPANDED_CONTAINERS_KEY),
  );
  private readonly _expandedGroups = signal<ReadonlySet<string>>(
    readStoredSet(EXPANDED_GROUPS_KEY),
  );

  /** Reload the whole forest. Safe to call repeatedly; the last write wins. */
  async reload(): Promise<void> {
    this._loading.set(true);
    try {
      const forest = await this.ipc.listWorkspaceTree();
      this._forest.set(forest);
      this._error.set(null);
    } catch (error) {
      // Keep whatever is cached: a failed refresh must not blank a tree the user
      // is navigating. The message is surfaced, the rows are not thrown away.
      this._error.set(messageOf(error));
    } finally {
      this._loading.set(false);
    }
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

  // ── expansion state, persisted per container and per type group ────────────

  isContainerExpanded(id: string): boolean {
    return this._expandedContainers().has(id);
  }

  toggleContainer(id: string): void {
    this._expandedContainers.set(
      toggled(this._expandedContainers(), id, EXPANDED_CONTAINERS_KEY),
    );
  }

  /**
   * Type groups are keyed by container AND kind, so collapsing "Meetings" in one
   * project leaves it open in another — the two are unrelated facts about
   * unrelated containers.
   */
  isGroupExpanded(containerId: string, kind: ItemKind): boolean {
    return this._expandedGroups().has(groupKey(containerId, kind));
  }

  toggleGroup(containerId: string, kind: ItemKind): void {
    this._expandedGroups.set(
      toggled(this._expandedGroups(), groupKey(containerId, kind), EXPANDED_GROUPS_KEY),
    );
  }
}

function groupKey(containerId: string, kind: ItemKind): string {
  return `${containerId}:${kind}`;
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

function messageOf(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }
  if (error && typeof error === "object" && "message" in error) {
    return String((error as { message: unknown }).message);
  }
  return "Nie udało się wczytać hierarchii";
}
