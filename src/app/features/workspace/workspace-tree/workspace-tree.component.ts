import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  input,
} from "@angular/core";
import { Router } from "@angular/router";

import { MurRowMenuComponent } from "../../../design-system/row-menu/row-menu.component";
import {
  MurTreeRowComponent,
  type TreeRowIcon,
} from "../../../design-system/tree-row/tree-row.component";
import type { ContainerNode, ItemKind, ItemRow, TypeGroup } from "../../../core/models";
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
  meeting: "Spotkania",
  note: "Notatki",
  task: "Zadania",
  dashboard: "Dashboardy",
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
  imports: [MurRowMenuComponent, MurTreeRowComponent],
  templateUrl: "./workspace-tree.component.html",
  styleUrl: "./workspace-tree.component.scss",
})
export class WorkspaceTreeComponent {
  private readonly router = inject(Router);
  protected readonly workspace = inject(WorkspaceService);

  /** Whether this section owns the current route (drives selection styling). */
  readonly sectionActive = input(false);

  /**
   * Load the forest when this tree first appears.
   *
   * The section header's toggle also reloads, but it cannot be the only trigger:
   * the section is EXPANDED by default, so on a fresh profile the tree renders
   * without anyone having toggled anything and would sit on "Brak projektów"
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
    return title ? title : "Bez tytułu";
  }

  protected seeAllLabel(group: TypeGroup): string {
    return `Zobacz wszystkie (${group.total})`;
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

  protected openContainer(container: ContainerNode): void {
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

  protected async newNote(container: ContainerNode): Promise<void> {
    const id = await this.workspace.createNote(container.id, "Nowa notatka");
    await this.router.navigate(["/notes", id]);
  }

  protected async newFolder(container: ContainerNode): Promise<void> {
    await this.workspace.createFolder(container.id, "Nowy folder");
  }

  protected openGroup(container: ContainerNode, group: TypeGroup): void {
    void this.router.navigate(["/container", container.id], {
      queryParams: { kind: group.kind },
    });
  }
}
