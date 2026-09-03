import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  OnInit,
  computed,
  effect,
  inject,
  signal,
  untracked,
} from "@angular/core";
import { Router, RouterLink } from "@angular/router";
import { TabsService } from "../../../core/tabs.service";
import type {
  NoteFolder,
  NotesListItem,
  OrganizeFailure,
  OrganizeMove,
  OrganizePlan,
  OrgItemHeader,
  OrgStatus,
} from "../../../core/models";
import { MurTableColumnComponent } from "../../../design-system/table/table-column.component";
import { MurTableComponent } from "../../../design-system/table/table.component";
import { MurSpinnerComponent } from "../../../design-system/spinner/spinner.component";
import { FoldersService } from "../../../services/folders.service";
import { NotesService } from "../../../services/notes.service";
import { NotesSavedViewsService } from "../../../services/notes-saved-views.service";
import { NotesViewEngine } from "../../../services/notes-view-engine";
import { OrgBrainService } from "../../../services/org-brain.service";
import { ToastService } from "../../../services/toast.service";
import {
  OrganizeSheetComponent,
  type OrganizeAttemptReceipt,
  type OrganizeViewPlan,
} from "../organize-sheet/organize-sheet.component";
import { NotesViewSwitcherComponent } from "../notes-view-switcher/notes-view-switcher.component";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";
import { AskHistoryPrivacyBarrierService } from "../../../core/ask-history-privacy-barrier.service";
import { DateFormatService } from "../../../core/date-format.service";

const MAX_ORGANIZE_FAILURE_REASON_LENGTH = 240;

/**
 * The Notes landing view — NOW a normal in-flow route beside the ALWAYS-VISIBLE
 * main sidebar (changed 2026-07-12, Notion/Obsidian-style navigation), not a
 * drill-down: it flows in `.app-main` next to `<mur-sidebar>` exactly like
 * `/record`. The note-folder tree (create / rename / delete / lock / unlock)
 * now lives IN the main sidebar (`NotesSidebarTreeComponent`, nested under the
 * "Notes" nav row) — this component owns only the content pane: the note cards
 * for the shared {@link NotesService.activeFolderId} scope, a "New note"
 * action, a per-note "Move to…" menu, an "Auto-organize" flow, and the
 * Shared-Brain org picker (a content-pane chip row, since orgs are not
 * note-folders and don't belong in the folder tree).
 *
 * Folder LOCK/UNLOCK for the CONTENT PANE's lock gate reuses the existing
 * folder lock lifecycle (`FoldersService` owns the biometric IPC); after an
 * unlock we refresh the NOTE lists (the note-folder `locked` flag + the masked
 * note rows come back through our own IPC). Locking itself now only happens
 * from the sidebar tree.
 */
@Component({
  selector: "app-notes-home",
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { "(document:keydown.escape)": "onEscape()" },
  imports: [
    RouterLink,
    MurSpinnerComponent,
    OrganizeSheetComponent,
    MurTableComponent,
    MurTableColumnComponent,
    NotesViewSwitcherComponent,
  ],
  templateUrl: "./notes-home.component.html",
  styleUrl: "./notes-home.component.scss",
})
export class NotesHomeComponent implements OnInit {
  private readonly dates = inject(DateFormatService);

  private readonly notes = inject(NotesService);
  private readonly notesSavedViews = inject(NotesSavedViewsService);
  private readonly orgBrain = inject(OrgBrainService);
  private readonly folders = inject(FoldersService);
  private readonly router = inject(Router);
  private readonly toast = inject(ToastService);
  private readonly tabsService = inject(TabsService);
  private readonly errorCopy = inject(ErrorCopyService);
  private readonly privacyBarrier = inject(AskHistoryPrivacyBarrierService);
  private readonly destroyRef = inject(DestroyRef);

  /** The note list from the store (gated — masked rows carry no snippet/tags). */
  readonly noteList = this.notes.notes;
  /** The note-kind folder list from the store (Move-to-folder menu + breadcrumb). */
  readonly noteFolders = this.notes.noteFolders;
  /** True while the note list is (re)loading. */
  readonly loading = this.notes.loading;

  /**
   * Selected note-folder id (null ⇒ all notes) — SHARED with the main sidebar's
   * `NotesSidebarTreeComponent` via {@link NotesService.activeFolderId}. This
   * component reads it; only the sidebar tree calls `notes.selectFolder`.
   */
  readonly activeFolderId = this.notes.activeFolderId;
  /** True while a create-note IPC call is in flight (guards the "New note" button). */
  readonly creating = signal(false);

  // --- Org (Shared Brain) shared-brain notes ------------------------------
  // The RAW org roster/items/loading state lives in the shared, root-persisted
  // OrgBrainService now (was: a component-local signal wiped to empty on every
  // destroy+recreate — the stale-while-revalidate fix, 2026-07-12). This
  // component keeps only its OWN derived view + chip-row selection.
  /** Every org (Shared Brain) this user belongs to — the content pane's "Shared brains" chip row. */
  readonly orgs = this.orgBrain.orgs;
  /** True while the org list + items are (re)loading (chip-row hint only — never a render gate). */
  readonly orgsLoading = this.orgBrain.loading;
  /**
   * Selected org id (null ⇒ not viewing a specific org), chosen via the content
   * pane's "Shared brains" chip row. Exiting to a DIFFERENT note-folder (picked
   * in the main sidebar's tree) clears this back to null — see
   * {@link _clearOrgOnFolderChange} — so the content pane always has exactly
   * one active scope.
   */
  readonly activeOrgId = signal<string | null>(null);
  /** The org whose items are shown when an org chip is selected (null otherwise). */
  readonly activeOrg = computed<OrgStatus | null>(() => {
    const oid = this.activeOrgId();
    return oid === null
      ? null
      : (this.orgBrain.orgs().find((o) => o.orgId === oid) ?? null);
  });
  /**
   * If the chip row's selected org has since disappeared (left/no longer
   * enabled here), fall back to "All notes" — mirrors LibraryComponent's
   * `_clearMissingActiveOrg`; reactive over the SHARED roster since a load can
   * land from either this view or Library's.
   */
  private readonly _clearMissingActiveOrg = effect(() => {
    const orgs = this.orgBrain.orgs();
    const sel = this.activeOrgId();
    if (sel !== null && !orgs.some((o) => o.orgId === sel)) {
      untracked(() => this.activeOrgId.set(null));
    }
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
    const orgItemsByOrg = this.orgBrain.orgItems();
    const orgs = this.orgBrain.orgs();
    const orgNameById = new Map(orgs.map((o) => [o.orgId, o.name]));

    // A specific org selected: show ONLY that org's items — EXCLUDING `kind === "meeting"`
    // ones ("notes has notes, meetings has meetings": a shared meeting note belongs in the
    // Library "Shared brains" rail, not here). An unclassified item (`kind` absent/`null` —
    // shared under the pre-v2 wire format, before source-kind existed) stays visible here,
    // same as it always has, since it can't be proven to be a meeting.
    if (orgId !== null) {
      const name = orgNameById.get(orgId) ?? "";
      return (orgItemsByOrg[orgId] ?? [])
        .filter((item) => item.kind !== "meeting")
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

    // "All notes": merge YOUR notes with EVERY org's shared items — but DROP org
    // replicas the caller authored (`ownedSource`): the editable original already
    // appears (as a note card, or under Meetings), so showing the replica too would
    // duplicate the row and surface a stale publish-time title. Replicas shared by
    // OTHERS stay (that's the point of the merged view). ALSO drop `kind === "meeting"`
    // items — those now live exclusively in the Library "Shared brains" rail ("notes has
    // notes, meetings has meetings"); an unclassified item (no `kind`, pre-v2 wire format)
    // stays here, matching its historical behavior.
    const orgCards: NotesListItem[] = orgs.flatMap((o) =>
      (orgItemsByOrg[o.orgId] ?? [])
        .filter((item) => !item.ownedSource && item.kind !== "meeting")
        .map((item) => this.toOrgCard(item, o.name)),
    );
    return [...noteCards, ...orgCards].sort((a, b) => b.sortAt - a.sortAt);
  });

  /** True when the unified list has zero rows (drives the empty state). */
  readonly listEmpty = computed(() => this.listItems().length === 0);

  /** `<mur-table>`'s required track key — the union's own namespaced `id`. */
  readonly trackByItemId = (row: NotesListItem): string => row.id;

  /**
   * `<mur-table>`'s per-row class hook: `is-muted` dims a masked/locked note
   * row (the table renders it, so notes-home's own scoped CSS can't reach the
   * `<tr>` directly — see `MurTableComponent`'s class doc); `is-menu-open`
   * raises the row whose "Move to…" popover is open above the click-away
   * scrim (both class names are `<mur-table>`'s own small reusable vocabulary).
   */
  readonly rowClassFor = (row: NotesListItem): Record<string, boolean> => ({
    "is-muted": row.kind === "note" && row.note.locked,
    "is-menu-open": row.kind === "note" && this.movePopoverId() === row.note.id,
  });

  /**
   * Open an org card as a tracked tab. When the caller AUTHORED the item
   * (`ownedSource`, resolved+gated backend-side) it opens the EDITABLE
   * original — a note or meeting tab — so the author edits the real thing
   * instead of a read-only replica; otherwise it opens the read-only
   * `/org-item/:id` viewer tab. Live-found bug, 2026-07-12: this used to be a
   * plain `[routerLink]`, so unlike an owned note/meeting it never registered
   * with {@link TabsService} — it opened but never appeared in the tab strip
   * and couldn't be switched back to. Routes through the SAME open* call an
   * owned note/meeting card would use.
   */
  openOrgCard(item: OrgItemHeader): void {
    const owned = item.ownedSource;
    if (owned) {
      if (owned.kind === "meeting") {
        void this.tabsService.openMeeting(owned.id, item.title || "Meeting");
      } else {
        void this.tabsService.openNote(owned.id, item.title || "Note");
      }
      return;
    }
    void this.tabsService.openOrgItem(item.itemId, item.title || "Shared note");
  }

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

  /**
   * True when the active folder is sealed AND not session-unlocked (drives the
   * locked-folder gate). Gating on `locked && !unlocked` — not `locked` alone —
   * is what makes "Unlock folder" actually lift the gate: a session-unlock never
   * flips the DB `locked` column, only the session `unlocked` flag, so the old
   * `!!locked` check kept the gate up forever after unlocking (2026-07-14 fix).
   */
  readonly activeFolderLocked = computed(() => {
    const f = this.activeFolder();
    return !!f?.locked && !f?.unlocked;
  });

  /**
   * The organizer's scoped backend reader requires a raw-open folder. A
   * session unlock is sufficient for ordinary gated reads, but deliberately
   * not for this AI classification path. Shared Brain rows are a different
   * dataset entirely and cannot be planned through `activeFolderId`.
   */
  readonly canOrganizeActiveScope = computed(() => {
    if (this.activeOrgId() !== null) {
      return false;
    }
    const folderId = this.activeFolderId();
    if (folderId === null) {
      return true;
    }
    const folder = this.activeFolder();
    return folder !== null && !folder.locked;
  });

  // --- Saved Views (mirrors Meetings; ported 2026-07-14) -------------------
  /** The active NOTES saved view (null ⇒ the plain List default). */
  readonly activeSavedView = this.notesSavedViews.activeView;

  /**
   * Resolve a note row's display FOLDER name (org rows have none). Injected into
   * the {@link NotesViewEngine} so the pure engine doesn't need the folder tree.
   * `this`-bound (an arrow field) since the engine calls it detached.
   */
  readonly folderNameFn = (item: NotesListItem): string | null => {
    if (item.kind !== "note") {
      return null;
    }
    return (
      this.noteFolders().find((f) => f.id === item.note.folderId)?.name ?? null
    );
  };

  /**
   * The rows the pane actually renders: the SAME already-gated {@link listItems}
   * union, filtered + sorted by the active saved view's config (via the pure
   * {@link NotesViewEngine}). With no active view it's `listItems()` verbatim —
   * so "List" is the unchanged default and a saved view is just a named
   * filter/sort preset over it (a masked/locked note stays masked in every view;
   * the engine only re-reads the fields the row already carries).
   */
  readonly viewItems = computed<NotesListItem[]>(() => {
    const view = this.activeSavedView();
    const items = this.listItems();
    // A saved view applies ONLY in the note-folder / All-notes scope — the SAME
    // scope the switcher is shown in (`activeOrgId() === null`). In an org
    // (Shared Brain) scope the switcher is hidden, so applying a persisted view
    // there would silently filter the org's items out with no way to clear it
    // (found by adversarial review 2026-07-14). Org rows always render raw.
    if (!view || this.activeOrgId() !== null) {
      return items;
    }
    return NotesViewEngine.rows(
      items,
      this.notesSavedViews.configOf(view),
      this.folderNameFn,
    );
  });

  /**
   * True when an active saved view's filter removed EVERY row (but the raw list
   * had some) — drives a "no notes match this view" hint distinct from the plain
   * "no notes yet" empty state.
   */
  readonly viewEmpty = computed(
    () => !this.listEmpty() && this.viewItems().length === 0,
  );

  /** Re-derive on any switcher change (select/config) — a no-op hook; `viewItems` is reactive. */
  onViewChanged(): void {
    this.movePopoverId.set(null);
  }

  // --- Per-note "Move to…" popover ----------------------------------------
  /** The note id whose move popover is open (null = none). */
  readonly movePopoverId = signal<string | null>(null);

  // --- Folder unlock (lock-gate CTA only — locking itself lives in the
  // sidebar tree now) --------------------------------------------------------
  /** Id of the folder whose unlock op is in flight (guards double-clicks). */
  readonly lockBusyId = signal<string | null>(null);

  // --- Auto-organize ------------------------------------------------------
  /** The proposed organize plan under review (null = sheet closed). */
  readonly organizePlan = signal<OrganizeViewPlan | null>(null);
  /** True while `plan_organize_notes` is being fetched (header button spinner). */
  readonly organizePlanning = signal(false);
  /** True while `apply_organize_plan` is in flight (sheet Apply spinner). */
  readonly organizeApplying = signal(false);
  /** True when the review sheet is showing (a plan has been fetched). */
  readonly organizeOpen = computed(() => this.organizePlan() !== null);
  /** Invalidates a late planner response after close or a newer request. */
  private organizePlanGeneration = 0;
  /** `undefined` means no active review; `null` is the valid global Notes scope. */
  private organizeScopeFolderId: string | null | undefined;

  /**
   * A pending response belongs to the exact folder scope it started in. Drop
   * it synchronously when the shared sidebar selects another folder, before
   * any old content-bearing plan can render in the new scope.
   */
  private readonly _scrubOrganizerOnFolderChange = effect(() => {
    const folderId = this.activeFolderId();
    untracked(() => {
      if (
        this.organizeScopeFolderId !== undefined &&
        this.organizeScopeFolderId !== folderId
      ) {
        this.scrubOrganizeReview();
      }
    });
  });

  constructor() {
    // Register before any organizer read. Tauri privacy events are not replayed,
    // so a relock in the listener gap could otherwise leave cached note titles,
    // destinations, reasons, or guidance visible in this mounted WebView.
    const unregister = this.privacyBarrier.registerInvalidator(() =>
      this.scrubOrganizeReview(),
    );
    this.destroyRef.onDestroy(() => {
      unregister();
      this.scrubOrganizeReview();
    });
  }

  /**
   * Picking a DIFFERENT note-folder in the main sidebar's tree exits any active
   * org view, so the content pane always shows exactly one scope (mirrors the
   * old rail's mutual-exclusion, now split across two components). Legitimate
   * signal-writing effect (T1) — no async orchestration, just a cross-component
   * UI-state sync driven by the shared {@link NotesService.activeFolderId}.
   */
  private readonly _clearOrgOnFolderChange = effect(() => {
    this.activeFolderId();
    this.activeOrgId.set(null);
  });

  async ngOnInit(): Promise<void> {
    // Live-refresh (org-feed-updated + window-focus) is subscribed ONCE, for the
    // app's lifetime, by the shared OrgBrainService now — no per-mount wiring here.

    // Load the note-folder list (Move-to-folder menu + breadcrumb — the sidebar
    // tree also loads it independently, but this component may mount first),
    // the note list for the CURRENT shared scope (persists across navigations —
    // the sidebar may have already selected a folder), and the org list, all in
    // parallel; settle each independently so one load's failure never blanks
    // the others.
    await Promise.allSettled([
      this.notes.loadFolders(),
      this.notes.loadNotes(this.notes.activeFolderId()),
      this.loadOrgs(),
      // The saved-view roster is root-persisted (survives the list route's
      // remount, §8) — a reload here just refreshes it.
      this.notesSavedViews.load(),
    ]);
  }

  /** (Re)load the org roster + items — delegates to the shared {@link OrgBrainService}. */
  async loadOrgs(): Promise<void> {
    await this.orgBrain.loadOrgs();
  }

  /** A friendly role hint for the chip row ("Owner" / "Member"). */
  orgRoleLabel(org: OrgStatus): string {
    return org.role === "owner" ? "Owner" : "Member";
  }

  /**
   * Esc closes open transient UI first (the organize sheet, a move popover) —
   * never mid-edit in a field. `/notes` is no longer a drill-down, so there is
   * no "back to Murmur" fallback here anymore; the persistent sidebar IS the
   * way back.
   */
  onEscape(): void {
    const tag = (document.activeElement as HTMLElement | null)?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") {
      return;
    }
    if (this.organizeOpen()) {
      this.closeOrganize();
      return;
    }
    if (this.movePopoverId() !== null) {
      this.movePopoverId.set(null);
    }
  }

  /** Select an org (chip row) — its shared items become the sole content-pane scope. */
  selectOrg(orgId: string): void {
    this.movePopoverId.set(null);
    this.scrubOrganizeReview();
    this.activeOrgId.set(orgId);
  }

  /** Clear the org selection back to the current note-folder scope. */
  clearOrg(): void {
    this.scrubOrganizeReview();
    this.activeOrgId.set(null);
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
    } catch (e) {
      // A sealed target folder (the selected one, or the default "Notes" folder if it's locked)
      // refuses the write with `[folder-locked]`. Say WHY — the old generic "couldn't create" hid
      // the real cause, so a user whose default folder was locked just saw an unexplained failure
      // (2026-07-14). P3 keeps that behaviour but keys it on the CODE rather than on `/locked/i`
      // over the raw string, which also matched unrelated messages containing the word.
      this.toast.danger(
        this.errorCopy.is(e, "folder-locked")
          ? "This folder is locked — unlock it first to add a note."
          : "Couldn’t create the note. Please try again.",
      );
    } finally {
      this.creating.set(false);
    }
  }

  /** Open a note in the editor (a masked/locked row instead routes to unlock). */
  openNote(id: string, locked: boolean, title?: string | null): void {
    if (locked) {
      void this.unlockActiveOrFolder(this.activeFolderId());
      return;
    }
    void this.tabsService.openNote(id, title || "Note");
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

  // --- Folder unlock (lock-gate CTA only) ---------------------------------
  // Folder CREATE / RENAME / DELETE / LOCK now live in the main sidebar's
  // `NotesSidebarTreeComponent`. This view keeps only the UNLOCK path, for the
  // content pane's "This folder is locked" gate (below) and a masked note row.

  /**
   * Session-unlock a sealed note-folder (Touch ID via the shared folder command),
   * called from the content pane's lock gate or a masked note row. Refreshes the
   * note lists (+ the sidebar tree's own folder list, via {@link NotesService})
   * so the previously-masked notes + the tree's lock badge both update.
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
      await Promise.allSettled([
        this.notes.loadFolders(),
        this.notes.loadNotes(this.activeFolderId()),
      ]);
    } catch {
      // A cancelled/denied Touch ID prompt — stay masked, no scary toast.
      this.toast.danger("Couldn’t unlock this folder.");
    } finally {
      this.lockBusyId.set(null);
    }
  }

  // --- Auto-organize ------------------------------------------------------

  /**
   * Fetch a proposed organize plan for the active scope (null ⇒ all notes) and
   * open the review sheet. The sheet renders even for an empty plan ("already
   * organized"). A fetch failure surfaces a toast and leaves the sheet closed.
   */
  async startOrganize(): Promise<void> {
    if (
      !this.canOrganizeActiveScope() ||
      this.organizePlanning() ||
      this.organizeOpen()
    ) {
      return;
    }
    const scopeFolderId = this.activeFolderId();
    this.organizeScopeFolderId = scopeFolderId;
    await this.planOrganize(scopeFolderId, null);
  }

  /** Replan the reviewed scope; sidebar navigation cannot silently retarget it. */
  async replanOrganize(guidance: string): Promise<void> {
    const viewPlan = this.organizePlan();
    const scopeFolderId = this.organizeScopeFolderId;
    if (
      !viewPlan ||
      scopeFolderId === undefined ||
      viewPlan.scopeFolderId !== scopeFolderId ||
      !this.organizerScopeIsCurrent(scopeFolderId) ||
      this.organizePlanning() ||
      this.organizeApplying()
    ) {
      return;
    }
    await this.planOrganize(scopeFolderId, guidance || null);
  }

  private async planOrganize(
    scopeFolderId: string | null,
    guidance: string | null,
  ): Promise<void> {
    const generation = ++this.organizePlanGeneration;
    this.organizePlanning.set(true);
    try {
      const privacyReady = await this.privacyBarrier.ensureReady();
      if (
        generation !== this.organizePlanGeneration ||
        !this.organizerScopeIsCurrent(scopeFolderId)
      ) {
        return;
      }
      if (!privacyReady) {
        this.scrubOrganizeReview();
        return;
      }
      const plan = await this.notes.planOrganize(scopeFolderId, guidance);
      if (
        generation !== this.organizePlanGeneration ||
        !this.organizerScopeIsCurrent(scopeFolderId)
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
        this.organizerScopeIsCurrent(scopeFolderId)
      ) {
        this.toast.danger("Couldn’t plan an auto-organize. Please try again.");
      }
    } finally {
      if (
        generation === this.organizePlanGeneration &&
        this.organizerScopeIsCurrent(scopeFolderId)
      ) {
        this.organizePlanning.set(false);
      }
    }
  }

  /** Apply the selected moves and keep every unresolved row available for review/retry. */
  async applyOrganize(moves: OrganizeMove[]): Promise<void> {
    if (this.organizePlanning() || this.organizeApplying()) {
      return;
    }
    const viewPlan = this.organizePlan();
    if (!viewPlan || moves.length === 0) {
      return;
    }
    const generation = this.organizePlanGeneration;
    const scopeFolderId = this.organizeScopeFolderId;
    if (
      scopeFolderId === undefined ||
      viewPlan.scopeFolderId !== scopeFolderId ||
      !this.organizerScopeIsCurrent(scopeFolderId)
    ) {
      return;
    }
    this.organizeApplying.set(true);
    try {
      // Strip frontend-only receipt/error state at the service boundary. The
      // backend receives exactly the plan the user reviewed, with only the
      // currently selected moves.
      const plan: OrganizePlan = {
        scopeFolderId: viewPlan.scopeFolderId,
        moves,
        totalScanned: viewPlan.totalScanned,
        alreadyOrganized: viewPlan.alreadyOrganized,
        deferred: viewPlan.deferred,
        targets: viewPlan.targets,
      };
      const result = await this.notes.applyOrganize(plan);
      if (
        generation !== this.organizePlanGeneration ||
        !this.organizerScopeIsCurrent(scopeFolderId)
      ) {
        return;
      }
      // Re-apply the active-folder filter after the store's all-notes reload.
      await this.notes.loadNotes(this.activeFolderId());
      if (
        generation !== this.organizePlanGeneration ||
        !this.organizerScopeIsCurrent(scopeFolderId)
      ) {
        return;
      }
      const receipt = this.mergeOrganizeReceipt(
        viewPlan.receipt,
        moves,
        result.appliedIds,
        result.failures,
      );
      const appliedIds = new Set(receipt.appliedIds);
      const remainingMoves = viewPlan.moves.filter(
        (move) => !appliedIds.has(move.noteId),
      );
      const moved = new Set(result.appliedIds).size;
      const unresolved = receipt.failures.length;

      if (unresolved === 0 && remainingMoves.length === 0) {
        this.organizePlan.set(null);
        this.toast.success(
          `${moved} ${moved === 1 ? "note" : "notes"} organized`,
        );
      } else {
        this.organizePlan.set({
          ...viewPlan,
          moves: remainingMoves,
          receipt,
          applyError: null,
        });
        if (unresolved > 0) {
          this.toast.danger(
            `${moved} moved; ${unresolved} still need attention.`,
          );
        } else {
          this.toast.success(
            `${moved} ${moved === 1 ? "note" : "notes"} organized; ${remainingMoves.length} still awaiting your choice.`,
          );
        }
      }
    } catch {
      if (
        generation !== this.organizePlanGeneration ||
        !this.organizerScopeIsCurrent(scopeFolderId)
      ) {
        return;
      }
      this.organizePlan.update((plan) =>
        plan
          ? {
              ...plan,
              applyError:
                "The filing request did not finish. Review the selected moves and retry.",
            }
          : plan,
      );
      this.toast.danger("Couldn’t finish applying the plan.");
    } finally {
      if (
        generation === this.organizePlanGeneration &&
        this.organizerScopeIsCurrent(scopeFolderId)
      ) {
        this.organizeApplying.set(false);
      }
    }
  }

  /** A late async result must still belong to the scope visible right now. */
  private organizerScopeIsCurrent(scopeFolderId: string | null): boolean {
    return (
      this.organizeScopeFolderId === scopeFolderId &&
      this.activeOrgId() === null &&
      this.activeFolderId() === scopeFolderId
    );
  }

  /** Close the organize review sheet without applying. */
  closeOrganize(): void {
    if (this.organizePlanning() || this.organizeApplying()) {
      return;
    }
    ++this.organizePlanGeneration;
    this.organizePlanning.set(false);
    this.organizePlan.set(null);
    this.organizeScopeFolderId = undefined;
  }

  /** Synchronous privacy boundary: no content-bearing organizer cache survives it. */
  private scrubOrganizeReview(): void {
    ++this.organizePlanGeneration;
    this.organizePlanning.set(false);
    this.organizeApplying.set(false);
    this.organizePlan.set(null);
    this.organizeScopeFolderId = undefined;
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
        unresolved.set(failure.noteId, {
          ...failure,
          reason: this.boundedFailureReason(failure.reason),
        });
      }
    }
    return {
      moves: [...moves.values()],
      appliedIds: [...applied],
      failures: [...unresolved.values()],
    };
  }

  /** Match the workspace organizer's bounded, actionable failure copy. */
  private boundedFailureReason(reason: string): string {
    const normalized = reason.replace(/\s+/g, " ").trim();
    if (!normalized) {
      return "Review the destination and try again.";
    }
    const hadInvalidArgumentPrefix = /^invalid argument\s*:/i.test(normalized);
    const detail = normalized.replace(/^invalid argument\s*:\s*/i, "").trim();
    let actionable = detail || "Review the destination and try again.";
    if (hadInvalidArgumentPrefix && /lock|seal/i.test(detail)) {
      actionable = "Unlock or choose an open destination, then retry.";
    } else if (hadInvalidArgumentPrefix) {
      actionable = `${this.sentenceCase(detail)} Review the destination and try again.`;
    }
    if (actionable.length <= MAX_ORGANIZE_FAILURE_REASON_LENGTH) {
      return actionable;
    }
    return `${actionable.slice(0, MAX_ORGANIZE_FAILURE_REASON_LENGTH - 1)}…`;
  }

  private sentenceCase(value: string): string {
    const withoutTrailingPunctuation = value.replace(/[.!?]+$/, "");
    if (!withoutTrailingPunctuation) {
      return "";
    }
    return `${withoutTrailingPunctuation[0].toLocaleUpperCase()}${withoutTrailingPunctuation.slice(1)}.`;
  }

  // --- Presentational -----------------------------------------------------

  /** Presentational only: epoch-ms → a friendly local date. */
  /** Formatted through {@link DateFormatService} — the one place a date becomes user-visible text. */
  formatDate(updatedAt: string): string {
    return this.dates.day(updatedAt);
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
