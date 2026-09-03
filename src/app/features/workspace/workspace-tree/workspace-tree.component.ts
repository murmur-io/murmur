import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  inject,
  input,
  signal,
} from "@angular/core";
import { Router } from "@angular/router";

import { MurRowMenuComponent } from "../../../design-system/row-menu/row-menu.component";
import { MurIconComponent } from "../../../design-system/icon/icon.component";
import { TeleportToBodyDirective } from "../../../design-system/teleport-to-body.directive";
import { FolderDropDirective } from "../../folders/folder-drop.directive";
import {
  NoteDragService,
  type DraggableKind,
} from "../../folders/note-drag.service";
import {
  MurTreeRowComponent,
  type TreeRowIcon,
} from "../../../design-system/tree-row/tree-row.component";
import type {
  ContainerNode,
  ItemKind,
  ItemRow,
  OrganizeFailure,
  OrganizeMove,
  OrganizePlan,
  SharedContainerNode,
  SharedItemRow,
} from "../../../core/models";
import { IpcService } from "../../../core/ipc.service";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";
import { AskHistoryPrivacyBarrierService } from "../../../core/ask-history-privacy-barrier.service";
import {
  type OrganizeAttemptReceipt,
  OrganizeSheetComponent,
  type OrganizeViewPlan,
} from "../../notes/organize-sheet/organize-sheet.component";
import { ToastService } from "../../../services/toast.service";
import { FolderLockFlowService } from "../../../services/folder-lock-flow.service";
import { FoldersService } from "../../../services/folders.service";
import { NotesService } from "../../../services/notes.service";
import { SharedWorkspaceService } from "../../../services/shared-workspace.service";
import { WorkspaceService } from "../workspace.service";
import {
  workspaceDestinations,
  type WorkspaceDestination,
} from "../workspace-destination";
import { WorkspaceMoveSheetComponent } from "../workspace-move-sheet/workspace-move-sheet.component";
import {
  ContainerShareSheetComponent,
  type ContainerShareTarget,
} from "../container-share-sheet/container-share-sheet.component";
import {
  WorkspaceManageSheetComponent,
  type WorkspaceManageMode,
} from "../workspace-manage-sheet/workspace-manage-sheet.component";
import { containerNoun } from "../../../core/hierarchy-vocabulary";

/** A flattened tree line, so one `@for` renders the whole forest. */
export interface TreeLine {
  /** Stable identity for `track` — unique across every line kind. */
  key: string;
  depth: number;
  /**
   * The LOCAL container this line belongs to. Absent on a shared line: content
   * another member published has no local container, and inventing a
   * placeholder one would put a row the folder gate does not govern behind a
   * type every reader takes as governed.
   */
  container?: ContainerNode;
  /** Present only on an item line. */
  item?: ItemRow;
  /** Present only on the shared continuation line. */
  seeAll?: boolean;
  /** Full visible item count across every kind in this container. */
  total?: number;
  /** Present only on a RECEIVED container row (a shared Workspace, folder, or the
   * virtual Shared Brains Workspace). */
  shared?: SharedContainerNode;
  /** Present only on a RECEIVED item row. */
  sharedItem?: SharedItemRow;
}

const MAX_VISIBLE_ITEMS = 8;

/** Persisted disclosure state for received containers, keyed like the local one. */
const EXPANDED_SHARED_KEY = "murmur.workspace.expandedShared";

/** Persisted disclosure state for the unfiled-notes inbox. */
const UNFILED_NOTES_EXPANDED_KEY = "murmur.workspace.unfiledNotesExpanded";

/**
 * Where activating an item navigates. These MUST name real entries in
 * `app.routes.ts`: an unmatched path hits the router'''s catch-all and redirects to
 * /record, so a wrong one here does not fail — it silently opens the recorder.
 */
const KIND_ROUTE: Record<ItemKind, string> = {
  meeting: "/meeting",
  note: "/notes",
  task: "/tasks",
  dashboard: "/dashboards",
};

/**
 * The workspace tree: Projects at the top, each holding Folders, and both
 * holding one time-ordered stream of Notes, Meetings, Tasks and Dashboards.
 *
 * Replaces the two per-type sidebar trees (`app-meetings-sidebar-tree` and
 * `app-notes-sidebar-tree`), which rendered one namespace each and could not
 * express a container holding both.
 *
 * The forest is FLATTENED into lines here rather than rendered by a recursive
 * component pair. Recursion would work, but a self-referential standalone pair
 * needs `forwardRef` in both directions or the first `@for` throws on an
 * undefined component definition (trap T2, which cost this repo the
 * "view breaks after adding the first folder" bug). A flat list has no cycle to
 * get wrong, and the depth is already in the data.
 */
@Component({
  selector: "app-workspace-tree",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    ContainerShareSheetComponent,
    FolderDropDirective,
    MurIconComponent,
    MurRowMenuComponent,
    MurTreeRowComponent,
    OrganizeSheetComponent,
    TeleportToBodyDirective,
    WorkspaceManageSheetComponent,
    WorkspaceMoveSheetComponent,
  ],
  templateUrl: "./workspace-tree.component.html",
  styleUrl: "./workspace-tree.component.scss",
})
export class WorkspaceTreeComponent {
  private readonly router = inject(Router);
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);
  private readonly privacyBarrier = inject(AskHistoryPrivacyBarrierService);
  private readonly toast = inject(ToastService);
  private readonly errorCopy = inject(ErrorCopyService);
  protected readonly workspace = inject(WorkspaceService);
  private readonly drag = inject(NoteDragService);
  private readonly folders = inject(FoldersService);
  private readonly lockFlow = inject(FolderLockFlowService);
  private readonly notes = inject(NotesService);
  protected readonly sharedWorkspace = inject(SharedWorkspaceService);

  /** Current route path, used to select a container or leaf row. */
  readonly currentPath = input("");

  /**
   * Which half of the forest this instance renders. The sidebar mounts the tree
   * TWICE — once for the user's own Workspaces, once for what an org shared with
   * them — so the two never interleave and a colleague's structure cannot be
   * mistaken for the user's own.
   *
   * Shared content the user has PRIVATELY FILED under a local container is not
   * affected: `pushContainer` still emits it in place, because they put it
   * there. Only unplaced shared roots move to the shared section.
   */
  readonly scope = input<"own" | "shared">("own");

  /**
   * Load the forest when this tree first appears.
   *
   * The section header's toggle also reloads, but it cannot be the only trigger:
   * the section is EXPANDED by default, so on a fresh profile the tree renders
   * without anyone having toggled anything and would sit on "No projects yet"
   * forever — indistinguishable from a workspace that really has none. This
   * component only exists while the section is open, so construction is exactly
   * "the tree became visible".
   *
   * Guarded on emptiness so a return visit renders the cached forest with no
   * refetch of its own; the header's toggle owns the deliberate refresh.
   */
  constructor() {
    const unregisterPrivacy = this.privacyBarrier.registerInvalidator(() =>
      this.scrubOrganizeReview(),
    );
    this.destroyRef.onDestroy(() => {
      unregisterPrivacy();
      this.scrubOrganizeReview();
    });
    void this.workspace.ensureLoaded();
    void this.sharedWorkspace.ensureLoaded();
  }

  /**
   * Notes that live in the reserved note root — the ones the user never filed.
   *
   * Found anywhere in the forest rather than at a fixed position: the root is
   * created inside the workspace project (`ensure_notes_root`), so its depth is
   * an implementation detail of that migration, not something to hard-code.
   */
  protected readonly unfiledNotes = computed<{
    items: ItemRow[];
    total: number;
  }>(() => {
    const find = (nodes: readonly ContainerNode[]): ContainerNode | null => {
      for (const node of nodes) {
        if (node.isRoot) {
          return node;
        }
        const inChild = find(node.folders);
        if (inChild) {
          return inChild;
        }
      }
      return null;
    };
    const root = find(this.workspace.forest());
    if (!root) {
      return { items: [], total: 0 };
    }
    const items = root.groups
      .flatMap((group) => group.items)
      .sort((left, right) => right.sortAt - left.sortAt)
      .slice(0, MAX_VISIBLE_ITEMS);
    const total = root.groups.reduce((sum, group) => sum + group.total, 0);
    return { items, total };
  });

  private readonly _unfiledNotesExpanded = signal(
    readStoredBoolean(UNFILED_NOTES_EXPANDED_KEY, true),
  );
  protected readonly unfiledNotesExpanded =
    this._unfiledNotesExpanded.asReadonly();

  protected toggleUnfiledNotes(): void {
    const next = !this._unfiledNotesExpanded();
    this._unfiledNotesExpanded.set(next);
    try {
      localStorage.setItem(UNFILED_NOTES_EXPANDED_KEY, JSON.stringify(next));
    } catch {
      /* a private window or blocked site data: the tree still works */
    }
  }

  protected viewAllNotesLabel(total: number): string {
    return `View all (${total})`;
  }

  protected openAllNotes(): void {
    void this.router.navigate(["/notes"]);
  }

  /**
   * Scope-aware, exactly like {@link error}.
   *
   * This component renders BOTH halves of the tree — the user's own workspace and the shared one —
   * and `loading` used to read `WorkspaceService.loading` in both. `SharedWorkspaceService.loading`
   * was never referenced anywhere in the component, so while a shared fetch was still in flight the
   * shared section fell through to its empty state and told the user "Nothing shared with you yet"
   * about content that was still arriving. Wrong content, not just a missing spinner — and the
   * spinner half of the same 2026-07-12 "reload flash" contract the template comment above
   * `sectionEmpty() && loading()` already spells out for cached rows.
   */
  protected readonly loading = computed(() =>
    this.isOwnScope() ? this.workspace.loading() : this.sharedWorkspace.loading(),
  );
  /**
   * The message this half of the forest should show, if any.
   *
   * Was always `workspace.error`, even on the SHARED instance — so a shared-workspace read that
   * failed rendered as "Nothing shared with you yet". A user whose relay was unreachable was told
   * their team had shared nothing. The own-workspace half is unchanged.
   */
  protected readonly error = computed(() =>
    this.isOwnScope()
      ? this.workspace.error()
      : this.sharedWorkspace.loadFailed()
        ? "Couldn't read what's shared with you. Showing the last known state."
        : null,
  );
  protected readonly workspaceEmpty = this.workspace.workspaceEmpty;
  protected readonly unfiledRecordings = this.workspace.unfiledRecordings;
  protected readonly unfiledExpanded = this.workspace.unfiledExpanded;

  /**
   * The whole forest as flat lines.
   *
   * A sealed-and-not-session-unlocked container contributes its own row and
   * nothing else — the backend sends no groups for it, not even totals, so
   * there is deliberately nothing to count or expand. Rendering it collapsed
   * with a lock glyph is the honest presentation: we know it exists, we do not
   * know what is inside.
   */
  protected readonly lines = computed<TreeLine[]>(() => {
    const out: TreeLine[] = [];
    if (this.scope() === "own") {
      for (const project of this.workspace.forest()) {
        this.pushContainer(out, project, 0);
      }
      return out;
    }
    // Received content now has its own section rather than trailing the user's
    // own Workspaces. Anything privately filed under a local container was
    // already emitted by `pushContainer` in the "own" pass, so it is skipped
    // here — otherwise "Keep in my Workspace…" would render it twice, or (when
    // this pass is the only one that emits it) leave it in Shared and make the
    // action look like it did nothing.
    for (const space of this.unplacedSharedRoots()) {
      this.pushShared(out, space, 0);
    }
    const brains = this.sharedWorkspace.sharedBrains();
    if (brains && (brains.folders.length > 0 || brains.items.length > 0)) {
      this.pushShared(out, brains, 0);
    }
    return out;
  });

  /**
   * Received Workspaces the user has NOT filed anywhere of their own — those
   * render at the top of the Shared section. A placed one is emitted under its
   * host container in the "own" pass instead.
   */
  private readonly unplacedSharedRoots = computed(() =>
    this.sharedWorkspace.spaces().filter((node) => !node.localParentId),
  );

  /**
   * Received nodes this user privately filed under a local container, indexed by
   * that container.
   *
   * Walks the WHOLE received forest, not just its roots: the "Keep in my
   * Workspace…" action is offered on every received container, including a
   * nested one, and a placement the merge could not find would be an affordance
   * that silently does nothing.
   */
  private readonly sharedByLocalParent = computed(() => {
    const map = new Map<string, SharedContainerNode[]>();
    const walk = (node: SharedContainerNode): void => {
      if (node.localParentId) {
        const bucket = map.get(node.localParentId) ?? [];
        bucket.push(node);
        map.set(node.localParentId, bucket);
      }
      node.folders.forEach(walk);
    };
    this.sharedWorkspace.spaces().forEach(walk);
    this.sharedWorkspace.sharedBrains()?.folders.forEach(walk);
    return map;
  });

  /**
   * Container ids that render under a LOCAL host instead of where their owner
   * filed them. A node listed here is skipped by `pushShared` at its original
   * position, so it appears exactly once.
   */
  private readonly placedSharedIds = computed(() => {
    const ids = new Set<string>();
    for (const bucket of this.sharedByLocalParent().values()) {
      for (const node of bucket) {
        if (node.containerId) {
          ids.add(node.containerId);
        }
      }
    }
    return ids;
  });

  /** True while this section has nothing of its own to render. */
  protected readonly sectionEmpty = computed(() =>
    this.scope() === "own"
      ? this.workspace.workspaceEmpty()
      : this.lines().length === 0,
  );

  protected readonly isOwnScope = computed(() => this.scope() === "own");

  private readonly _expandedShared = signal<ReadonlySet<string>>(
    readStoredSharedSet(),
  );

  protected sharedKey(node: SharedContainerNode): string {
    return `${node.orgId}:${node.containerId ?? "shared-brains"}`;
  }

  protected isSharedExpanded(node: SharedContainerNode): boolean {
    return this._expandedShared().has(this.sharedKey(node));
  }

  protected toggleShared(node: SharedContainerNode): void {
    const key = this.sharedKey(node);
    const next = new Set(this._expandedShared());
    if (!next.delete(key)) {
      next.add(key);
    }
    this._expandedShared.set(next);
    try {
      localStorage.setItem(EXPANDED_SHARED_KEY, JSON.stringify([...next]));
    } catch {
      /* a private window or blocked site data: the tree still works */
    }
  }

  protected sharedExpandable(node: SharedContainerNode): boolean {
    return node.folders.length > 0 || node.items.length > 0;
  }

  /** Received containers are read-only structure: no rename, delete or create. */
  protected sharedIcon(node: SharedContainerNode): TreeRowIcon {
    return node.level === "folder" ? "folder" : "space";
  }

  protected sharedAccessLabel(node: SharedContainerNode): string {
    return node.access === "edit" ? "Can edit" : "View only";
  }

  /**
   * The sentence behind the shared glyph — one dim mark, the words on hover and
   * for a screen reader. A pill here would take the row's spare width and
   * truncate the name, which is the lesson the unlocked mark already records.
   */
  protected sharedMark(line: TreeLine): string | null {
    if (line.shared) {
      const node = line.shared;
      if (node.level === "virtual") {
        return null;
      }
      return `From ${node.orgName} · ${node.authorHint} · ${this.sharedAccessLabel(node)}`;
    }
    if (line.sharedItem) {
      const item = line.sharedItem;
      const access = item.access === "edit" ? "Can edit" : "View only";
      return `From ${item.orgName} · ${item.authorHint} · ${access}`;
    }
    // An ITEM row the user published on its own. Anything inside a shared
    // container is deliberately unmarked here — that container's row already
    // says it, and repeating the glyph on every child turns a quiet signal into
    // noise. The backend read excludes container-owned rows for the same reason.
    if (line.item) {
      const target = this.sharedWorkspace
        .shareByItem()
        .get(`${line.item.kind}:${line.item.id}`);
      if (!target) {
        return null;
      }
      const access = target.access === "edit" ? "Can edit" : "View only";
      return `Shared to ${target.orgName} · ${access}`;
    }
    const container = line.container;
    if (!container) {
      return null;
    }
    const share = this.sharedWorkspace.shareByFolder().get(container.id);
    if (!share) {
      return null;
    }
    const access = share.access === "edit" ? "Can edit" : "View only";
    return `Shared to ${share.orgName} · ${access}`;
  }

  /** True when THIS user publishes this local container. */
  protected isShared(container: ContainerNode): boolean {
    return this.sharedWorkspace.shareByFolder().has(container.id);
  }

  private pushShared(
    out: TreeLine[],
    node: SharedContainerNode,
    depth: number,
  ): void {
    out.push({ key: `s:${this.sharedKey(node)}`, depth, shared: node });
    if (!this.isSharedExpanded(node)) {
      return;
    }
    for (const child of node.folders) {
      // A child the user has filed somewhere of their own renders THERE, not
      // here, or it would appear twice under two different parents.
      if (child.containerId && this.placedSharedIds().has(child.containerId)) {
        continue;
      }
      this.pushShared(out, child, depth + 1);
    }
    for (const item of node.items) {
      out.push({
        key: `si:${item.itemId}`,
        depth: depth + 1,
        sharedItem: item,
      });
    }
  }

  private pushContainer(
    out: TreeLine[],
    container: ContainerNode,
    depth: number,
  ): void {
    if (container.isRoot) {
      // The reserved note root is the "Notes" SECTION, not a folder — the
      // migration that introduced `is_root` says so, and every management
      // affordance is already disabled on it (rename, delete, lock, share). The
      // old notes tree hid it; the 2026-08-22 hierarchy rebuild renders every
      // container from `list_containers`, whose predicate filters on kind and
      // path but not `is_root`, so it came back as a folder-shaped row nobody
      // can do anything with. Its notes render as an inbox instead — symmetric
      // with unfiled recordings.
      //
      // Its child folders are still hoisted to this depth: a container the user
      // created must never become unreachable because its parent stopped being
      // drawn.
      for (const child of container.folders) {
        this.pushContainer(out, child, depth);
      }
      return;
    }
    out.push({ key: `c:${container.id}`, depth, container });
    if (this.isSealed(container) || !this.isContainerExpanded(container)) {
      return;
    }
    // Anything the user privately filed here. Their arrangement, their device —
    // the owner and every other member see nothing of it.
    for (const placed of this.sharedByLocalParent().get(container.id) ?? []) {
      this.pushShared(out, placed, depth + 1);
    }
    const allItems = container.groups
      .flatMap((group) => group.items)
      .sort((left, right) => right.sortAt - left.sortAt);
    const newestItems = allItems.slice(0, MAX_VISIBLE_ITEMS);
    const selectedItem = allItems.find((item) => this.isItemSelected(item));
    const selectedIsNewest = selectedItem
      ? newestItems.some(
          (item) =>
            item.kind === selectedItem.kind && item.id === selectedItem.id,
        )
      : false;
    const items =
      selectedItem && !selectedIsNewest
        ? [
            ...allItems
              .filter(
                (item) =>
                  item.kind !== selectedItem.kind ||
                  item.id !== selectedItem.id,
              )
              .slice(0, MAX_VISIBLE_ITEMS - 1),
            selectedItem,
          ].sort((left, right) => right.sortAt - left.sortAt)
        : newestItems;
    for (const item of items) {
      out.push({
        key: `i:${item.kind}:${item.id}`,
        depth: depth + 1,
        container,
        item,
      });
    }
    const total = container.groups.reduce((sum, group) => sum + group.total, 0);
    if (total > items.length) {
      out.push({
        key: `s:${container.id}`,
        depth: depth + 1,
        container,
        seeAll: true,
        total,
      });
    }
    for (const child of container.folders) {
      this.pushContainer(out, child, depth + 1);
    }
  }

  /**
   * Every kind gets its own glyph. One tree now renders containers AND the items
   * inside them, so a meeting and a note sharing the folder icon would make the
   * list unreadable — the glyph is what tells you what a row IS, before you read
   * it.
   */
  protected kindIcon(kind: ItemKind): TreeRowIcon {
    return kind;
  }

  protected itemKindLabel(kind: ItemKind): string {
    switch (kind) {
      case "meeting":
        return "Recording";
      case "note":
        return "Note";
      case "task":
        return "Task";
      case "dashboard":
        return "Dashboard";
    }
  }

  /** A sealed container reads as its lock; an open one as what it is. */
  protected containerIcon(container: ContainerNode): TreeRowIcon {
    if (container.locked && !container.unlocked) {
      return "locked";
    }
    return container.level === "project" ? "space" : "folder";
  }

  protected itemTitle(item: ItemRow): string {
    const title = item.title?.trim();
    return title ? title : "Untitled";
  }

  protected viewAllLabel(total: number): string {
    return `View all (${total})`;
  }

  protected viewAllRecordingsLabel(total: number): string {
    return `View all recordings (${total})`;
  }

  protected toggleUnfiled(): void {
    this.workspace.toggleUnfiled();
  }

  /** A container with no groups and no folders has nothing to disclose. */
  protected containerExpandable(container: ContainerNode): boolean {
    return (
      !this.isSealed(container) &&
      (container.groups.length > 0 ||
        container.folders.length > 0 ||
        // Received content the user has privately filed here counts as content
        // for the purpose of the caret. Without this, filing a shared Workspace
        // into an EMPTY local one would hide it: the host has nothing of its
        // own, so it would render with no way to expand and reveal what was
        // just put inside it.
        (this.sharedByLocalParent().get(container.id)?.length ?? 0) > 0)
    );
  }

  protected isContainerExpanded(container: ContainerNode): boolean {
    if (this.isSealed(container)) {
      return false;
    }
    return (
      this.workspace.isContainerExpanded(container.id) ||
      this.containerContainsCurrentSelection(container)
    );
  }

  protected isSealed(container: ContainerNode): boolean {
    return container.locked && !container.unlocked;
  }

  private containerContainsCurrentSelection(container: ContainerNode): boolean {
    if (
      container.groups.some((group) =>
        group.items.some((item) => this.isItemSelected(item)),
      )
    ) {
      return true;
    }
    return container.folders.some(
      (folder) =>
        this.isContainerSelected(folder) ||
        this.containerContainsCurrentSelection(folder),
    );
  }

  protected toggleContainer(container: ContainerNode): void {
    this.workspace.toggleContainer(container.id);
  }

  protected isContainerSelected(container: Pick<ContainerNode, "id">): boolean {
    return this.currentPath() === `/container/${container.id}`;
  }

  protected isItemSelected(item: ItemRow): boolean {
    return this.currentPath() === `${KIND_ROUTE[item.kind]}/${item.id}`;
  }

  /**
   * Open a container's own view — and select it on the two per-type lists as well.
   *
   * Only the removed note tree ever called `selectFolder`, so without this the Notes and
   * Meetings lists lost the ability to be filtered to a folder at all: the filter survived,
   * with nothing left able to set it. Selecting here keeps those surfaces coherent with the
   * hierarchy instead of quietly stranding a feature.
   */
  protected openContainer(container: ContainerNode): void {
    void this.notes.selectFolder(container.id);
    this.folders.selectFolder(container.id);
    void this.router.navigate(["/container", container.id]);
  }

  protected openItem(item: ItemRow): void {
    void this.router.navigate([KIND_ROUTE[item.kind], item.id]);
  }

  // ── received content ───────────────────────────────────────────────────────

  protected isSharedSelected(node: SharedContainerNode): boolean {
    if (!node.containerId) {
      return this.currentPath() === "/shared-brains";
    }
    return this.currentPath() === `/shared/${node.orgId}/${node.containerId}`;
  }

  protected isSharedItemSelected(item: SharedItemRow): boolean {
    return this.currentPath() === `/org-item/${item.itemId}`;
  }

  /**
   * Open a received container. The virtual Shared Brains Workspace has no container
   * of its own — it is a view over everything loose — so it opens the list route
   * with its per-org filter.
   */
  protected openShared(node: SharedContainerNode): void {
    if (!node.containerId) {
      void this.router.navigate(["/shared-brains"]);
      return;
    }
    void this.router.navigate(["/shared", node.orgId, node.containerId]);
  }

  /** A received item opens read-only in the org viewer, never the local editor. */
  protected openSharedItem(item: SharedItemRow): void {
    void this.router.navigate(["/org-item", item.itemId]);
  }

  /**
   * A container can be offered to an Org when it is not sealed and is not the
   * reserved Notes root.
   *
   * Sealed is refused by the backend too — this only avoids offering an action
   * that always errors.
   */
  protected canShare(container: ContainerNode): boolean {
    return !container.isRoot && !(container.locked && !container.unlocked);
  }

  protected openShareSheet(container: ContainerNode): void {
    this.shareRequest.set({
      id: container.id,
      name: container.name,
      level: container.level,
    });
  }

  protected closeShareSheet(): void {
    this.shareRequest.set(null);
  }

  protected async onContainerShared(): Promise<void> {
    this.shareRequest.set(null);
    await this.sharedWorkspace.load();
    this.toast.success("Shared to your organization");
  }

  /** The container whose share sheet is open, if any. */
  protected readonly shareRequest = signal<ContainerShareTarget | null>(null);

  // ── private arrangement of received content ────────────────────────────────

  /**
   * The received node the user is filing somewhere of their own, if any.
   *
   * Reuses the ordinary move sheet, deliberately: to the user this IS a move —
   * "put that shared Workspace in my Clients Workspace". What differs is invisible to
   * them and load-bearing underneath: nothing is published, the owner sees
   * nothing, and the content keeps updating from the org feed.
   */
  protected readonly placeRequest = signal<SharedContainerNode | null>(null);
  protected readonly placeBusy = signal(false);
  protected readonly placeError = signal<string | null>(null);

  /** Every local container this user could file a received node under. */
  protected placeTargets(): WorkspaceDestination[] {
    return workspaceDestinations(this.workspace.forest());
  }

  protected openPlace(node: SharedContainerNode): void {
    this.placeError.set(null);
    this.placeRequest.set(node);
  }

  protected closePlace(): void {
    this.placeRequest.set(null);
    this.placeBusy.set(false);
    this.placeError.set(null);
  }

  protected async placeInto(destination: WorkspaceDestination): Promise<void> {
    const node = this.placeRequest();
    if (!node || !node.containerId) {
      return;
    }
    this.placeBusy.set(true);
    this.placeError.set(null);
    try {
      await this.sharedWorkspace.place(
        node.orgId,
        "container",
        node.containerId,
        destination.container.id,
        0,
      );
      this.closePlace();
      this.toast.success(`Filed under ${destination.label}`);
    } catch (e) {
      this.placeError.set(this.errorCopy.humanize(e));
    } finally {
      this.placeBusy.set(false);
    }
  }

  /** Put a received node back wherever its owner filed it. */
  protected async resetPlacement(node: SharedContainerNode): Promise<void> {
    if (!node.containerId) {
      return;
    }
    try {
      await this.sharedWorkspace.unplace(node.orgId, "container", node.containerId);
      this.toast.success("Moved back to Shared");
    } catch (e) {
      this.toast.danger(this.errorCopy.humanize(e));
    }
  }

  /**
   * A container that is sealed and not unlocked for this session cannot take a new
   * child: there is no key to seal it with, so the backend refuses. Hiding the
   * affordance is better than offering one that always errors.
   */
  protected canCreateIn(container: ContainerNode): boolean {
    return !(container.locked && !container.unlocked);
  }

  /**
   * Only meetings and notes can be dragged: neither a task nor a dashboard has a
   * container anchor yet, so a drop would have nowhere to file it. A row that
   * cannot be dropped anywhere must not look draggable.
   */
  /**
   * Every kind the tree renders is draggable, because every one now has a mover behind it.
   *
   * This used to admit only meetings and notes — correctly, at the time: a dashboard had no
   * container to move between and a task had no local placement at all, so a drag would have
   * been a gesture with nothing to do. Both gained a backend half, and a row a user can see
   * under a project is a row they will try to drag out of it. `ItemKind` and `DraggableKind` are
   * kept as separate types on purpose: they agree today, and the day a kind is renderable but
   * not movable, this function is where that is said.
   */
  protected draggableKind(item: ItemRow): DraggableKind | null {
    switch (item.kind) {
      case "meeting":
      case "note":
      case "dashboard":
      case "task":
        return item.kind;
      default:
        return null;
    }
  }

  protected onDragStart(
    event: DragEvent,
    item: ItemRow,
    current: ContainerNode | null,
  ): void {
    const kind = this.draggableKind(item);
    if (!kind || (kind === "meeting" && current?.locked)) {
      event.preventDefault();
      this.drag.end();
      return;
    }
    this.drag.begin(item.id, kind);
    event.dataTransfer?.setData(NoteDragService.MIME, item.id);
    if (event.dataTransfer) {
      event.dataTransfer.effectAllowed = "move";
    }
  }

  protected onDragEnd(): void {
    this.drag.end();
  }

  /**
   * A sealed, not-session-unlocked container is refused by every mover, so it is
   * not a drop target. Arming one would invite a drop that can only fail.
   */
  protected canDropInto(container: ContainerNode): boolean {
    return !(container.locked && !container.unlocked);
  }

  /**
   * Every container an item could be moved INTO — the keyboard's equivalent of a drag.
   *
   * Drag and drop is a pointer gesture with no keyboard form of its own, so shipping
   * it alone would make filing an item impossible without a mouse. The same rules
   * apply as to a drop: a sealed, not-session-unlocked container is refused by every
   * mover, so it is not offered here either.
   */
  protected readonly moveTargets = computed<WorkspaceDestination[]>(() =>
    workspaceDestinations(this.workspace.forest()),
  );

  /** The container an item currently sits in, so the menu can leave it out. */
  protected moveTargetsFor(
    item: ItemRow,
    current: ContainerNode,
  ): WorkspaceDestination[] {
    const kind = this.draggableKind(item);
    if (kind === "meeting" && current.locked) {
      return [];
    }
    return kind
      ? this.moveTargets().filter(
          (target) =>
            target.container.id !== current.id &&
            this.canMoveKindInto(kind, target.container),
        )
      : [];
  }

  /** Unfiled recordings have no current container to exclude. */
  protected unfiledMoveTargets(item: ItemRow): WorkspaceDestination[] {
    const kind = this.draggableKind(item);
    return kind
      ? this.moveTargets().filter((target) =>
          this.canMoveKindInto(kind, target.container),
        )
      : [];
  }

  protected readonly moveRequest = signal<{
    item: ItemRow;
    fromLabel: string;
    targets: WorkspaceDestination[];
  } | null>(null);
  protected readonly moveBusy = signal(false);
  protected readonly moveError = signal<string | null>(null);

  protected openMove(
    item: ItemRow,
    from: ContainerNode | null,
    targets: WorkspaceDestination[],
  ): void {
    if (targets.length === 0) {
      return;
    }
    const fromLabel = from
      ? this.moveTargets().find((target) => target.container.id === from.id)
          ?.label ?? from.name
      : "Unfiled recordings";
    this.moveError.set(null);
    this.moveRequest.set({ item, fromLabel, targets });
  }

  protected closeMove(): void {
    if (!this.moveBusy()) {
      this.moveRequest.set(null);
      this.moveError.set(null);
    }
  }

  protected async moveItemTo(target: WorkspaceDestination): Promise<void> {
    const request = this.moveRequest();
    const kind = request ? this.draggableKind(request.item) : null;
    if (!request || !kind || this.moveBusy()) {
      return;
    }
    this.moveBusy.set(true);
    this.moveError.set(null);
    try {
      await this.workspace.moveItem(
        kind,
        request.item.id,
        target.container.id,
      );
      this.moveRequest.set(null);
      this.toast.success(
        `Moved “${this.itemTitle(request.item)}” to ${target.label}`,
      );
    } catch (error) {
      const message = this.errorCopy.is(error, "recording-linked-note")
        ? this.errorCopy.humanize(error)
        : `Couldn’t move “${this.itemTitle(request.item)}” to ${target.label}.`;
      this.moveError.set(message);
      this.toast.danger(message);
    } finally {
      this.moveBusy.set(false);
    }
  }

  private canMoveKindInto(
    _kind: DraggableKind,
    container: ContainerNode,
  ): boolean {
    return !(container.locked && !container.unlocked);
  }

  protected async onDropItem(
    container: ContainerNode,
    payload: { id: string; kind: DraggableKind },
  ): Promise<void> {
    // The refusal lives HERE, not in the binding below it. `dropFolderId` is data
    // the directive hands back to its consumer; it does not gate the drop, so
    // passing null there looked like a guard while the handler went on using the
    // container id regardless — and a sealed destination accepted the move.
    if (
      !this.canDropInto(container) ||
      !this.canMoveKindInto(payload.kind, container)
    ) {
      return;
    }
    const targetLabel =
      this.moveTargets().find((target) => target.container.id === container.id)
        ?.label ?? container.name;
    try {
      await this.workspace.moveItem(payload.kind, payload.id, container.id);
      this.toast.success(`Moved item to ${targetLabel}`);
    } catch (error) {
      this.toast.danger(
        this.errorCopy.is(error, "recording-linked-note")
          ? this.errorCopy.humanize(error)
          : `Couldn’t move this item to ${targetLabel}.`,
      );
    }
  }

  protected async newNote(container: ContainerNode): Promise<void> {
    try {
      const id = await this.workspace.createNote(container.id, "Untitled");
      this.toast.success(`Created a note in ${container.name}`);
      await this.router.navigate(["/notes", id]);
    } catch {
      this.toast.danger(`Couldn’t create a note in ${container.name}.`);
    }
  }

  protected async newFolder(container: ContainerNode): Promise<void> {
    try {
      const id = await this.workspace.createFolder(container, "New folder");
      this.toast.success(`Created a folder in ${container.name}`);
      await this.router.navigate(["/container", id]);
    } catch {
      this.toast.danger(`Couldn’t create a folder in ${container.name}.`);
    }
  }

  protected async newDashboard(container: ContainerNode): Promise<void> {
    try {
      const id = await this.workspace.createDashboard(
        container.id,
        "New dashboard",
      );
      this.toast.success(`Created a dashboard in ${container.name}`);
      await this.router.navigate(["/dashboards", id]);
    } catch {
      this.toast.danger(`Couldn’t create a dashboard in ${container.name}.`);
    }
  }

  /**
   * Lock, unlock, rename and delete — the per-container actions the two per-type trees
   * carried before this one replaced them.
   *
   * They are not decoration. Locking a folder from the sidebar is how a user makes a
   * folder private at all, and removing the trees that offered it would have taken the
   * feature away rather than moved it. Creation and management remain separate
   * controls so their consequences are not conflated.
   */
  /// Locking goes through the lock FLOW, never `FoldersService.lock` directly.
  ///
  /// The flow is what checks for live shares first and puts the lock×shares dialog in front
  /// of the seal. Calling the service straight through looked equivalent and silently
  /// removed that gate — a folder shared with someone would have been sealed without ever
  /// asking, which is the case the dialog exists for.
  protected async lock(container: ContainerNode): Promise<void> {
    if (this.lockFlow.busy()) {
      return;
    }
    await this.lockFlow.requestLock(container.id, container.name, async () => {
      await this.refreshAfterLockChange();
    });
  }

  /**
   * Refresh everything the TWO trees used to refresh between them.
   *
   * This one replaced both, so it inherited both their obligations. Reloading only its own
   * forest left the Notes and Meetings surfaces — and the caches that scrub themselves when
   * a folder's lock state changes — reading stale rows, which for a LOCK means plaintext
   * still on screen after the folder holding it was sealed.
   */
  private async refreshAfterLockChange(): Promise<void> {
    await Promise.allSettled([
      this.workspace.reload(),
      this.notes.loadFolders(),
      this.notes.loadNotes(null),
      this.folders.load(),
    ]);
  }

  protected async unlock(container: ContainerNode): Promise<void> {
    await this.folders.unlock(container.id);
    await this.refreshAfterLockChange();
  }

  protected async relock(container: ContainerNode): Promise<void> {
    await this.folders.relock(container.id);
    await this.refreshAfterLockChange();
  }

  protected readonly manageRequest = signal<{
    mode: WorkspaceManageMode;
    container: ContainerNode;
  } | null>(null);
  protected readonly manageBusy = signal(false);
  protected readonly manageError = signal<string | null>(null);

  protected startRename(container: ContainerNode): void {
    this.manageError.set(null);
    this.manageRequest.set({ mode: "rename", container });
  }

  protected startDelete(container: ContainerNode): void {
    this.manageError.set(null);
    this.manageRequest.set({ mode: "delete", container });
  }

  protected closeManage(): void {
    if (!this.manageBusy()) {
      this.manageRequest.set(null);
      this.manageError.set(null);
    }
  }

  protected async rename(name: string): Promise<void> {
    const request = this.manageRequest();
    if (!request || request.mode !== "rename" || this.manageBusy()) {
      return;
    }
    this.manageBusy.set(true);
    this.manageError.set(null);
    try {
      if (request.container.kind === "note") {
        await this.ipc.renameNoteFolder(request.container.id, name);
        await this.notes.loadFolders();
      } else {
        await this.folders.rename(request.container.id, name);
      }
      await this.workspace.reload();
      this.manageRequest.set(null);
      this.toast.success(
        `Renamed ${this.containerNoun(request.container)} to “${name}”`,
      );
    } catch {
      this.manageError.set(
        `Couldn’t rename “${request.container.name}”. Please try again.`,
      );
    } finally {
      this.manageBusy.set(false);
    }
  }

  protected async remove(): Promise<void> {
    const request = this.manageRequest();
    if (!request || request.mode !== "delete" || this.manageBusy()) {
      return;
    }
    this.manageBusy.set(true);
    this.manageError.set(null);
    try {
      if (request.container.kind === "note") {
        await this.ipc.deleteNoteFolder(request.container.id);
        await this.notes.loadFolders();
      } else {
        await this.folders.delete(request.container.id);
      }
      await this.workspace.reload();
      this.manageRequest.set(null);
      this.toast.success(
        `Deleted ${this.containerNoun(request.container)} “${request.container.name}”`,
      );
      if (this.isContainerSelected(request.container)) {
        const first = this.workspace.forest()[0];
        await this.router.navigate(
          first ? ["/container", first.id] : ["/record"],
        );
      }
    } catch {
      this.manageError.set(
        `Couldn’t delete “${request.container.name}”. It may still contain nested folders.`,
      );
    } finally {
      this.manageBusy.set(false);
    }
  }

  /** The ONE user-facing noun for this level — see `core/hierarchy-vocabulary.ts`. */
  protected readonly containerNoun = containerNoun;

  protected canCreateNote(container: ContainerNode): boolean {
    return this.canCreateIn(container);
  }

  protected canManage(container: ContainerNode): boolean {
    return !container.isRoot;
  }

  /** The reserved note root can never be sealed, so it is never offered a lock. */
  protected canLock(container: ContainerNode): boolean {
    return !container.isRoot && !container.locked;
  }

  /**
   * What locking this container will actually do.
   *
   * Locking cascades: a container holding containers seals every one of them, because a project
   * that rendered locked while the folders inside it stayed readable would be a label, not a
   * lock. The menu has to say so — "Lock folder" on a project with six folders under it describes
   * a much smaller action than the one about to happen, and a user who finds out afterwards has
   * been surprised by a security control, which is the worst place to be surprised.
   */
  protected lockLabel(container: ContainerNode): string {
    const nested = container.folders.length;
    const noun = container.level === "project" ? "Workspace" : "folder";
    if (nested === 0) {
      return `Lock ${noun}`;
    }
    return nested === 1
      ? `Lock ${noun} and the folder inside it`
      : `Lock ${noun} and the ${nested} folders inside it`;
  }

  /** The unlock half of {@link lockLabel} — it cascades too. */
  protected unlockLabel(container: ContainerNode): string {
    if (this.isSealed(container)) {
      // A sealed row must not reveal whether the payload contains descendants.
      return "Unlock for this session";
    }
    const nested = container.folders.length;
    return nested === 0
      ? "Unlock for this session"
      : `Unlock this ${container.level === "project" ? "Workspace" : "folder"} and its folders for this session`;
  }

  // ── AI organize, per container ────────────────────────────────────────────
  //
  // The planner and the review sheet already existed; they were reachable only
  // from the Notes home header, scoped to whichever note-folder happened to be
  // active. That is the wrong place for them now: the thing a user wants to
  // tidy is a PROJECT or a FOLDER, and the hierarchy is where those are named.
  //
  // Two-step and non-destructive, unchanged: `plan_organize_notes` PROPOSES,
  // the sheet lets the user drop individual moves, and nothing moves until
  // `apply_organize_plan`. An AI that silently re-filed a vault would be a
  // feature nobody could trust twice.

  /** The proposed plan; `null` means the sheet is closed. */
  protected readonly organizePlan = signal<OrganizeViewPlan | null>(null);
  /** Content-free identity of the reviewed container; never retain its item tree. */
  private readonly organizeScopeId = signal<string | null>(null);
  /** True while the plan is being fetched (the menu entry says so). */
  protected readonly organizePlanning = signal(false);
  /** True while the apply is in flight (the sheet's own spinner). */
  protected readonly organizeApplying = signal(false);
  private organizePlanGeneration = 0;

  /**
   * A sealed container cannot be organized, and the reason is the planner's:
   * it reads titles and body excerpts to classify them, and those reads are
   * gated. Offering the action would produce an empty plan and look broken.
   */
  protected canOrganize(container: ContainerNode): boolean {
    return (
      !container.locked &&
      container.groups.some((group) => group.kind === "note" && group.total > 0)
    );
  }

  protected async organize(container: ContainerNode): Promise<void> {
    if (this.organizePlanning() || this.organizePlan()) {
      return;
    }
    this.organizeScopeId.set(container.id);
    await this.planOrganize(container.id, null);
  }

  protected async replanOrganize(guidance: string): Promise<void> {
    const scopeId = this.organizeScopeId();
    if (!scopeId || this.organizePlanning() || this.organizeApplying()) {
      return;
    }
    await this.planOrganize(scopeId, guidance || null);
  }

  private async planOrganize(
    scopeId: string,
    guidance: string | null,
  ): Promise<void> {
    const generation = ++this.organizePlanGeneration;
    this.organizePlanning.set(true);
    try {
      const privacyReady = await this.privacyBarrier.ensureReady();
      if (
        generation !== this.organizePlanGeneration ||
        this.organizeScopeId() !== scopeId
      ) {
        return;
      }
      if (!privacyReady) {
        this.scrubOrganizeReview();
        return;
      }
      const plan = await this.ipc.planOrganizeNotes(scopeId, guidance);
      if (
        generation !== this.organizePlanGeneration ||
        this.organizeScopeId() !== scopeId
      ) {
        return;
      }
      this.organizePlan.set({
        ...plan,
        plannedProposedCount: plan.moves.length,
        receipt: null,
        applyError: null,
      });
    } catch {
      if (
        generation === this.organizePlanGeneration &&
        this.organizeScopeId() === scopeId
      ) {
        this.toast.danger("Couldn't plan an auto-organize. Please try again.");
      }
    } finally {
      if (
        generation === this.organizePlanGeneration &&
        this.organizeScopeId() === scopeId
      ) {
        this.organizePlanning.set(false);
      }
    }
  }

  protected async applyOrganize(moves: OrganizeMove[]): Promise<void> {
    if (this.organizePlanning() || this.organizeApplying()) {
      return;
    }
    if (moves.length === 0) {
      this.closeOrganize();
      return;
    }
    const viewPlan = this.organizePlan();
    const scopeId = this.organizeScopeId();
    if (!viewPlan || !scopeId || viewPlan.scopeFolderId !== scopeId) {
      return;
    }
    const generation = this.organizePlanGeneration;
    this.organizeApplying.set(true);
    try {
      const privacyReady = await this.privacyBarrier.ensureReady();
      if (
        generation !== this.organizePlanGeneration ||
        this.organizeScopeId() !== scopeId
      ) {
        return;
      }
      if (!privacyReady) {
        this.scrubOrganizeReview();
        return;
      }
      const plan: OrganizePlan = {
        scopeFolderId: viewPlan.scopeFolderId,
        moves,
        totalScanned: viewPlan.totalScanned,
        alreadyOrganized: viewPlan.alreadyOrganized,
        deferred: viewPlan.deferred,
        targets: viewPlan.targets,
      };
      const result = await this.ipc.applyOrganizePlan(plan);
      if (
        generation !== this.organizePlanGeneration ||
        this.organizeScopeId() !== scopeId
      ) {
        return;
      }
      await this.workspace.reload();
      if (
        generation !== this.organizePlanGeneration ||
        this.organizeScopeId() !== scopeId
      ) {
        return;
      }
      const receipt = this.mergeOrganizeReceipt(
        viewPlan.receipt,
        moves,
        result.appliedIds,
        result.failures,
      );
      if (receipt.failures.length === 0) {
        this.toast.success(
          `${result.appliedIds.length} ${result.appliedIds.length === 1 ? "note" : "notes"} organized`,
        );
        // A successful apply still owns the busy flag, so bypass the public
        // close guard and synchronously evict the completed review.
        this.scrubOrganizeReview();
      } else {
        this.organizePlan.set({
          ...viewPlan,
          receipt,
          applyError: null,
        });
        this.toast.danger(
          `${result.appliedIds.length} moved; ${receipt.failures.length} still need attention.`,
        );
      }
    } catch {
      if (
        generation === this.organizePlanGeneration &&
        this.organizeScopeId() === scopeId
      ) {
        this.organizePlan.update((plan) =>
          plan
            ? {
                ...plan,
                applyError:
                  "The filing request did not finish. Review the selected moves and retry.",
              }
            : plan,
        );
        this.toast.danger("Couldn't finish applying the plan.");
      }
    } finally {
      if (
        generation === this.organizePlanGeneration &&
        this.organizeScopeId() === scopeId
      ) {
        this.organizeApplying.set(false);
      }
    }
  }

  protected closeOrganize(): void {
    if (this.organizePlanning() || this.organizeApplying()) {
      return;
    }
    this.scrubOrganizeReview();
  }

  /** Synchronous privacy boundary for every copied organizer plaintext field. */
  private scrubOrganizeReview(): void {
    ++this.organizePlanGeneration;
    this.organizePlanning.set(false);
    this.organizeApplying.set(false);
    this.organizePlan.set(null);
    this.organizeScopeId.set(null);
  }

  private mergeOrganizeReceipt(
    previous: OrganizeAttemptReceipt | null | undefined,
    attemptedMoves: readonly OrganizeMove[],
    appliedIds: readonly string[],
    failures: readonly OrganizeFailure[],
  ): OrganizeAttemptReceipt {
    const moves = new Map(
      (previous?.moves ?? []).map((move) => [move.noteId, move]),
    );
    const applied = new Set(previous?.appliedIds ?? []);
    const unresolved = new Map(
      (previous?.failures ?? []).map((failure) => [failure.noteId, failure]),
    );
    for (const move of attemptedMoves) {
      moves.set(move.noteId, move);
      unresolved.delete(move.noteId);
    }
    for (const id of appliedIds) {
      applied.add(id);
      unresolved.delete(id);
    }
    for (const failure of failures) {
      if (!applied.has(failure.noteId)) {
        unresolved.set(failure.noteId, failure);
      }
    }
    return {
      moves: [...moves.values()],
      appliedIds: [...applied],
      failures: [...unresolved.values()],
    };
  }

  protected openAll(container: ContainerNode): void {
    this.openContainer(container);
  }

  protected openAllRecordings(): void {
    void this.notes.selectFolder(null);
    this.folders.selectFolder(null);
    void this.router.navigate(["/library"]);
  }
}

/** Read the persisted shared-container disclosure set, tolerating a blocked store. */
function readStoredSharedSet(): ReadonlySet<string> {
  try {
    const raw = localStorage.getItem(EXPANDED_SHARED_KEY);
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

/** Read a persisted boolean, tolerating a blocked or empty store. */
function readStoredBoolean(key: string, fallback: boolean): boolean {
  try {
    const raw = localStorage.getItem(key);
    return raw === null ? fallback : JSON.parse(raw) === true;
  } catch {
    return fallback;
  }
}
