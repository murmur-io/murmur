import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  input,
} from "@angular/core";
import { Router } from "@angular/router";

import { MurRowMenuComponent } from "../../../design-system/row-menu/row-menu.component";
import { FolderDropDirective } from "../../folders/folder-drop.directive";
import {
  NoteDragService,
  type DraggableKind,
} from "../../folders/note-drag.service";
import {
  MurTreeRowComponent,
  type TreeRowIcon,
} from "../../../design-system/tree-row/tree-row.component";
import type { ContainerNode, ItemKind, ItemRow, TypeGroup } from "../../../core/models";
import { FolderLockFlowService } from "../../../services/folder-lock-flow.service";
import { FoldersService } from "../../../services/folders.service";
import { NotesService } from "../../../services/notes.service";
import { WorkspaceService } from "../workspace.service";

/** A flattened tree line, so one `@for` renders the whole forest. */
export interface TreeLine {
  /** Stable identity for `track` — unique across every line kind. */
  key: string;
  depth: number;
  container: ContainerNode;
  /** Present on a type-group header line and on an item line. */
  group?: TypeGroup;
  /** Present only on an item line. */
  item?: ItemRow;
  /** Present only on a "see all" line. */
  seeAll?: boolean;
}

/** Polish labels for the four kinds, in the order the backend emits them. */
const KIND_LABEL: Record<ItemKind, string> = {
  meeting: "Meetings",
  note: "Notes",
  task: "Tasks",
  dashboard: "Dashboards",
};

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
 * holding collapsible per-type groups of Notes, Meetings, Tasks and Dashboards.
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
  imports: [FolderDropDirective, MurRowMenuComponent, MurTreeRowComponent],
  templateUrl: "./workspace-tree.component.html",
  styleUrl: "./workspace-tree.component.scss",
})
export class WorkspaceTreeComponent {
  private readonly router = inject(Router);
  protected readonly workspace = inject(WorkspaceService);
  private readonly drag = inject(NoteDragService);
  private readonly folders = inject(FoldersService);
  private readonly lockFlow = inject(FolderLockFlowService);
  private readonly notes = inject(NotesService);

  /** Whether this section owns the current route (drives selection styling). */
  readonly sectionActive = input(false);

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
    if (this.workspace.forestEmpty()) {
      void this.workspace.reload();
    }
  }

  protected readonly loading = this.workspace.loading;
  protected readonly error = this.workspace.error;
  protected readonly forestEmpty = this.workspace.forestEmpty;

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
    for (const project of this.workspace.forest()) {
      this.pushContainer(out, project, 0);
    }
    return out;
  });

  private pushContainer(out: TreeLine[], container: ContainerNode, depth: number): void {
    out.push({ key: `c:${container.id}`, depth, container });
    if (!this.workspace.isContainerExpanded(container.id)) {
      return;
    }
    for (const group of container.groups) {
      const groupKey = `g:${container.id}:${group.kind}`;
      out.push({ key: groupKey, depth: depth + 1, container, group });
      if (!this.workspace.isGroupExpanded(container.id, group.kind)) {
        continue;
      }
      for (const item of group.items) {
        out.push({
          key: `i:${item.kind}:${item.id}`,
          depth: depth + 2,
          container,
          group,
          item,
        });
      }
      if (group.total > group.items.length) {
        out.push({ key: `s:${groupKey}`, depth: depth + 2, container, group, seeAll: true });
      }
    }
    for (const child of container.folders) {
      this.pushContainer(out, child, depth + 1);
    }
  }

  protected kindLabel(kind: ItemKind): string {
    return KIND_LABEL[kind];
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

  /** A sealed container reads as its lock; an open one as what it is. */
  protected containerIcon(container: ContainerNode): TreeRowIcon {
    if (container.locked && !container.unlocked) {
      return "locked";
    }
    return container.level === "project" ? "project" : "folder";
  }

  protected itemTitle(item: ItemRow): string {
    const title = item.title?.trim();
    return title ? title : "Untitled";
  }

  protected seeAllLabel(group: TypeGroup): string {
    return `See all (${group.total})`;
  }

  /** A container with no groups and no folders has nothing to disclose. */
  protected containerExpandable(container: ContainerNode): boolean {
    return container.groups.length > 0 || container.folders.length > 0;
  }

  protected isContainerExpanded(container: ContainerNode): boolean {
    return this.workspace.isContainerExpanded(container.id);
  }

  protected isGroupExpanded(container: ContainerNode, group: TypeGroup): boolean {
    return this.workspace.isGroupExpanded(container.id, group.kind);
  }

  protected toggleContainer(container: ContainerNode): void {
    this.workspace.toggleContainer(container.id);
  }

  protected toggleGroup(container: ContainerNode, group: TypeGroup): void {
    this.workspace.toggleGroup(container.id, group.kind);
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
  protected draggableKind(item: ItemRow): DraggableKind | null {
    return item.kind === "meeting" || item.kind === "note" ? item.kind : null;
  }

  protected onDragStart(event: DragEvent, item: ItemRow): void {
    const kind = this.draggableKind(item);
    if (!kind) {
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
  protected readonly moveTargets = computed<ContainerNode[]>(() => {
    const out: ContainerNode[] = [];
    const walk = (container: ContainerNode): void => {
      if (this.canDropInto(container)) {
        out.push(container);
      }
      container.folders.forEach(walk);
    };
    this.workspace.forest().forEach(walk);
    return out;
  });

  /** The container an item currently sits in, so the menu can leave it out. */
  protected moveTargetsFor(item: ItemRow, current: ContainerNode): ContainerNode[] {
    return this.draggableKind(item)
      ? this.moveTargets().filter((target) => target.id !== current.id)
      : [];
  }

  protected async moveItemTo(item: ItemRow, target: ContainerNode): Promise<void> {
    const kind = this.draggableKind(item);
    if (!kind) {
      return;
    }
    await this.workspace.moveItem(kind, item.id, target.id);
  }

  protected async onDropItem(
    container: ContainerNode,
    payload: { id: string; kind: DraggableKind },
  ): Promise<void> {
    // The refusal lives HERE, not in the binding below it. `dropFolderId` is data
    // the directive hands back to its consumer; it does not gate the drop, so
    // passing null there looked like a guard while the handler went on using the
    // container id regardless — and a sealed destination accepted the move.
    if (!this.canDropInto(container)) {
      return;
    }
    await this.workspace.moveItem(payload.kind, payload.id, container.id);
  }

  protected async newNote(container: ContainerNode): Promise<void> {
    const id = await this.workspace.createNote(container.id, "New note");
    await this.router.navigate(["/notes", id]);
  }

  protected async newFolder(container: ContainerNode): Promise<void> {
    await this.workspace.createFolder(container.id, "New folder");
  }

  /**
   * Lock, unlock, rename and delete — the per-container actions the two per-type trees
   * carried before this one replaced them.
   *
   * They are not decoration. Locking a folder from the sidebar is how a user makes a
   * folder private at all, and removing the trees that offered it would have taken the
   * feature away rather than moved it. They live behind the same `…` a ClickUp row uses,
   * beside the `+`, so creating and managing stay visibly different actions.
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

  protected async rename(container: ContainerNode): Promise<void> {
    const name = window.prompt("Rename folder", container.name)?.trim();
    if (!name || name === container.name) {
      return;
    }
    await this.folders.rename(container.id, name);
    await this.workspace.reload();
  }

  protected async remove(container: ContainerNode): Promise<void> {
    await this.folders.delete(container.id);
    await this.workspace.reload();
  }

  /** The reserved note root can never be sealed, so it is never offered a lock. */
  protected canLock(container: ContainerNode): boolean {
    return !container.isRoot && !container.locked;
  }

  protected openGroup(container: ContainerNode, group: TypeGroup): void {
    void this.router.navigate(["/container", container.id], {
      queryParams: { kind: group.kind },
    });
  }
}
