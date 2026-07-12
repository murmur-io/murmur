import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  OnInit,
  computed,
  effect,
  inject,
  signal,
  untracked,
  viewChild,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import { TabsService } from "../../../core/tabs.service";
import type {
  FolderNode,
  Meeting,
  MeetingStatus,
  SearchHit,
} from "../../../core/models";
import {
  FoldersService,
  type FolderExposure,
} from "../../../services/folders.service";
import { LockBadgeComponent } from "../../folders/lock-badge/lock-badge.component";
import { MoveToMenuComponent } from "../../folders/move-to-menu/move-to-menu.component";
import { NoteDragService } from "../../folders/note-drag.service";
import { ToastService } from "../../../services/toast.service";

/** Debounce window for search-as-you-type — quick enough to feel instant. */
const SEARCH_DEBOUNCE_MS = 180;

/** A snippet split into runs around the query match, for safe <mark>-style emphasis. */
interface SnippetPart {
  text: string;
  hit: boolean;
}

/**
 * One row of the no-query list. Meetings is a RECORDINGS-ONLY view: a
 * `"meeting"` row is a local recording (opens `/meeting/:id`, keeps its
 * folder-chip / delete / drag / lock affordances). It exposes an epoch-ms
 * `sortAt` so the list orders by date desc, and a `meeting:`-namespaced `id`
 * so the `@for` track key is stable.
 */
export interface MeetingsListItem {
  kind: "meeting";
  id: string;
  sortAt: number;
  meeting: Meeting;
}

@Component({
  selector: "app-library",
  changeDetection: ChangeDetectionStrategy.OnPush,
  // Esc in Meetings backs out ("← Murmur") — but NOT while you're typing: in the
  // search box Esc clears/blurs it first, and it never hijacks another form field.
  // Declarative host listeners — Angular owns their lifecycle (mirrors settings).
  // Esc lives at DOCUMENT level on purpose: after clicking non-focusable text
  // focus falls to <body>, so a panel-scoped (keydown.escape) would go dead.
  // document:click closes the row ⋯ menu / move popover on an outside click.
  host: {
    "(document:keydown.escape)": "onEscape()",
    "(document:click)": "onDocumentClick($event)",
  },
  imports: [LockBadgeComponent, MoveToMenuComponent],
  templateUrl: "./library.component.html",
  styleUrl: "./library.component.scss",
})
export class LibraryComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);
  private readonly folders = inject(FoldersService);
  private readonly drag = inject(NoteDragService);
  private readonly toast = inject(ToastService);
  private readonly tabsService = inject(TabsService);

  /**
   * Open a meeting as a browser-style tab (replaces the row's plain
   * `[routerLink]` so re-opening an already-open meeting activates its
   * existing tab instead of navigating fresh).
   */
  openMeeting(event: Event, id: string, title: string | null): void {
    event.preventDefault();
    void this.tabsService.openMeeting(id, title || "Meeting");
  }

  /**
   * Esc while in Meetings closes open transient UI first (one Esc = one
   * dismissal: the row ⋯ menu, then the move popover) — but NEVER while
   * you're typing: in the search box the first Esc clears it (or blurs when
   * empty), and Esc is ignored inside any other form field, so it never
   * ejects you mid-edit. `/library` is no longer a drill-down (Stage 2,
   * 2026-07-12, mirrors Notes/Stage 1): there is no "back to Murmur"
   * fallback anymore — the persistent sidebar IS the way back.
   */
  onEscape(): void {
    if (this.rowMenuId() !== null) {
      this.rowMenuId.set(null);
      return;
    }
    if (this.movePopoverId() !== null) {
      this.closeMovePopover();
      return;
    }
    const el = document.activeElement as HTMLElement | null;
    if (el?.classList.contains("search-input")) {
      if (this.query().trim()) {
        this.clear();
      } else {
        el.blur();
      }
    }
  }

  /** The meeting id whose "Move to…" popover is open (null = none). */
  readonly movePopoverId = signal<string | null>(null);

  /** The meeting id whose row ⋯ actions menu is open (null = none). */
  readonly rowMenuId = signal<string | null>(null);

  /**
   * Outside-click dismissal for the row ⋯ menu and the move popover. The ⋯
   * trigger and every menu item stopPropagation, so any click that reaches the
   * document and isn't inside an open panel means "clicked elsewhere → close".
   */
  onDocumentClick(event: MouseEvent): void {
    if (this.rowMenuId() === null && this.movePopoverId() === null) {
      return;
    }
    const target = event.target as HTMLElement | null;
    if (target?.closest(".row-menu, .row-menu-btn, .move-anchor")) {
      return;
    }
    this.rowMenuId.set(null);
    this.movePopoverId.set(null);
  }

  /** The meeting id currently being dragged (mirrors the shared drag signal). */
  readonly draggingId = this.drag.draggingId;

  /** The search box element — focused after a clear. */
  private readonly searchInput =
    viewChild<ElementRef<HTMLInputElement>>("searchInput");

  // --- No-query meetings list (unchanged behaviour) -----------------------
  readonly meetings = signal<Meeting[]>([]);
  readonly loading = signal(true);

  // --- Folder filter ---------------------------------------------------
  // The folder TREE UI (create/rename/delete/lock/unlock/drag-drop, "Lock
  // all") now lives in the main sidebar's `MeetingsSidebarTreeComponent`
  // (Stage 2, 2026-07-12 — mirrors Notes/Stage 1). This view only reads the
  // SHARED selection to filter its own content.
  /** The lock-aware folder forest from the signal store (for name/exposure lookups). */
  readonly folderTree = this.folders.tree;
  /**
   * Selected folder id (null = no folder filter — show the tag/all list),
   * SHARED with the sidebar tree via {@link FoldersService.activeFolderId}.
   * Mutually exclusive with the tag filter: selecting one clears the other.
   */
  readonly activeFolderId = this.folders.activeFolderId;

  /**
   * Picking a folder in the sidebar tree bypasses this component entirely
   * (it calls `FoldersService.selectFolder` directly), so the tag-filter /
   * transient-row-UI clearing {@link selectFolder} used to do inline now runs
   * as a reactive sync on the SHARED signal instead — mirrors
   * `NotesHomeComponent`'s `_clearOrgOnFolderChange` (Stage 1). Legitimate
   * signal-writing effect (T1): a cross-component UI-state sync, not async
   * orchestration. `untracked` keeps `activeFolderId()` the only dependency.
   */
  private readonly _syncOnFolderChange = effect(() => {
    const fid = this.activeFolderId();
    untracked(() => {
      this.cancelDelete();
      this.rowMenuId.set(null);
      this.movePopoverId.set(null);
      if (fid !== null) {
        this.activeTag.set(null);
        this.tagMeetings.set([]);
        this.tagLoading.set(false);
      }
    });
  });

  /**
   * Every folder node keyed by id (flattened) — for O(1) exposure/mask lookups
   * keyed off a meeting's `folderId`. Recomputes whenever the tree reloads.
   */
  private readonly folderById = computed(() => {
    const map = new Map<string, FolderNode>();
    const walk = (nodes: FolderNode[]): void => {
      for (const n of nodes) {
        map.set(n.id, n);
        // Defensive: a node from an older/odd backend may omit `children`.
        // Never let a missing array throw here — that would take the whole
        // Library view (both panes) down, not just the folder tree.
        if (n.children?.length) {
          walk(n.children);
        }
      }
    };
    walk(this.folderTree());
    return map;
  });

  /**
   * Meetings in the active folder. Derived from the already-loaded `meetings`
   * list via the committed `Meeting.folderId` contract field (no extra IPC —
   * the backend exposes no folder-scoped list command). When the field is
   * absent (older backend) this is simply empty until notes carry a folderId.
   */
  readonly folderMeetings = computed(() => {
    const fid = this.activeFolderId();
    if (fid === null) {
      return [];
    }
    return this.meetings().filter((m) => m.folderId === fid);
  });

  // --- Tag filter ----------------------------------------------------------
  /** All distinct tags across meetings; empty → no filter bar is rendered. */
  readonly tags = signal<string[]>([]);
  /** Selected tag (null = "All", i.e. the full meetings list). */
  readonly activeTag = signal<string | null>(null);
  /** Meetings carrying the active tag (only used when a tag is selected). */
  readonly tagMeetings = signal<Meeting[]>([]);
  /** True while a tag's meetings are being fetched. */
  readonly tagLoading = signal(false);

  /**
   * The MEETINGS (recordings) to render when not searching, in strict precedence
   * (search is handled separately via `hasQuery()`):
   *   folder selected → folder-filtered;
   *   else tag selected → tag-filtered;
   *   else → the full list.
   * This is the meeting SOURCE the {@link listItems} list is built from.
   */
  readonly displayedMeetings = computed(() => {
    if (this.activeFolderId() !== null) {
      return this.folderMeetings();
    }
    return this.activeTag() === null ? this.meetings() : this.tagMeetings();
  });

  /**
   * The no-query list feeding the pane (RECORDINGS ONLY), sorted by date desc.
   * Built from {@link displayedMeetings}, which already applies the folder/tag
   * precedence. Only recordings can be masked (folder-sealed).
   */
  readonly listItems = computed<MeetingsListItem[]>(() =>
    this.displayedMeetings()
      .map<MeetingsListItem>((meeting) => ({
        kind: "meeting",
        id: `meeting:${meeting.id}`,
        sortAt: this.meetingSortAt(meeting),
        meeting,
      }))
      .sort((a, b) => b.sortAt - a.sortAt),
  );

  /** True when the no-query list has zero rows (drives the empty state). */
  readonly listEmpty = computed(() => this.listItems().length === 0);

  /** Epoch-ms sort key for a meeting (its start time; 0 when unparseable). */
  private meetingSortAt(m: Meeting): number {
    const t = Date.parse(m.startedAt);
    return Number.isNaN(t) ? 0 : t;
  }

  /** Loading state for the visible no-query list (initial load or tag fetch). */
  readonly listLoading = computed(() => {
    if (this.activeFolderId() !== null) {
      // Folder filtering is client-side over `meetings`, so it shares the
      // initial-load flag (and the tree's own loading shows in the left pane).
      return this.loading();
    }
    if (this.activeTag() === null) {
      return this.loading();
    }
    return this.tagLoading();
  });

  /** Heading for the no-query list: folder name → tag → "Meetings". */
  readonly listHeading = computed(() => {
    const fid = this.activeFolderId();
    if (fid !== null) {
      return this.folderById().get(fid)?.name ?? "Folder";
    }
    return this.activeTag() ?? "Meetings";
  });

  /** Exposure of the active folder (for the header lock badge); null when none. */
  readonly activeFolderExposure = computed<FolderExposure | null>(() => {
    const fid = this.activeFolderId();
    if (fid === null) {
      return null;
    }
    const node = this.folderById().get(fid);
    return node ? this.folders.exposureOf(node) : null;
  });

  // --- Delete affordance (in-app, signal-driven confirm) ------------------
  /** Id of the meeting whose inline confirm panel is open (null = none). */
  readonly pendingDeleteId = signal<string | null>(null);
  /** True while a delete IPC call is in flight — guards the confirm button. */
  readonly deleting = signal(false);
  /** Non-empty when the last delete failed (cleared on the next attempt). */
  readonly deleteError = signal<string | null>(null);

  // --- Search state -------------------------------------------------------
  /** Raw, untrimmed query bound to the input. */
  readonly query = signal("");
  /** Latest applied search hits. */
  readonly results = signal<SearchHit[]>([]);
  /** True while an IPC search is in flight (drives the "Searching…" state). */
  readonly searching = signal(false);

  /** Whether the (trimmed) query is non-empty — switches list ↔ results. */
  readonly hasQuery = computed(() => this.query().trim().length > 0);

  /** Ids of the meetings carrying the active tag — O(1) membership checks. */
  private readonly tagMeetingIds = computed(
    () => new Set(this.tagMeetings().map((m) => m.id)),
  );

  /**
   * Search hits, narrowed by the active tag when one is selected. The backend
   * search command has no tag parameter, so the intersection is client-side:
   * `tagMeetings` (already fetched by {@link selectTag}) provides the id set of
   * meetings carrying the tag, and hits outside it are dropped. "All" (null tag)
   * passes every hit through untouched.
   */
  readonly displayedResults = computed(() => {
    const hits = this.results();
    if (this.activeTag() === null) {
      return hits;
    }
    const ids = this.tagMeetingIds();
    return hits.filter((h) => ids.has(h.meeting.id));
  });

  /**
   * The search surface is busy while the search itself OR the active tag's
   * meeting list (needed to filter the hits) is still in flight — either gap
   * would otherwise flash a bogus "no matches".
   */
  readonly searchBusy = computed(
    () => this.searching() || (this.activeTag() !== null && this.tagLoading()),
  );

  /** Tracked so we can cancel a pending debounce on re-trigger / destroy. */
  private searchTimer: ReturnType<typeof setTimeout> | null = null;

  /** True once the initial `ngOnInit` meetings load has settled — guards {@link _reloadMeetingsOnTreeChange}. */
  private hasLoadedOnce = false;

  /**
   * Re-fetch the meetings list whenever the SHARED folder tree changes (a
   * move dropped on the sidebar tree, a lock/unlock, a rename…) — this is how
   * a moved note leaves the current folder view now that the drop TARGET
   * (`MeetingsSidebarTreeComponent`) is a sibling, not a child, and can't
   * patch this component's local `meetings` signal directly (see the comment
   * at {@link onRowDragEnd}). Skipped until the initial `ngOnInit` load has
   * settled, so this doesn't race/duplicate that first fetch. Legitimate
   * signal-writing effect (T1) — async orchestration with a guard.
   */
  private readonly _reloadMeetingsOnTreeChange = effect(() => {
    this.folders.tree();
    if (this.hasLoadedOnce) {
      untracked(() => void this.reloadMeetings());
    }
  });

  async ngOnInit(): Promise<void> {
    // Clean up any in-flight debounce timer when the view is torn down.
    this.destroyRef.onDestroy(() => {
      if (this.searchTimer) {
        clearTimeout(this.searchTimer);
      }
    });

    // Load the meetings list and the tag set in parallel; a tag-load failure
    // must not break the meetings list, so settle each independently.
    const [meetings] = await Promise.allSettled([
      this.ipc.listMeetings(),
      this.loadTags(),
    ]);
    if (meetings.status === "fulfilled") {
      this.meetings.set(meetings.value);
    }
    this.loading.set(false);
    this.hasLoadedOnce = true;
  }

  /** Best-effort reload of the meetings list (a failure leaves the last-known list standing). */
  private async reloadMeetings(): Promise<void> {
    try {
      this.meetings.set(await this.ipc.listMeetings());
    } catch {
      // Stale list self-heals on the next successful reload.
    }
  }

  /** Fetch the distinct tag set; on failure leave `tags` empty (no filter bar). */
  private async loadTags(): Promise<void> {
    try {
      this.tags.set(await this.ipc.listAllTags());
    } catch {
      this.tags.set([]);
    }
  }

  // --- Tag filtering -------------------------------------------------------

  /**
   * Select a tag (or `null` for "All"). "All" clears back to the full meetings
   * list; a tag loads its meetings into `tagMeetings`. Latest-tag-wins so a
   * slower earlier fetch can't clobber a newer selection.
   */
  async selectTag(tag: string | null): Promise<void> {
    if (this.activeTag() === tag) {
      return;
    }
    // Switching the view dismisses any open delete confirm / row menu / move
    // popover to avoid a dangling panel pointing at a row not in the new list.
    this.cancelDelete();
    this.rowMenuId.set(null);
    this.movePopoverId.set(null);
    // Tag + folder scopes are mutually exclusive: picking a tag clears any
    // active folder (the SHARED sidebar-tree selection) so they never compose
    // into an empty surprise.
    if (tag !== null) {
      this.folders.selectFolder(null);
    }
    this.activeTag.set(tag);

    if (tag === null) {
      this.tagMeetings.set([]);
      this.tagLoading.set(false);
      return;
    }

    this.tagLoading.set(true);
    try {
      const list = await this.ipc.listMeetingsByTag(tag);
      if (this.activeTag() !== tag) {
        return; // stale — a newer tag selection superseded this request.
      }
      this.tagMeetings.set(list);
    } catch {
      if (this.activeTag() === tag) {
        this.tagMeetings.set([]);
      }
    } finally {
      if (this.activeTag() === tag) {
        this.tagLoading.set(false);
      }
    }
  }

  // --- Filing: the per-row ⋯ actions menu + "Move to…" popover -------------

  /**
   * The display name of a meeting's current folder, or null when it's at the
   * vault root. Drives the row's display-only folder label pill.
   */
  folderNameOf(m: Meeting): string | null {
    const fid = m.folderId ?? null;
    if (fid === null) {
      return null;
    }
    return this.folderById().get(fid)?.name ?? null;
  }

  /** Toggle the row's ⋯ actions menu (one open at a time; closes the mover). */
  toggleRowMenu(id: string): void {
    this.movePopoverId.set(null);
    this.rowMenuId.update((cur) => (cur === id ? null : id));
  }

  /** ⋯ menu → "Move to folder…": swap the menu for the folder-picker popover. */
  openMoveFromMenu(id: string): void {
    this.rowMenuId.set(null);
    this.movePopoverId.set(id);
  }

  /** ⋯ menu → "Delete meeting…": swap the menu for the inline confirm panel. */
  deleteFromMenu(id: string): void {
    this.rowMenuId.set(null);
    this.askDelete(id);
  }

  closeMovePopover(): void {
    this.movePopoverId.set(null);
  }

  /**
   * Apply a move (from the row chip popover OR a drag-drop) into `folderId` (null
   * = vault root). The FoldersService reloads the tree (so counts refresh), and
   * we patch the LIBRARY-LOCAL `meetings` signal's `folderId` so the derived
   * `folderMeetings` recomputes at once — the moved note leaves the current
   * folder view without a manual reload. `tagMeetings` is patched in lockstep so
   * a tag view stays coherent if one is active.
   */
  private async applyMove(
    meetingId: string,
    folderId: string | null,
  ): Promise<void> {
    const patch = (list: Meeting[]): Meeting[] =>
      list.map((m) => (m.id === meetingId ? { ...m, folderId } : m));
    this.meetings.update(patch);
    this.tagMeetings.update(patch);
  }

  /**
   * The popover's `moved` output already ran the IPC move via FoldersService;
   * here we only reconcile the local list + close the popover.
   */
  onMoved(meetingId: string, folderId: string | null): void {
    void this.applyMove(meetingId, folderId);
    this.closeMovePopover();
  }

  // --- Filing: drag a row onto a folder (the enhancement path) -------------

  /** Begin a row drag: stash the meeting id on the transfer + the shared signal. */
  onRowDragStart(event: DragEvent, m: Meeting): void {
    // A locked-and-not-unlocked note is masked; dragging it is still fine (the
    // move runs through the same load-bearing confirm at the destination), so we
    // allow it. The transfer carries the id under our private MIME type.
    if (event.dataTransfer) {
      event.dataTransfer.effectAllowed = "move";
      event.dataTransfer.setData(NoteDragService.MIME, m.id);
    }
    this.drag.begin(m.id);
  }

  /** End a row drag (fires whether or not it landed on a target). */
  onRowDragEnd(): void {
    this.drag.end();
  }

  // The DROP target (the folder tree) now lives in the main sidebar's
  // `MeetingsSidebarTreeComponent` (Stage 2, 2026-07-12) — a SIBLING
  // component, not a child, so it can no longer reach into this component's
  // local `meetings` signal to patch it directly the way `onDropNote` used
  // to. Reloading `meetings` whenever the shared folder tree changes (see
  // `_reloadMeetingsOnTreeChange` below) is how this self-heals instead —
  // mirrors the "backend is truth, reload rather than patch" rule the rest
  // of this app's stores already follow.

  // --- Lock-aware row rendering -------------------------------------------

  /**
   * The exposure of the folder a meeting lives in (open / locked / session), or
   * null when the note is at the vault root / its folder isn't known. Drives the
   * inline lock badge on a meeting row.
   */
  folderExposureOf(m: Meeting): FolderExposure | null {
    const fid = m.folderId ?? null;
    if (fid === null) {
      return null;
    }
    const node = this.folderById().get(fid);
    return node ? this.folders.exposureOf(node) : null;
  }

  /**
   * Whether a meeting's title must be masked: it lives in a folder that is
   * sealed and NOT session-unlocked (`exposure === 'locked'`). A session-
   * unlocked folder ('session') shows its titles normally.
   */
  isMasked(m: Meeting): boolean {
    return this.folderExposureOf(m) === "locked";
  }

  // --- Search-as-you-type --------------------------------------------------

  /** Mirror the input into the `query` signal, then debounce the search. */
  onQueryInput(event: Event): void {
    this.query.set((event.target as HTMLInputElement).value);
    this.scheduleSearch();
  }

  /**
   * Debounced search dispatch (DestroyRef-tracked timeout — no bare setTimeout
   * lifecycle). An empty/whitespace query clears results immediately; a real
   * query runs after the debounce window.
   */
  private scheduleSearch(): void {
    if (this.searchTimer) {
      clearTimeout(this.searchTimer);
      this.searchTimer = null;
    }

    const q = this.query().trim();
    if (!q) {
      // Empty query: drop any in-flight state and show the meetings list.
      this.searching.set(false);
      this.results.set([]);
      return;
    }

    // Search takes precedence over the FOLDER filter (search is vault-wide),
    // but COMPOSES with the tag filter: the tagbar stays visible while
    // searching and the active tag narrows the hits (see `displayedResults`).
    if (this.activeFolderId() !== null) {
      this.folders.selectFolder(null);
    }

    this.searching.set(true);
    this.searchTimer = setTimeout(() => {
      void this.runSearch(q);
    }, SEARCH_DEBOUNCE_MS);
  }

  /**
   * Execute one search. Latest-query-wins: by the time the promise resolves the
   * user may have typed on, so we only apply results if `q` still matches the
   * current trimmed query — otherwise a slower earlier request can't clobber a
   * newer one.
   */
  private async runSearch(q: string): Promise<void> {
    try {
      const hits = await this.ipc.searchMeetings(q);
      if (this.query().trim() !== q) {
        return; // stale — a newer keystroke superseded this request.
      }
      this.results.set(hits);
    } catch {
      if (this.query().trim() === q) {
        this.results.set([]);
      }
    } finally {
      if (this.query().trim() === q) {
        this.searching.set(false);
      }
    }
  }

  /** Reset the query + results and return focus to the input. */
  clear(): void {
    if (this.searchTimer) {
      clearTimeout(this.searchTimer);
      this.searchTimer = null;
    }
    this.query.set("");
    this.results.set([]);
    this.searching.set(false);
    this.searchInput()?.nativeElement.focus();
  }

  // --- Delete a meeting (open confirm → await IPC → prune signal) ----------

  /**
   * Open the inline confirm panel for `id`. The triggering ✕ button calls
   * `preventDefault`/`stopPropagation` itself so the row never navigates.
   */
  askDelete(id: string): void {
    this.deleteError.set(null);
    this.pendingDeleteId.set(id);
  }

  /** Dismiss the confirm panel without deleting (ignored mid-flight). */
  cancelDelete(): void {
    if (this.deleting()) {
      return;
    }
    this.pendingDeleteId.set(null);
    this.deleteError.set(null);
  }

  /**
   * Confirm the pending delete: await the irreversible IPC call, then prune the
   * row from the local `meetings` signal (no full reload needed). On failure we
   * surface an inline error and keep the panel open so the user can retry.
   */
  async confirmDelete(id: string): Promise<void> {
    if (this.deleting()) {
      return;
    }
    this.deleting.set(true);
    this.deleteError.set(null);
    try {
      await this.ipc.deleteMeeting(id);
      // Prune from both lists so whichever view is showing updates at once.
      this.meetings.update((list) => list.filter((m) => m.id !== id));
      this.tagMeetings.update((list) => list.filter((m) => m.id !== id));
      this.pendingDeleteId.set(null);
    } catch {
      this.deleteError.set("Couldn’t delete this meeting. Please try again.");
    } finally {
      this.deleting.set(false);
    }
  }

  // --- Snippet highlighting (no innerHTML / DomSanitizer) ------------------

  /**
   * Split a snippet into runs around case-insensitive matches of the current
   * query, so the template can wrap matched runs in a styled <mark> element.
   * Returns a single non-hit run when the query doesn't occur in the snippet.
   */
  snippetParts(snippet: string): SnippetPart[] {
    const q = this.query().trim();
    if (!q) {
      return [{ text: snippet, hit: false }];
    }
    const parts: SnippetPart[] = [];
    const haystack = snippet.toLowerCase();
    const needle = q.toLowerCase();
    let from = 0;
    let at = haystack.indexOf(needle, from);
    while (at !== -1) {
      if (at > from) {
        parts.push({ text: snippet.slice(from, at), hit: false });
      }
      parts.push({ text: snippet.slice(at, at + needle.length), hit: true });
      from = at + needle.length;
      at = haystack.indexOf(needle, from);
    }
    if (from < snippet.length) {
      parts.push({ text: snippet.slice(from), hit: false });
    }
    return parts;
  }

  /** Human label for the field a hit matched in. */
  matchLabel(matchedIn: string): string {
    switch (matchedIn) {
      case "transcript":
        return "in transcript";
      case "note":
        return "in note";
      default:
        return "title";
    }
  }

  /** Tint the matched-in badge: transcript/note = accent, title = neutral. */
  matchBadgeClass(matchedIn: string): string {
    switch (matchedIn) {
      case "transcript":
        return "is-accent";
      case "note":
        return "is-success";
      default:
        return "";
    }
  }

  statusLabel(s: string): string {
    return s.charAt(0) + s.slice(1).toLowerCase();
  }

  /** Maps a meeting status to a status-pill state modifier (matches Record). */
  statusPillClass(s: MeetingStatus): string {
    switch (s) {
      case "RECORDING":
      case "ERROR":
        return "is-danger";
      case "TRANSCRIBED":
      case "SUMMARIZED":
        return "is-accent";
      case "EXPORTED":
        return "is-success";
      default:
        return "";
    }
  }

  /** Presentational only: render the stored timestamp as a friendly local date. */
  formatDate(startedAt: string): string {
    const d = new Date(startedAt);
    if (Number.isNaN(d.getTime())) {
      return startedAt;
    }
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  }

  /** Presentational only: seconds → compact "Hh Mm" / "Mm Ss" / "Ss" duration. */
  formatDuration(durationS: number): string {
    const total = Math.max(0, Math.round(durationS));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    if (h > 0) {
      return `${h}h ${m}m`;
    }
    if (m > 0) {
      return `${m}m ${s}s`;
    }
    return `${s}s`;
  }

  /**
   * A finalized recording whose audio was freed to save space (audio gone, note kept).
   * NEVER a locked/sealed meeting: a masked read can surface `audioPath: null`, and
   * prune excludes every `folders.locked = 1` folder — so mirror that exclusion here
   * (`node.locked`) rather than mislabel a sealed meeting as "audio freed".
   */
  isAudioFreed(m: Meeting): boolean {
    if (m.audioPath !== null || m.status === "ERROR") return false;
    const node = m.folderId ? this.folderById().get(m.folderId) : undefined;
    return !node?.locked;
  }
}
