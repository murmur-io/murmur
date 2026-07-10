import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  OnInit,
  afterNextRender,
  computed,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { Router } from "@angular/router";
import { NavHistoryService } from "../../../core/nav-history.service";
import type { NoteFolder, OrganizeMove, OrganizePlan } from "../../../core/models";
import { MurSidebarComponent } from "../../../design-system/sidebar/sidebar.component";
import { FoldersService } from "../../../services/folders.service";
import { NotesService } from "../../../services/notes.service";
import { ToastService } from "../../../services/toast.service";
import { OrganizeSheetComponent } from "../organize-sheet/organize-sheet.component";

/**
 * The Notes landing view — a Meetings-style drill-down `[note-folder rail |
 * note list]`. The left rail lists + manages the note-kind folders (create /
 * rename / delete / lock / unlock — a lean list, NOT the meetings
 * `FolderTreeComponent`, whose service/commands are meeting-folder bound); the
 * content pane shows the note cards for the selected folder, a "New note"
 * action, a per-note "Move to…" menu, and an "Auto-organize" flow that reviews
 * a proposed plan before applying it. Clicking a card opens `/notes/:id`.
 *
 * A full drill-down (app-shell hides the primary rail on `/notes`): the fixed
 * host fills the window, mirroring `LibraryComponent`.
 *
 * Folder LOCK/UNLOCK reuses the existing folder lock lifecycle (`FoldersService`
 * owns the biometric IPC — the same commands the meetings side uses, kind-
 * agnostic); after a lock transition we refresh the NOTE lists (the note-folder
 * `locked` flag + the masked note rows come back through our own IPC).
 */
@Component({
  selector: "app-notes-home",
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { "(document:keydown.escape)": "onEscape()" },
  imports: [MurSidebarComponent, OrganizeSheetComponent],
  templateUrl: "./notes-home.component.html",
  styleUrl: "./notes-home.component.scss",
})
export class NotesHomeComponent implements OnInit {
  private readonly notes = inject(NotesService);
  private readonly folders = inject(FoldersService);
  private readonly router = inject(Router);
  private readonly toast = inject(ToastService);
  private readonly injector = inject(Injector);

  /** Drill-down back navigation ("← Murmur" + Esc). */
  readonly nav = inject(NavHistoryService);

  /** The note list from the store (gated — masked rows carry no snippet/tags). */
  readonly noteList = this.notes.notes;
  /** The note-kind folder list from the store. */
  readonly noteFolders = this.notes.noteFolders;
  /** True while the note list is (re)loading. */
  readonly loading = this.notes.loading;

  /** Selected note-folder id (null ⇒ all notes). Drives the content pane. */
  readonly activeFolderId = signal<string | null>(null);
  /** True while a create-note IPC call is in flight (guards the "New note" button). */
  readonly creating = signal(false);

  /** The active folder node (null for the "All notes" root). */
  readonly activeFolder = computed<NoteFolder | null>(() => {
    const fid = this.activeFolderId();
    return fid === null
      ? null
      : (this.noteFolders().find((f) => f.id === fid) ?? null);
  });

  /** The active folder's display name, or "All notes" for the null selection. */
  readonly listHeading = computed(() => this.activeFolder()?.name ?? "All notes");

  /** True when the active folder is sealed (drives the locked-folder banner). */
  readonly activeFolderLocked = computed(() => !!this.activeFolder()?.locked);

  // --- New-folder inline create -------------------------------------------
  /** True when the rail's "New folder" field is open. */
  readonly creatingFolder = signal(false);
  /** Draft folder name bound to the field. */
  readonly folderDraft = signal("");
  /** True while a folder create/rename/delete IPC call is in flight. */
  readonly folderBusy = signal(false);
  /** The new-folder name field — focused after it renders (afterNextRender). */
  private readonly folderInput =
    viewChild<ElementRef<HTMLInputElement>>("folderInput");

  // --- Rename inline ------------------------------------------------------
  /** Id of the folder being renamed inline (null = none). */
  readonly renamingId = signal<string | null>(null);
  /** Draft name bound to the rename field. */
  readonly renameDraft = signal("");

  // --- Delete confirm -----------------------------------------------------
  /** Id of the folder whose delete confirm is open (null = none). */
  readonly pendingDeleteId = signal<string | null>(null);

  // --- Per-note "Move to…" popover ----------------------------------------
  /** The note id whose move popover is open (null = none). */
  readonly movePopoverId = signal<string | null>(null);

  // --- Folder lock/unlock -------------------------------------------------
  /** Id of the folder whose lock/unlock op is in flight (guards double-clicks). */
  readonly lockBusyId = signal<string | null>(null);

  // --- Auto-organize ------------------------------------------------------
  /** The proposed organize plan under review (null = sheet closed). */
  readonly organizePlan = signal<OrganizePlan | null>(null);
  /** True while `plan_organize_notes` is being fetched (header button spinner). */
  readonly organizePlanning = signal(false);
  /** True while `apply_organize_plan` is in flight (sheet Apply spinner). */
  readonly organizeApplying = signal(false);
  /** True when the review sheet is showing (a plan has been fetched). */
  readonly organizeOpen = computed(() => this.organizePlan() !== null);

  async ngOnInit(): Promise<void> {
    // Load the note-folder rail + the (all-notes) list in parallel; settle each
    // independently so a folder-load failure never blanks the note list.
    await Promise.allSettled([
      this.notes.loadFolders(),
      this.notes.loadNotes(null),
    ]);
  }

  /** Esc backs out — but never mid-edit in a field, and it closes open transient UI first. */
  onEscape(): void {
    const tag = (document.activeElement as HTMLElement | null)?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") {
      return;
    }
    // Dismiss any open transient surface before backing out of the view.
    if (this.organizeOpen()) {
      this.closeOrganize();
      return;
    }
    if (this.movePopoverId() !== null) {
      this.movePopoverId.set(null);
      return;
    }
    if (this.creatingFolder() || this.renamingId() || this.pendingDeleteId()) {
      this.cancelFolderEdits();
      return;
    }
    this.nav.back();
  }

  /** Select a note-folder (or null for "All notes") and reload its notes. */
  async selectFolder(folderId: string | null): Promise<void> {
    this.movePopoverId.set(null);
    if (this.activeFolderId() === folderId) {
      return;
    }
    this.activeFolderId.set(folderId);
    await this.notes.loadNotes(folderId);
  }

  // --- Notes --------------------------------------------------------------

  /**
   * Create an empty note in the active folder and open its editor. `create`
   * rejects only on a real create failure (the follow-on list refresh is
   * swallowed) — on success we navigate straight to `/notes/:id`.
   */
  async newNote(): Promise<void> {
    if (this.creating()) {
      return;
    }
    this.creating.set(true);
    try {
      const id = await this.notes.create(this.activeFolderId(), "Untitled");
      await this.router.navigate(["/notes", id]);
    } catch {
      this.toast.danger("Couldn’t create the note. Please try again.");
    } finally {
      this.creating.set(false);
    }
  }

  /** Open a note in the editor (a masked/locked row instead routes to unlock). */
  openNote(id: string, locked: boolean): void {
    if (locked) {
      void this.unlockActiveOrFolder(this.activeFolderId());
      return;
    }
    void this.router.navigate(["/notes", id]);
  }

  // --- Per-note "Move to…" popover ----------------------------------------

  /** Open / close / toggle a note's folder picker (one open at a time). */
  toggleMovePopover(id: string, event: Event): void {
    event.stopPropagation();
    this.movePopoverId.update((cur) => (cur === id ? null : id));
  }
  closeMovePopover(): void {
    this.movePopoverId.set(null);
  }

  /**
   * Move a note into `folderId`, then reload the ACTIVE folder view so the moved
   * note leaves the current list at once. The folder rail counts self-heal on
   * the next folder load; we refresh both to stay coherent.
   */
  async moveNote(noteId: string, folderId: string): Promise<void> {
    this.closeMovePopover();
    try {
      await this.notes.move(noteId, folderId);
      // notes.move reloads ALL notes — re-apply the active-folder filter.
      await this.notes.loadNotes(this.activeFolderId());
      const name =
        this.noteFolders().find((f) => f.id === folderId)?.name ?? "folder";
      this.toast.success(`Moved to ${name}`);
    } catch {
      this.toast.danger("Couldn’t move this note. Please try again.");
    }
  }

  // --- Folder create / rename / delete ------------------------------------

  /** Open the inline "New folder" field and focus it (afterNextRender; no setTimeout). */
  startCreateFolder(): void {
    this.cancelFolderEdits();
    this.folderDraft.set("");
    this.creatingFolder.set(true);
    afterNextRender(() => this.folderInput()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

  /** Confirm the inline folder create (creates under the active folder as parent). */
  async confirmCreateFolder(event: Event): Promise<void> {
    event.preventDefault();
    const name = this.folderDraft().trim();
    if (!name || this.folderBusy()) {
      return;
    }
    this.folderBusy.set(true);
    try {
      const folder = await this.notes.createFolder(name, this.activeFolderId());
      this.creatingFolder.set(false);
      this.folderDraft.set("");
      // Jump into the just-created folder so it's obviously there.
      await this.selectFolder(folder.id);
    } catch {
      this.toast.danger("Couldn’t create the folder. Please try again.");
    } finally {
      this.folderBusy.set(false);
    }
  }

  /** Begin an inline rename of `folder`. */
  startRename(folder: NoteFolder, event: Event): void {
    event.stopPropagation();
    this.cancelFolderEdits();
    this.renamingId.set(folder.id);
    this.renameDraft.set(folder.name);
  }

  /** Confirm the inline rename. */
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

  /** Open the delete-confirm for `folder`. */
  askDeleteFolder(folder: NoteFolder, event: Event): void {
    event.stopPropagation();
    this.cancelFolderEdits();
    this.pendingDeleteId.set(folder.id);
  }

  /** Confirm folder delete: its notes reparent to the default folder. */
  async confirmDeleteFolder(id: string): Promise<void> {
    if (this.folderBusy()) {
      return;
    }
    this.folderBusy.set(true);
    try {
      await this.notes.deleteFolder(id);
      this.pendingDeleteId.set(null);
      // If the deleted folder was active, fall back to "All notes".
      if (this.activeFolderId() === id) {
        await this.selectFolder(null);
      }
    } catch {
      this.toast.danger("Couldn’t delete the folder. Please try again.");
    } finally {
      this.folderBusy.set(false);
    }
  }

  /** Cancel any open inline folder edit (create / rename / delete confirm). */
  cancelFolderEdits(): void {
    this.creatingFolder.set(false);
    this.renamingId.set(null);
    this.pendingDeleteId.set(null);
  }

  // --- Folder lock / unlock -----------------------------------------------

  /**
   * Lock a note-folder (seal its notes). Reuses the shared folder lock command
   * via `FoldersService`; on success we refresh the NOTE lists so the rail badge
   * + masked rows reflect the new sealed state.
   */
  async lockFolder(folder: NoteFolder, event: Event): Promise<void> {
    event.stopPropagation();
    if (this.lockBusyId() !== null) {
      return;
    }
    this.lockBusyId.set(folder.id);
    try {
      await this.folders.lock(folder.id);
      await this.refreshAfterLockChange();
      this.toast.success(`Locked “${folder.name}”`);
    } catch {
      this.toast.danger("Couldn’t lock this folder. Please try again.");
    } finally {
      this.lockBusyId.set(null);
    }
  }

  /**
   * Session-unlock a sealed note-folder (Touch ID via the shared folder command).
   * On success refresh the NOTE lists so the previously-masked notes appear.
   */
  async unlockFolder(folder: NoteFolder, event: Event): Promise<void> {
    event.stopPropagation();
    await this.unlockActiveOrFolder(folder.id);
  }

  /**
   * Shared unlock path (used by the folder button AND by clicking a masked note
   * row). A no-op for the null/"All notes" selection. Refreshes the note lists.
   */
  private async unlockActiveOrFolder(folderId: string | null): Promise<void> {
    if (folderId === null || this.lockBusyId() !== null) {
      return;
    }
    this.lockBusyId.set(folderId);
    try {
      await this.folders.unlock(folderId);
      await this.refreshAfterLockChange();
    } catch {
      // A cancelled/denied Touch ID prompt — stay masked, no scary toast.
      this.toast.danger("Couldn’t unlock this folder.");
    } finally {
      this.lockBusyId.set(null);
    }
  }

  /** Refresh the folder rail + the active note list after a lock transition. */
  private async refreshAfterLockChange(): Promise<void> {
    await Promise.allSettled([
      this.notes.loadFolders(),
      this.notes.loadNotes(this.activeFolderId()),
    ]);
  }

  // --- Auto-organize ------------------------------------------------------

  /**
   * Fetch a proposed organize plan for the active scope (null ⇒ all notes) and
   * open the review sheet. The sheet renders even for an empty plan ("already
   * organized"). A fetch failure surfaces a toast and leaves the sheet closed.
   */
  async startOrganize(): Promise<void> {
    if (this.organizePlanning() || this.organizeOpen()) {
      return;
    }
    this.organizePlanning.set(true);
    try {
      const plan = await this.notes.planOrganize(this.activeFolderId());
      this.organizePlan.set(plan);
    } catch {
      this.toast.danger("Couldn’t plan an auto-organize. Please try again.");
    } finally {
      this.organizePlanning.set(false);
    }
  }

  /** Apply the user-selected moves, then refresh + confirm. */
  async applyOrganize(moves: OrganizeMove[]): Promise<void> {
    if (this.organizeApplying()) {
      return;
    }
    this.organizeApplying.set(true);
    try {
      await this.notes.applyOrganize(moves);
      // Re-apply the active-folder filter after the store's all-notes reload.
      await this.notes.loadNotes(this.activeFolderId());
      this.organizePlan.set(null);
      const n = moves.length;
      this.toast.success(`Moved ${n} ${n === 1 ? "note" : "notes"}`);
    } catch {
      this.toast.danger("Couldn’t apply the plan. Please try again.");
    } finally {
      this.organizeApplying.set(false);
    }
  }

  /** Close the organize review sheet without applying. */
  closeOrganize(): void {
    if (this.organizeApplying()) {
      return;
    }
    this.organizePlan.set(null);
  }

  // --- Presentational -----------------------------------------------------

  /** Presentational only: epoch-ms → a friendly local date. */
  formatDate(updatedAt: number): string {
    const d = new Date(updatedAt);
    if (Number.isNaN(d.getTime())) {
      return "";
    }
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  }
}
