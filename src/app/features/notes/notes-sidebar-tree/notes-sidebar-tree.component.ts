import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  inject,
  input,
  signal,
  viewChild,
} from "@angular/core";
import { Router } from "@angular/router";
import type { NoteFolder } from "../../../core/models";
import { MurTreeRowComponent } from "../../../design-system/tree-row/tree-row.component";
import { FolderLockFlowService } from "../../../services/folder-lock-flow.service";
import { FoldersService } from "../../../services/folders.service";
import { NotesService } from "../../../services/notes.service";
import { ToastService } from "../../../services/toast.service";

/**
 * The Notes section of the ALWAYS-VISIBLE main sidebar (Notion/Obsidian model,
 * 2026-07-12): an Obsidian-style expandable list of note-kind folders +
 * "All notes", with inline create/rename/delete and lock/unlock. Mounted by
 * `AppShellComponent` under the "Notes" nav item — this REPLACES the local
 * `.folders-pane` rail `NotesHomeComponent` used to own when `/notes` was a
 * drill-down that hid the primary sidebar. `NotesHomeComponent` now only
 * renders the content pane and reads the shared selection from
 * {@link NotesService.activeFolderId}.
 *
 * Folder LOCK/UNLOCK reuses the SAME lock×shares flow the meetings tree runs
 * ({@link FolderLockFlowService}) — a shared note-folder is never sealed
 * without the owner deciding what happens to outstanding shares (PK-F1). The
 * dialog itself is NOT rendered here (2026-07-12 fix) — see `AppShellComponent`,
 * which renders it exactly once for the whole app now that both this tree and
 * `MeetingsSidebarTreeComponent`'s rows drive the SAME singleton flow.
 *
 * This component is imported directly by the eagerly-loaded `AppShellComponent`
 * (like `FoldersService`'s `unlockedCount` badge already is) so the sidebar has
 * live folder data on every route, not just while `/notes` is active.
 */
@Component({
  selector: "app-notes-sidebar-tree",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurTreeRowComponent],
  templateUrl: "./notes-sidebar-tree.component.html",
  styleUrl: "./notes-sidebar-tree.component.scss",
})
export class NotesSidebarTreeComponent {
  private readonly notes = inject(NotesService);
  private readonly folders = inject(FoldersService);
  private readonly toast = inject(ToastService);
  private readonly router = inject(Router);
  private readonly injector = inject(Injector);

  /** Shared lock×shares flow (probe → warn/revoke dialog → lock). */
  readonly lockFlow = inject(FolderLockFlowService);

  /** The note-kind folder list. */
  readonly noteFolders = this.notes.noteFolders;
  /** The shared active-folder selection (null = "All notes"). */
  readonly activeFolderId = this.notes.activeFolderId;

  /**
   * True while `/notes`/`/notes/:id` is the CURRENT route (bound from
   * `AppShellComponent.isNotesRoute`). Gates the VISUAL selected pill
   * (2026-07-12 fix) — `activeFolderId` is a persisted "last selected folder"
   * that lives on regardless of which page is open, so without this gate a
   * folder kept showing as "selected" even while looking at `/library` or
   * `/record`, misleadingly claiming Notes was current when nothing about it
   * was. The underlying selection is NOT cleared — returning to `/notes`
   * still shows the same folder — only the active/selected STYLING is gated.
   */
  readonly sectionActive = input(false);

  /** Id of the folder whose lock/unlock op is in flight. */
  readonly lockBusyId = signal<string | null>(null);

  // --- New-folder inline create -------------------------------------------
  readonly creatingFolder = signal(false);
  readonly folderDraft = signal("");
  readonly folderBusy = signal(false);
  private readonly folderInput =
    viewChild<ElementRef<HTMLInputElement>>("folderInput");

  // --- Rename inline --------------------------------------------------------
  readonly renamingId = signal<string | null>(null);
  readonly renameDraft = signal("");

  // --- Delete confirm --------------------------------------------------------
  readonly pendingDeleteId = signal<string | null>(null);

  constructor() {
    void this.notes.loadFolders();
  }

  /** Select a folder (or null for "All notes") and land on the Notes list. */
  async select(folderId: string | null): Promise<void> {
    await this.notes.selectFolder(folderId);
    void this.router.navigate(["/notes"]);
  }

  // --- Folder create / rename / delete ------------------------------------

  /**
   * Open the inline "New folder" field. Public so `AppShellComponent`'s
   * compact "+" icon (next to the "Notes" section header, 2026-07-12 —
   * replaces the full-width dashed CTA row that used to live here and read
   * as duplicate UI next to Meetings' identical row) can trigger it via a
   * `viewChild` reference.
   */
  startCreateFolder(): void {
    this.cancelFolderEdits();
    this.folderDraft.set("");
    this.creatingFolder.set(true);
    afterNextRender(() => this.folderInput()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

  async confirmCreateFolder(event: Event): Promise<void> {
    event.preventDefault();
    const name = this.folderDraft().trim();
    if (!name || this.folderBusy()) {
      return;
    }
    this.folderBusy.set(true);
    try {
      const folder = await this.notes.createFolder(name, null);
      this.creatingFolder.set(false);
      this.folderDraft.set("");
      await this.select(folder.id);
    } catch {
      this.toast.danger("Couldn’t create the folder. Please try again.");
    } finally {
      this.folderBusy.set(false);
    }
  }

  startRename(folder: NoteFolder, event: Event): void {
    event.stopPropagation();
    this.cancelFolderEdits();
    this.renamingId.set(folder.id);
    this.renameDraft.set(folder.name);
  }

  async confirmRename(id: string, event: Event): Promise<void> {
    event.preventDefault();
    const name = this.renameDraft().trim();
    if (!name || this.folderBusy()) {
      return;
    }
    this.folderBusy.set(true);
    try {
      await this.notes.renameFolder(id, name);
      this.renamingId.set(null);
    } catch {
      this.toast.danger("Couldn’t rename the folder. Please try again.");
    } finally {
      this.folderBusy.set(false);
    }
  }

  askDeleteFolder(folder: NoteFolder, event: Event): void {
    event.stopPropagation();
    this.cancelFolderEdits();
    this.pendingDeleteId.set(folder.id);
  }

  async confirmDeleteFolder(id: string): Promise<void> {
    if (this.folderBusy()) {
      return;
    }
    this.folderBusy.set(true);
    try {
      await this.notes.deleteFolder(id);
      this.pendingDeleteId.set(null);
      if (this.activeFolderId() === id) {
        await this.notes.selectFolder(null);
      }
    } catch {
      this.toast.danger("Couldn’t delete the folder. Please try again.");
    } finally {
      this.folderBusy.set(false);
    }
  }

  cancelFolderEdits(): void {
    this.creatingFolder.set(false);
    this.renamingId.set(null);
    this.pendingDeleteId.set(null);
  }

  // --- Folder lock / unlock -------------------------------------------------

  async lockFolder(folder: NoteFolder, event: Event): Promise<void> {
    event.stopPropagation();
    if (this.lockBusyId() !== null || this.lockFlow.busy()) {
      return;
    }
    this.lockBusyId.set(folder.id);
    try {
      await this.lockFlow.requestLock(folder.id, folder.name, async () => {
        await this.refreshAfterLockChange();
        this.toast.success(`Locked “${folder.name}”`);
      });
    } catch {
      this.toast.danger("Couldn’t lock this folder. Please try again.");
    } finally {
      this.lockBusyId.set(null);
    }
  }

  /** Session-unlock a sealed note-folder (Touch ID via the shared folder command). */
  async unlockFolder(folder: NoteFolder, event: Event): Promise<void> {
    event.stopPropagation();
    if (this.lockBusyId() !== null) {
      return;
    }
    this.lockBusyId.set(folder.id);
    try {
      await this.folders.unlock(folder.id);
      await this.refreshAfterLockChange();
    } catch {
      // A cancelled/denied Touch ID prompt — stay masked, no scary toast.
      this.toast.danger("Couldn’t unlock this folder.");
    } finally {
      this.lockBusyId.set(null);
    }
  }

  private async refreshAfterLockChange(): Promise<void> {
    await Promise.allSettled([
      this.notes.loadFolders(),
      this.notes.loadNotes(this.activeFolderId()),
    ]);
  }
}
