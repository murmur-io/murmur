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
import { RouterLink } from "@angular/router";
import { IpcService } from "../../../core/ipc.service";
import { TabsService } from "../../../core/tabs.service";
import { MurSpinnerComponent } from "../../../design-system/spinner/spinner.component";
import {
  meetingStatusLabel,
  meetingStatusPillClass,
} from "../../../design-system/meeting-status";
import type {
  FolderNode,
  Meeting,
  MeetingOrgShareRow,
  OrgItemHeader,
  OrgStatus,
  SearchHit,
} from "../../../core/models";
import {
  FoldersService,
  type FolderExposure,
} from "../../../services/folders.service";
import { MeetingsListStore } from "../../../services/meetings-list-store.service";
import { OrgBrainService } from "../../../services/org-brain.service";
import { SavedViewsService } from "../../../services/saved-views.service";
import { ViewEngine } from "../../../services/view-engine";
import { LockBadgeComponent } from "../../folders/lock-badge/lock-badge.component";
import { MoveToMenuComponent } from "../../folders/move-to-menu/move-to-menu.component";
import { NoteDragService } from "../../folders/note-drag.service";
import { ToastService } from "../../../services/toast.service";
import { MeetingsViewSwitcherComponent } from "../meetings-view-switcher/meetings-view-switcher.component";
import { MeetingsTableViewComponent } from "../meetings-table-view/meetings-table-view.component";
import { matchedInLabel } from "../../../core/copy/labels";

/** Debounce window for search-as-you-type — quick enough to feel instant. */
const SEARCH_DEBOUNCE_MS = 180;

/** A snippet split into runs around the query match, for safe <mark>-style emphasis. */
interface SnippetPart {
  text: string;
  hit: boolean;
}

/**
 * A search hit with its presentation already resolved.
 *
 * The template renders these fields directly instead of calling a helper per row: a method
 * binding inside a `@for` re-runs on every change-detection pass for every visible row, which
 * the zoneless rules table bans in favour of a `computed()` view model.
 */
interface SearchRow extends SearchHit {
  /** Human label for the field the hit matched in — `""` when it cannot be named. */
  readonly whereMatched: string;
  /** Tint class for the matched-in badge. */
  readonly badgeClass: string;
}

/**
 * Tint the matched-in badge: transcript/note = accent, title = neutral.
 *
 * A pure module function, not a component method, so it cannot be reached from a template.
 */
function matchBadgeClass(matchedIn: string): string {
  switch (matchedIn) {
    case "transcript":
      return "is-accent";
    case "note":
      return "is-success";
    default:
      return "";
  }
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
  /** Pre-derived pill presentation — the template must not call helpers per row. */
  statusPillClass: string;
  statusLabel: string;
}

/**
 * One row of an org's "Shared Brains" meeting list (shown ONLY when an org is
 * rail-selected — never merged into {@link MeetingsListItem}'s "All meetings").
 * A READ-ONLY replica (opens `/org-item/:id`), UNLESS the caller authored it
 * (`ownedSource`), in which case it routes straight to the editable original
 * (`/meeting/:id`). `id` is `org:`-namespaced for a stable `@for` track key.
 */
export interface OrgMeetingListItem {
  kind: "org";
  id: string;
  sortAt: number;
  item: OrgItemHeader;
  orgName: string;
  /** `item.kind` was `null`/absent (shared before the meeting/document distinction
   * existed) — shown here anyway (never silently hidden), just badged so it reads
   * as unverified rather than a confirmed meeting. See {@link orgListItems}. */
  unclassified: boolean;
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
  imports: [
    RouterLink,
    MurSpinnerComponent,
    LockBadgeComponent,
    MoveToMenuComponent,
    MeetingsViewSwitcherComponent,
    MeetingsTableViewComponent,
  ],
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
  private readonly meetingsStore = inject(MeetingsListStore);
  private readonly orgBrain = inject(OrgBrainService);
  private readonly savedViews = inject(SavedViewsService);

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

  // --- No-query meetings list (root-persisted — see MeetingsListStore's doc:
  // survives a destroy+recreate so returning to /library shows the last-known
  // rows instantly instead of a reload flash) --------------------------------
  readonly meetings = this.meetingsStore.meetings;
  readonly loading = this.meetingsStore.loading;

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
      // A folder pick is a THIRD mutually-exclusive scope alongside tag/org —
      // always drop the org chip selection here too (mirrors NotesHomeComponent's
      // `_clearOrgOnFolderChange`), so the content pane never shows two scopes.
      this.activeOrgId.set(null);
      if (fid !== null) {
        this.activeTag.set(null);
        this.tagMeetings.set([]);
        this.tagLoading.set(false);
      }
    });
  });

  // --- Org (Shared Brain) chip row — "Shared Brains" meeting lists ---------
  // The RAW org roster/items/loading state lives in the shared, root-persisted
  // OrgBrainService now (was: a component-local signal wiped to empty on every
  // destroy+recreate — the stale-while-revalidate fix, 2026-07-12). This
  // component keeps only its OWN derived view + chip-row selection.
  /** Every org (Shared Brain) this user belongs to — the content pane's "Shared brains" chip row. */
  readonly orgs = this.orgBrain.orgs;
  /** True while the org list + items are (re)loading (chip-row hint only — never a render gate). */
  readonly orgsLoading = this.orgBrain.loading;
  /**
   * Selected org id, chosen via the content pane's "Shared brains" chip row
   * (null ⇒ not viewing a specific org). MUTUALLY
   * exclusive with a folder/tag selection: selecting an org clears both back to
   * the "All meetings" root, and vice-versa — the content pane has exactly one
   * active scope, and an org's items are NEVER merged into "All meetings" (the
   * bug #259 fixed).
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
   * enabled here), fall back to "All meetings" — mirrors the reset the OLD
   * per-component `loadOrgs()` used to do inline; now reactive over the
   * SHARED roster since a load can land from either this view or Notes'.
   */
  private readonly _clearMissingActiveOrg = effect(() => {
    const orgs = this.orgBrain.orgs();
    const sel = this.activeOrgId();
    if (sel !== null && !orgs.some((o) => o.orgId === sel)) {
      untracked(() => this.activeOrgId.set(null));
    }
  });

  /**
   * The active org's items narrowed to `kind === "meeting"` PLUS any
   * unclassified item (`kind == null` — shared before the meeting/document
   * distinction existed). Live-found bug (2026-07-12): a genuinely-shared
   * meeting whose share predates `source_kind` was silently EXCLUDED here
   * (only a passive "N items not shown" note hinted it existed) — shared
   * content must never just vanish, so an unclassified item is now included
   * and badged `unclassified: true` (the template shows it distinctly, never
   * as a confirmed meeting) rather than hidden. A `kind === "document"` item
   * still never appears here (it belongs in Notes).
   */
  readonly orgListItems = computed<OrgMeetingListItem[]>(() => {
    const org = this.activeOrg();
    if (!org) {
      return [];
    }
    const items = this.orgBrain.orgItems()[org.orgId] ?? [];
    return items
      .filter((item) => item.kind === "meeting" || item.kind == null)
      .map((item) => this.toOrgMeetingCard(item, org.name))
      .sort((a, b) => b.sortAt - a.sortAt);
  });

  /**
   * Open an org meeting card as a tracked tab — the editable original for the
   * author, else the read-only `/org-item/:id` viewer tab. Live-found bug,
   * 2026-07-12: this used to be a plain `[routerLink]`, so it never
   * registered with {@link TabsService} (opened but never appeared in the tab
   * strip, unlike an owned meeting). Mirrors `openMeeting`'s `<a>` +
   * `event.preventDefault()` shape (and `NotesHomeComponent.openOrgCard`).
   */
  openOrgCard(event: Event, item: OrgItemHeader): void {
    event.preventDefault();
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

  /** Build an org meeting card from a header + its origin org name. */
  private toOrgMeetingCard(item: OrgItemHeader, orgName: string): OrgMeetingListItem {
    const t = Date.parse(item.createdAt);
    return {
      kind: "org",
      id: `org:${item.itemId}`,
      sortAt: Number.isNaN(t) ? 0 : t,
      item,
      orgName,
      unclassified: item.kind == null,
    };
  }

  /** (Re)load the org roster + items — delegates to the shared {@link OrgBrainService}. */
  async loadOrgs(): Promise<void> {
    await this.orgBrain.loadOrgs();
  }

  /**
   * Select an org chip (Shared Brain): the pane shows ONLY that org's shared
   * meetings, in a list distinct from "All meetings". Mutually exclusive with a
   * folder/tag selection — clears both back to the root + closes any open
   * row menu / move popover / delete confirm.
   */
  selectOrg(orgId: string): void {
    this.cancelDelete();
    this.rowMenuId.set(null);
    this.movePopoverId.set(null);
    // Clears the SHARED sidebar-tree folder selection too (not just this
    // component's tag/org state) — the folder tree now lives outside this
    // component (`MeetingsSidebarTreeComponent`), so its selection can only be
    // reset through `FoldersService`.
    this.folders.selectFolder(null);
    this.activeTag.set(null);
    this.tagMeetings.set([]);
    this.tagLoading.set(false);
    this.activeOrgId.set(orgId);
  }

  /** Clear the org selection back to "All meetings" (mirrors `selectFolder(null)`). */
  clearOrgSelection(): void {
    this.activeOrgId.set(null);
  }

  // --- Own-meeting org-share badges (Library row + Detail) ------------------
  /**
   * Every active meeting→org share pairing across ALL of the caller's OWN
   * meetings (`listMeetingOrgShares`, bulk — avoids an N+1 per-row fetch).
   * Loaded alongside the org roster (shared {@link OrgBrainService}); empty
   * for a meeting never shared, or masked away server-side for a locked one.
   */
  /** `meetingId` → the orgs it's shared into — O(1) lookup for the row badge. */
  readonly orgSharesByMeetingId = computed(() => {
    const map = new Map<string, MeetingOrgShareRow[]>();
    for (const row of this.orgBrain.myOrgShares()) {
      const list = map.get(row.meetingId);
      if (list) {
        list.push(row);
      } else {
        map.set(row.meetingId, [row]);
      }
    }
    return map;
  });
  /** The orgs a meeting is shared into (empty when never shared). Drives the row badge. */
  orgSharesOf(meetingId: string): MeetingOrgShareRow[] {
    return this.orgSharesByMeetingId().get(meetingId) ?? [];
  }

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
  /** All distinct tags across meetings; empty → no filter bar is rendered.
   * Root-persisted (MeetingsListStore) for the same reason as {@link meetings}. */
  readonly tags = this.meetingsStore.tags;
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
        statusPillClass: meetingStatusPillClass(meeting.status),
        statusLabel: meetingStatusLabel(meeting.status),
      }))
      .sort((a, b) => b.sortAt - a.sortAt),
  );

  /** True when the no-query list has zero rows (drives the empty state). */
  readonly listEmpty = computed(() => this.listItems().length === 0);

  // --- Saved views (Feature B — Table + Board over the meetings list) -------
  /**
   * The currently-active saved view (null ⇒ the plain List default — the
   * existing rendering is then byte-identical). Root-persisted in
   * {@link SavedViewsService} so it survives the list route's destroy+recreate.
   */
  readonly activeSavedView = this.savedViews.activeView;
  /**
   * Per-meeting open/done action-item counts, merged into the Table/Board
   * views. Root-persisted alongside the roster; empty until loaded (a locked
   * meeting is omitted server-side → its row shows no counts).
   */
  readonly actionSummaries = this.savedViews.actionSummaries;

  /**
   * The rows a saved TABLE view renders — the ViewEngine applies the active
   * view's parsed config (filter/sort) over the SAME `displayedMeetings` the
   * List uses (already gated; a masked meeting stays masked). `null` when no
   * view is active or the active view is a board.
   */
  readonly viewTableRows = computed(() => {
    const view = this.activeSavedView();
    if (!view) {
      return null;
    }
    // Board was removed (2026-07-14) — every saved view is a Table now. A legacy
    // row still on disk with `layout:"board"` falls through here and renders as a
    // table (no data loss, no dangling board branch).
    return ViewEngine.rows(
      this.displayedMeetings(),
      this.savedViews.configOf(view),
      this.actionSummaries(),
      this.folderNameFn,
    );
  });

  /** The active view's visible column ids (table only; safe default otherwise). */
  readonly viewColumns = computed<string[]>(() => {
    const view = this.activeSavedView();
    return view ? this.savedViews.configOf(view).columns : [];
  });

  /**
   * Bound function references handed to the Table/Board child inputs (they
   * need `this` bound since Angular calls them detached from the instance).
   * All three are pure display resolvers over already-loaded state.
   */
  readonly folderNameFn = (m: Meeting): string | null => this.folderNameOf(m);
  readonly folderExposureFn = (m: Meeting): FolderExposure | null =>
    this.folderExposureOf(m);
  readonly isMaskedFn = (m: Meeting): boolean => this.isMasked(m);

  /** A Table/Board row asked to open a meeting — same tab-open path as the list. */
  onViewOpenMeeting(payload: { event: Event; meeting: Meeting }): void {
    this.openMeeting(
      payload.event,
      payload.meeting.id,
      payload.meeting.title,
    );
  }

  /** The switcher changed the active view / its config — refresh action counts lazily. */
  onViewChanged(): void {
    // The roster + config already live in SavedViewsService (the switcher drove
    // them); nothing to reload here except keep the action counts warm on first
    // activation. Cheap + idempotent (stale-while-revalidate).
    void this.savedViews.loadActionSummaries();
  }

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

  /** Heading for the no-query list: org name → folder name → tag → "Meetings". */
  readonly listHeading = computed(() => {
    const org = this.activeOrg();
    if (org !== null) {
      return org.name;
    }
    const fid = this.activeFolderId();
    if (fid !== null) {
      return this.folderById().get(fid)?.name ?? "Folder";
    }
    return this.activeTag() ?? "Meetings";
  });

  /** True when an org chip is selected and its meeting-kind list has zero rows. */
  readonly orgListEmpty = computed(() => this.orgListItems().length === 0);

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
  readonly displayedResults = computed<SearchRow[]>(() => {
    const hits = this.results();
    let narrowed = hits;
    if (this.activeTag() !== null) {
      const ids = this.tagMeetingIds();
      narrowed = hits.filter((h) => ids.has(h.meeting.id));
    }
    // The matched-in label and its tint are resolved HERE, once per hit, rather than by
    // template method calls inside the `@for` — a method binding re-runs on every change
    // detection pass for every visible row, and the rules table bans it outright.
    //
    // `matchedInLabel` returns "" for a value it cannot name, and the template renders NO
    // badge for "". The old `default: "title"` arm asserted the hit was in the TITLE for any
    // unrecognised value — a confident lie rather than an absent answer.
    return narrowed.map((hit) => ({
      ...hit,
      whereMatched: matchedInLabel(hit.matchedIn),
      badgeClass: matchBadgeClass(hit.matchedIn),
    }));
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

    // Live-refresh (org-feed-updated + window-focus) is subscribed ONCE, for the
    // app's lifetime, by the shared OrgBrainService now — no per-mount wiring here.

    // Load the meetings list, the tag set, the org chip row, the saved-view
    // roster + action counts in parallel; any one failing must not break the
    // others, so settle each independently. (The saved-view roster + counts
    // live in the root SavedViewsService — they survive this route's remount.)
    const [meetings] = await Promise.allSettled([
      this.ipc.listMeetings(),
      this.loadTags(),
      this.loadOrgs(),
      this.savedViews.load(),
      this.savedViews.loadActionSummaries(),
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
    // into an empty surprise. An org selection is a THIRD mutually-exclusive
    // scope — picking a tag drops it too.
    if (tag !== null) {
      this.folders.selectFolder(null);
    }
    this.activeOrgId.set(null);
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
    if (this.folders.isLocked(m.folderId)) {
      event.preventDefault();
      this.drag.end();
      this.toast.danger("Remove the folder lock before moving this recording.");
      return;
    }
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
   * Confirm the pending delete: await the IPC call (which moves the recording to
   * the Trash, recoverable for the retention window), then prune the
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
