import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  Injector,
  OnInit,
  afterNextRender,
  computed,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { Router, RouterLink } from "@angular/router";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "../../../core/ipc.service";
import { NavHistoryService } from "../../../core/nav-history.service";
import type {
  NoteFolder,
  NoteSummary,
  OrganizeMove,
  OrganizePlan,
  OrgItemHeader,
  OrgStatus,
} from "../../../core/models";
import { MurSidebarComponent } from "../../../design-system/sidebar/sidebar.component";
import { FoldersService } from "../../../services/folders.service";
import { FolderLockFlowService } from "../../../services/folder-lock-flow.service";
import { NotesService } from "../../../services/notes.service";
import { ToastService } from "../../../services/toast.service";
import { LockSharesDialogComponent } from "../../folders/lock-shares-dialog/lock-shares-dialog.component";
import { OrganizeSheetComponent } from "../organize-sheet/organize-sheet.component";

/**
 * One row of the unified content list — a discriminated union over the two
 * sources the pane merges. A `"note"` card is YOUR authored note (opens the
 * editor); an `"org"` card is a READ-ONLY org (Shared Brain) replica (opens the
 * `/org-item/:id` viewer, carries the origin org's name for its badge). Both
 * expose an epoch-ms `sortAt` so the merged list orders by date desc regardless
 * of source. `id` is namespaced (`note:`/`org:`) so the `@for` track key is
 * stable + collision-free across the two id spaces.
 */
export type NotesListItem =
  | {
      kind: "note";
      id: string;
      sortAt: number;
      note: NoteSummary;
    }
  | {
      kind: "org";
      id: string;
      sortAt: number;
      item: OrgItemHeader;
      /** The origin org's display name (drives the "shared brain" badge label). */
      orgName: string;
    };

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
  imports: [
    RouterLink,
    MurSidebarComponent,
    OrganizeSheetComponent,
    LockSharesDialogComponent,
  ],
  templateUrl: "./notes-home.component.html",
  styleUrl: "./notes-home.component.scss",
})
export class NotesHomeComponent implements OnInit {
  private readonly notes = inject(NotesService);
  private readonly folders = inject(FoldersService);
  private readonly ipc = inject(IpcService);
  private readonly router = inject(Router);
  private readonly toast = inject(ToastService);
  private readonly injector = inject(Injector);
  private readonly destroyRef = inject(DestroyRef);
  /**
   * Shared lock×shares flow (probe → warn/revoke dialog → lock). The SAME flow the
   * meetings tree runs, so locking a note-folder with live shares also warns before
   * sealing (PK-F1). Public so the template can bind the dialog + its actions.
   */
  readonly lockFlow = inject(FolderLockFlowService);

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

  // --- Org (Shared Brain) shared-brain notes ------------------------------
  /**
   * Every org (Shared Brain) this user belongs to — the rail's "Shared brains"
   * section + the source of merged org items in "All notes". Loaded stale-guarded
   * on init, on the `org-feed-updated` event, and on window focus.
   */
  private readonly _orgs = signal<OrgStatus[]>([]);
  readonly orgs = this._orgs.asReadonly();
  /**
   * The org's shared items keyed by orgId (`listOrgItems`). A flat parallel signal
   * (not per-org sub-signals) so the merged `listItems` computed re-derives on any
   * change. Populated alongside {@link _orgs} in {@link loadOrgs}.
   */
  private readonly _orgItems = signal<Record<string, OrgItemHeader[]>>({});
  /**
   * Selected org id in the rail (null ⇒ not viewing a specific org). MUTUALLY
   * exclusive with a note-folder selection: selecting an org clears
   * {@link activeFolderId} back to the "All notes" root and vice-versa, so the
   * content pane has exactly one active scope.
   */
  readonly activeOrgId = signal<string | null>(null);
  /** True while the org list + items are (re)loading (rail hint only). */
  readonly orgsLoading = signal(false);
  /** The org whose items are shown when an org is rail-selected (null otherwise). */
  readonly activeOrg = computed<OrgStatus | null>(() => {
    const oid = this.activeOrgId();
    return oid === null
      ? null
      : (this._orgs().find((o) => o.orgId === oid) ?? null);
  });

  /**
   * The unified content list feeding the pane, sorted by date desc:
   *  - a specific org rail-selected ⇒ ONLY that org's items;
   *  - "All notes" (no folder, no org) ⇒ your authored notes MERGED with EVERY
   *    org's shared items;
   *  - a specific note-folder ⇒ only that folder's authored notes (no org items).
   * Org items never carry a lock (they are deliberately-disclosed org content) —
   * only authored notes can be masked.
   */
  readonly listItems = computed<NotesListItem[]>(() => {
    const orgId = this.activeOrgId();
    const orgItemsByOrg = this._orgItems();
    const orgs = this._orgs();
    const orgNameById = new Map(orgs.map((o) => [o.orgId, o.name]));

    // A specific org selected: show ONLY that org's items.
    if (orgId !== null) {
      const name = orgNameById.get(orgId) ?? "";
      return (orgItemsByOrg[orgId] ?? [])
        .map((item) => this.toOrgCard(item, name))
        .sort((a, b) => b.sortAt - a.sortAt);
    }

    const noteCards: NotesListItem[] = this.noteList().map((note) => ({
      kind: "note" as const,
      id: `note:${note.id}`,
      sortAt: note.updatedAt,
      note,
    }));

    // A specific note-folder is selected (not "All notes"): authored notes only.
    if (this.activeFolderId() !== null) {
      return noteCards.sort((a, b) => b.sortAt - a.sortAt);
    }

    // "All notes": merge YOUR notes with EVERY org's shared items.
    const orgCards: NotesListItem[] = orgs.flatMap((o) =>
      (orgItemsByOrg[o.orgId] ?? []).map((item) =>
        this.toOrgCard(item, o.name),
      ),
    );
    return [...noteCards, ...orgCards].sort((a, b) => b.sortAt - a.sortAt);
  });

  /** True when the unified list has zero rows (drives the empty state). */
  readonly listEmpty = computed(() => this.listItems().length === 0);

  /** Build an org card from a header + its origin org name. */
  private toOrgCard(item: OrgItemHeader, orgName: string): NotesListItem {
    const t = Date.parse(item.createdAt);
    return {
      kind: "org",
      id: `org:${item.itemId}`,
      sortAt: Number.isNaN(t) ? 0 : t,
      item,
      orgName,
    };
  }

  /** Released on destroy to detach the org-feed-updated live-refresh listener. */
  private orgFeedUnlisten: UnlistenFn | null = null;
  /** True once destroyed — so a `listen()` that resolves AFTER teardown releases immediately
   * (distinct from `orgFeedUnlisten === null`, which also means "not yet resolved"). */
  private orgFeedDestroyed = false;
  /** Bumped per org load so a late (stale) reload result is dropped (T1 guard). */
  private orgLoadSeq = 0;
  /** Bound window-focus handler — re-loads org items when the view regains focus. */
  private readonly onWindowFocus = (): void => {
    void this.loadOrgs();
  };

  /** The active folder node (null for the "All notes" root). */
  readonly activeFolder = computed<NoteFolder | null>(() => {
    const fid = this.activeFolderId();
    return fid === null
      ? null
      : (this.noteFolders().find((f) => f.id === fid) ?? null);
  });

  /**
   * The content-pane heading: the active org's name when an org is selected, else
   * the active note-folder's name, else "All notes" for the root selection.
   */
  readonly listHeading = computed(
    () => this.activeOrg()?.name ?? this.activeFolder()?.name ?? "All notes",
  );

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
    // Live-refresh: the background org-sync loop fires `org-feed-updated` on a
    // productive tick (≥1 ingest/tombstone). Subscribe ONCE (push straight into a
    // reload — NEVER subscribe-into-a-field), and re-load org items when the view
    // regains focus. Both cleaned up on destroy (release the UnlistenFn + the
    // focus handler).
    this.destroyRef.onDestroy(() => {
      this.orgFeedDestroyed = true;
      this.orgFeedUnlisten?.();
      this.orgFeedUnlisten = null;
      window.removeEventListener("focus", this.onWindowFocus);
    });
    window.addEventListener("focus", this.onWindowFocus);
    void this.ipc
      .onOrgFeedUpdated(() => void this.loadOrgs())
      .then((un) => {
        // If the view was already torn down before the listener resolved, release
        // it immediately (never leak a subscription past destroy).
        if (this.orgFeedDestroyed) {
          un();
        } else {
          this.orgFeedUnlisten = un;
        }
      })
      .catch(() => {
        /* best-effort: no Tauri host (e.g. plain browser) → no live refresh */
      });

    // Load the note-folder rail + the (all-notes) list + the org list in parallel;
    // settle each independently so one load's failure never blanks the others.
    await Promise.allSettled([
      this.notes.loadFolders(),
      this.notes.loadNotes(null),
      this.loadOrgs(),
    ]);
  }

  /**
   * (Re)load the org (Shared Brain) list + every org's shared items, stale-guarded
   * on {@link orgLoadSeq} so a late reload (event / focus / init racing) never
   * overwrites a newer result. Best-effort throughout: `orgRefresh` (server
   * membership discovery) and each per-org `listOrgItems` swallow their own
   * failures so a transient/offline error leaves the last-known list standing
   * rather than blanking the pane. Never throws.
   */
  async loadOrgs(): Promise<void> {
    const seq = ++this.orgLoadSeq;
    this.orgsLoading.set(true);
    try {
      // Discover freshly-invited orgs before reading the local replica (best-effort).
      try {
        await this.ipc.orgRefresh();
      } catch {
        /* offline / no server → fall through to the local replica */
      }
      let orgs: OrgStatus[];
      try {
        orgs = await this.ipc.orgListStatuses();
      } catch {
        return; // keep the last-known orgs on a transient failure
      }
      if (seq !== this.orgLoadSeq) {
        return; // a newer load superseded this one — drop the result
      }
      // Pull each org's browsable items in parallel; a per-org failure yields [].
      const itemLists = await Promise.all(
        orgs.map((o) =>
          this.ipc.listOrgItems(o.orgId).catch(() => [] as OrgItemHeader[]),
        ),
      );
      if (seq !== this.orgLoadSeq) {
        return;
      }
      const byOrg: Record<string, OrgItemHeader[]> = {};
      orgs.forEach((o, i) => {
        byOrg[o.orgId] = itemLists[i];
      });
      this._orgs.set(orgs);
      this._orgItems.set(byOrg);
      // If the rail's selected org has since disappeared, fall back to "All notes".
      const sel = this.activeOrgId();
      if (sel !== null && !orgs.some((o) => o.orgId === sel)) {
        this.activeOrgId.set(null);
      }
    } finally {
      if (seq === this.orgLoadSeq) {
        this.orgsLoading.set(false);
      }
    }
  }

  /**
   * Rail-select an org (Shared Brain): the pane shows ONLY that org's shared
   * items. Mutually exclusive with a note-folder selection — clears
   * {@link activeFolderId} back to the root + closes any open move popover.
   */
  selectOrg(orgId: string): void {
    this.movePopoverId.set(null);
    this.activeFolderId.set(null);
    this.activeOrgId.set(orgId);
  }

  /** A friendly role hint for the rail ("Owner" / "Member"). */
  orgRoleLabel(org: OrgStatus): string {
    return org.role === "owner" ? "Owner" : "Member";
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

  /**
   * Select a note-folder (or null for "All notes") and reload its notes. Clears
   * any active org selection (the two rail scopes are mutually exclusive). A
   * no-op re-select of the SAME folder while no org is active is skipped — but a
   * re-select of "All notes" WHILE an org is active still runs, to drop the org.
   */
  async selectFolder(folderId: string | null): Promise<void> {
    this.movePopoverId.set(null);
    if (this.activeFolderId() === folderId && this.activeOrgId() === null) {
      return;
    }
    this.activeOrgId.set(null);
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
   * Lock a note-folder (seal its notes) THROUGH the shared lock×shares flow (PK-F1):
   * the flow FIRST probes `folder_active_shares` and — if the folder still has live
   * shares, or the probe itself fails (FAIL-CLOSED, F5) — opens the warn/revoke dialog
   * instead of sealing straight away. Previously this called `FoldersService.lock`
   * directly and BYPASSED that dialog, so a shared note-folder could be sealed without
   * the owner deciding what happens to the outstanding shares. The `onLocked` callback
   * refreshes the NOTE lists once a lock actually lands (from any path) so the rail
   * badge + masked rows reflect the new sealed state.
   */
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

  /** Presentational only: an ISO timestamp (org item `createdAt`) → a friendly date. */
  formatOrgDate(iso: string): string {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) {
      return "";
    }
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  }
}
