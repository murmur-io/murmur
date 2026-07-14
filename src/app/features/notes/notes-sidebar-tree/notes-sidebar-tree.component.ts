import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  inject,
  input,
  signal,
  viewChild,
} from "@angular/core";
import { Router } from "@angular/router";
import type { NoteFolder } from "../../../core/models";
import { MurTreeRowComponent } from "../../../design-system/tree-row/tree-row.component";
import { MurRowMenuComponent } from "../../../design-system/row-menu/row-menu.component";
import { FolderLockFlowService } from "../../../services/folder-lock-flow.service";
import {
  FoldersService,
  type FolderExposure,
} from "../../../services/folders.service";
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
 *
 * TRAILING ACTIONS (2026-07-12, unification round 2): Rename + Delete live
 * behind the shared `<mur-row-menu>` gear dropdown (design-system,
 * `row-menu.component.ts`) — the SAME component instance type
 * `FolderRowComponent` (Meetings) renders, so the two trees' dropdowns are
 * guaranteed identical rather than two hand-copied lookalikes (see that
 * component's class doc for the full history of why "measured the same size"
 * turned out not to mean "looks the same"). Lock/unlock stays its own small
 * icon in the cluster, NOT folded into the dropdown — this tree never treated
 * lock as a glanceable-without-hovering state badge in the first place
 * (unlike Meetings' `.lock-toggle`), so there's nothing lost by keeping it
 * separate, and both dropdowns end up with the exact same two items (Rename,
 * Delete) in the exact same order.
 */
@Component({
  selector: "app-notes-sidebar-tree",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurTreeRowComponent, MurRowMenuComponent],
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

  /**
   * The folders the tree RENDERS — everything except the reserved always-open
   * root (2026-07-14): the root IS the "Notes" section itself (where unfiled new
   * notes live), so showing it as a nested "Notes" row would just re-create the
   * confusing "Notes-inside-Notes" redundancy the root was introduced to kill.
   */
  readonly visibleFolders = computed(() =>
    this.noteFolders().filter((f) => !f.isRoot),
  );
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

  /**
   * A note-folder's three-state privacy exposure (open / locked / session),
   * derived from its lock flags — same model the Meetings tree uses. `session`
   * (sealed on disk but decrypted for this session) is the only state that stays
   * glanceable + accent-tinted; open/locked are quiet. Needs `NoteFolder.unlocked`
   * (2026-07-14) — before that this tree only knew locked/open and could never
   * show that a folder was session-unlocked.
   */
  exposureOf(folder: NoteFolder): FolderExposure {
    if (!folder.locked) {
      return "open";
    }
    return folder.unlocked ? "session" : "locked";
  }

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

  /**
   * Re-seal a session-unlocked note-folder right now (drops the decrypted
   * plaintext for this session; the `.enc`/blobs stay). Mirrors the Meetings
   * tree's per-row re-seal — the action behind the `session` exposure state.
   */
  async relockFolder(folder: NoteFolder, event: Event): Promise<void> {
    event.stopPropagation();
    if (this.lockBusyId() !== null) {
      return;
    }
    this.lockBusyId.set(folder.id);
    try {
      await this.folders.relock(folder.id);
      await this.refreshAfterLockChange();
    } catch {
      this.toast.danger("Couldn’t re-seal this folder. Please try again.");
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
