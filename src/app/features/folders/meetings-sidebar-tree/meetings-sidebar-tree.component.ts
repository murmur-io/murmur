import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  input,
  viewChild,
} from "@angular/core";
import { Router } from "@angular/router";
import type { FolderNode } from "../../../core/models";
import { FoldersService } from "../../../services/folders.service";
import { ToastService } from "../../../services/toast.service";
import { FolderTreeComponent } from "../folder-tree/folder-tree.component";

/**
 * The Meetings section of the ALWAYS-VISIBLE main sidebar (Stage 2 of the
 * Notion/Obsidian-style navigation work, 2026-07-12 — mirrors
 * `NotesSidebarTreeComponent`, Stage 1's equivalent for Notes). Mounted by
 * `AppShellComponent` under the "Meetings" nav item.
 *
 * Unlike Notes (which needed a NEW lean folder-tree component), the meetings
 * folder tree ({@link FolderTreeComponent} / `FolderRowComponent`) is ALREADY
 * a fully self-contained, meeting-folder-bound component — the SAME one
 * `LibraryComponent`'s local rail used to own (create/rename/delete/lock/
 * unlock/drag-drop, all internal to `FolderTreeComponent` itself). This
 * wrapper only adds the "Lock all" privacy affordance (which lived in
 * `LibraryComponent`'s rail header, not the tree) and owns the drag-drop
 * landing (`onDropNote`) + folder selection, both now against the SHARED
 * {@link FoldersService.activeFolderId} so `LibraryComponent`'s content pane
 * stays in sync without a route param (same pattern as Notes' `selectFolder`).
 */
@Component({
  selector: "app-meetings-sidebar-tree",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FolderTreeComponent],
  templateUrl: "./meetings-sidebar-tree.component.html",
  styleUrl: "./meetings-sidebar-tree.component.scss",
})
export class MeetingsSidebarTreeComponent {
  private readonly toast = inject(ToastService);
  private readonly router = inject(Router);

  /** Shared folder store (tree data + the lock lifecycle FolderTreeComponent drives). */
  readonly folders = inject(FoldersService);

  /**
   * True while `/library`/`/meeting/:id` is the CURRENT route (bound from
   * `AppShellComponent.isMeetingsRoute`). Gates the folder tree's VISUAL
   * selected pill (2026-07-12 fix) the SAME way `NotesSidebarTreeComponent`
   * does — `FoldersService.activeFolderId` is a persisted "last selected
   * folder" that outlives the route, so without this gate a folder kept
   * showing as selected even while looking at `/notes` or `/record`. Forwarded
   * straight into `<app-folder-tree>`'s `selectionActive` input (threaded
   * recursively to `FolderRowComponent` → nested tree levels, same as
   * `depth`) rather than nulling `selectedId` — `null` is itself a
   * meaningful value here (the vault root), so nulling it while inactive
   * would have made the ROOT row incorrectly show as selected instead.
   */
  readonly sectionActive = input(false);

  /**
   * The nested `<app-folder-tree>` — already owns the inline "New folder"
   * create UI internally (`openCreate()`/`confirmCreate()`, its own
   * `.new-folder` field). {@link openCreateFolder} is a thin forward so
   * `AppShellComponent`'s compact "+" icon (next to the "Meetings" section
   * header, 2026-07-12 — replaces the full-width dashed CTA row that used to
   * live inside this tree and read as duplicate UI next to Notes' identical
   * row) can trigger it without this component re-implementing create logic
   * `FolderTreeComponent` already owns.
   */
  private readonly folderTree = viewChild(FolderTreeComponent);

  /** Open the tree's inline "New folder" field (forwarded from the section header's "+"). */
  openCreateFolder(): void {
    this.folderTree()?.openCreate();
  }

  /** Every folder node keyed by id (flattened) — for the drop-toast's folder name. */
  private readonly folderById = computed(() => {
    const map = new Map<string, FolderNode>();
    const walk = (nodes: FolderNode[]): void => {
      for (const n of nodes) {
        map.set(n.id, n);
        if (n.children?.length) {
          walk(n.children);
        }
      }
    };
    walk(this.folders.tree());
    return map;
  });

  /**
   * Select a folder (or null for no filter) — the shared sidebar/content
   * scope — and, for a REAL folder pick, land on `/library` so the selection
   * is actually visible (mirrors `NotesSidebarTreeComponent.select`, which
   * navigates to `/notes`; fixed 2026-07-12 — without this, clicking a
   * folder row from `/record` or a meeting tab silently set the filter with
   * no visible effect at all). The `null` case (the delete-of-selected
   * fallback bubbled from `FolderRowComponent.onDelete`) only clears the
   * filter — the "all items" navigation belongs to the section header's own
   * routerLink (`AppShellComponent.onMeetingsHeaderSelect`).
   */
  select(folderId: string | null): void {
    this.folders.selectFolder(folderId);
    if (folderId !== null) {
      void this.router.navigate(["/library"]);
    }
  }

  /**
   * A note was dropped onto a folder (or "All notes") from `LibraryComponent`'s
   * row grip (the SAME `NoteDragService` singleton coordinates the drag
   * regardless of which component mounts the source row vs. the drop target,
   * so this works unchanged even though the source row lives in a DIFFERENT
   * component now). Mirrors the drop handler `LibraryComponent` used to own.
   */
  async onDropNote(payload: {
    meetingId: string;
    folderId: string | null;
  }): Promise<void> {
    try {
      await this.folders.moveNote(payload.meetingId, payload.folderId);
      const name =
        payload.folderId === null
          ? "All notes"
          : (this.folderById().get(payload.folderId)?.name ?? "folder");
      this.toast.success(`Moved to ${name}`);
    } catch {
      this.toast.danger("Couldn’t move this note. Please try again.");
    }
  }
}
