import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  OnInit,
  computed,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "../../../core/ipc.service";
import { NavHistoryService } from "../../../core/nav-history.service";
import { MurSidebarComponent } from "../../../design-system/sidebar/sidebar.component";
import { MurSpinnerComponent } from "../../../design-system/spinner/spinner.component";
import type {
  FolderNode,
  Meeting,
  MeetingOrgShareRow,
  MeetingStatus,
  OrgItemHeader,
  OrgStatus,
  SearchHit,
} from "../../../core/models";
import {
  FoldersService,
  type FolderExposure,
} from "../../../services/folders.service";
import { FolderTreeComponent } from "../../folders/folder-tree/folder-tree.component";
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
    MurSidebarComponent,
    MurSpinnerComponent,
    RouterLink,
    FolderTreeComponent,
    LockBadgeComponent,
    MoveToMenuComponent,
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

  /** Drill-down back navigation ("← Murmur" + Esc) — no library state coupling. */
  readonly nav = inject(NavHistoryService);

  /**
   * Esc while in Meetings. An open row ⋯ menu / move popover closes first (one
   * Esc = one dismissal); otherwise backs out to where you came from — EXCEPT
   * while you're typing: in the search box the first Esc clears it (or blurs
   * when empty), and Esc is ignored inside any other form field, so it never
   * ejects you mid-edit. Mirrors settings.component's onEscape.
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
      return;
    }
    const tag = el?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") {
      return;
    }
    this.nav.back();
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

  // --- Folder filter (left pane) ------------------------------------------
  /** The lock-aware folder forest from the signal store. */
  readonly folderTree = this.folders.tree;
  /** True while the folder tree is loading (drives the left-pane state). */
  readonly foldersLoading = this.folders.loading;
  /** How many sealed folders are session-unlocked right now (drives "Lock all"). */
  readonly unlockedCount = this.folders.unlockedCount;
  /** True while a "Lock all" op is in flight. */
  readonly relockingAll = signal(false);
  /**
   * Selected folder id (null = no folder filter — show the tag/all list).
   * Mutually exclusive with the tag filter: selecting one clears the other.
   */
  readonly activeFolderId = signal<string | null>(null);

  // --- Org (Shared Brain) rail — "Shared Brains" meeting lists -------------
  /**
   * Every org (Shared Brain) this user belongs to — the rail's "Shared brains"
   * section. Loaded stale-guarded on init, on the `org-feed-updated` event, and
   * on window focus. Deliberately separate from Notes' own org state (each view
   * loads independently; "notes has notes, meetings has meetings" — PR #259).
   */
  private readonly _orgs = signal<OrgStatus[]>([]);
  readonly orgs = this._orgs.asReadonly();
  /**
   * Each org's shared items keyed by orgId (`listOrgItems`) — UNFILTERED (both
   * kinds); {@link orgListItems} narrows to `kind === "meeting"` for display.
   */
  private readonly _orgItems = signal<Record<string, OrgItemHeader[]>>({});
  /**
   * Selected org id in the rail (null ⇒ not viewing a specific org). MUTUALLY
   * exclusive with a folder/tag selection: selecting an org clears both back to
   * the "All meetings" root, and vice-versa — the content pane has exactly one
   * active scope, and an org's items are NEVER merged into "All meetings" (the
   * bug #259 fixed).
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
   * The active org's items narrowed to `kind === "meeting"` — a DISTINCT list,
   * never merged into {@link listItems} ("All meetings" stays exactly what
   * #259 left it: the user's own recordings only). A `kind === "document"`
   * item never appears here (it belongs in Notes); a `kind == null`
   * (unclassified, pre-kind wire format) item is EXCLUDED from this
   * confirmed-meetings list too — see {@link orgUnclassifiedCount}.
   */
  readonly orgListItems = computed<OrgMeetingListItem[]>(() => {
    const org = this.activeOrg();
    if (!org) {
      return [];
    }
    const items = this._orgItems()[org.orgId] ?? [];
    return items
      .filter((item) => item.kind === "meeting")
      .map((item) => this.toOrgMeetingCard(item, org.name))
      .sort((a, b) => b.sortAt - a.sortAt);
  });

  /**
   * Count of the active org's items that are neither a confirmed meeting nor a
   * confirmed document (`kind == null` — shared under the pre-kind wire format).
   * Surfaced as a small "N unclassified" note rather than silently folding them
   * into the meeting list as if verified.
   */
  readonly orgUnclassifiedCount = computed(() => {
    const org = this.activeOrg();
    if (!org) {
      return 0;
    }
    const items = this._orgItems()[org.orgId] ?? [];
    return items.filter((item) => item.kind == null).length;
  });

  /** Router target for an org meeting card — the editable original for the author, else the read-only viewer. */
  orgItemLink(item: OrgItemHeader): string[] {
    const owned = item.ownedSource;
    if (owned) {
      return owned.kind === "meeting"
        ? ["/meeting", owned.id]
        : ["/notes", owned.id];
    }
    return ["/org-item", item.itemId];
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
    };
  }

  /** Released on destroy to detach the org-feed-updated live-refresh listener. */
  private orgFeedUnlisten: UnlistenFn | null = null;
  /** True once destroyed — a `listen()` resolving AFTER teardown releases immediately. */
  private orgFeedDestroyed = false;
  /** Bumped per org load so a late (stale) reload result is dropped (T1 guard). */
  private orgLoadSeq = 0;
  /** Bound window-focus handler — re-loads org items when the view regains focus. */
  private readonly onOrgWindowFocus = (): void => {
    void this.loadOrgs();
  };

  /**
   * (Re)load the org (Shared Brain) list + every org's shared items, stale-guarded
   * on {@link orgLoadSeq}, and the bulk own-meeting org-share pairings (for the
   * Library row badge). Best-effort throughout — a transient/offline error leaves
   * the last-known state standing. Never throws.
   */
  async loadOrgs(): Promise<void> {
    const seq = ++this.orgLoadSeq;
    this.orgsLoading.set(true);
    try {
      try {
        await this.ipc.orgRefresh();
      } catch {
        /* offline / no server → fall through to the local replica */
      }
      let orgs: OrgStatus[];
      try {
        // PER-INSTANCE ORG TOGGLE: the rail is a content BROWSER, not the management
        // surface (that's Settings → Organization, which fetches its own UNFILTERED
        // list so every joined org's toggle is reachable) — a disabled org must not
        // appear as a pickable "Shared brains" entry here at all, matching the user's
        // mental model ("F disabled ⇒ not used on this instance"). The actual content
        // gate is already backend-enforced (list_org_items_inner returns empty for a
        // disabled org); this filter just keeps the rail honest about what's usable.
        orgs = (await this.ipc.orgListStatuses()).filter((o) => o.contextEnabled);
      } catch {
        return; // keep the last-known orgs on a transient failure
      }
      if (seq !== this.orgLoadSeq) {
        return;
      }
      const [itemLists, ownShares] = await Promise.all([
        Promise.all(
          orgs.map((o) =>
            this.ipc.listOrgItems(o.orgId).catch(() => [] as OrgItemHeader[]),
          ),
        ),
        this.ipc.listMeetingOrgShares().catch(() => [] as MeetingOrgShareRow[]),
      ]);
      if (seq !== this.orgLoadSeq) {
        return;
      }
      const byOrg: Record<string, OrgItemHeader[]> = {};
      orgs.forEach((o, i) => {
        byOrg[o.orgId] = itemLists[i];
      });
      this._orgs.set(orgs);
      this._orgItems.set(byOrg);
      this._myOrgShares.set(ownShares);
      // If the rail's selected org has since disappeared, fall back to "All meetings".
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
   * meetings, in a list distinct from "All meetings". Mutually exclusive with a
   * folder/tag selection — clears both back to the root + closes any open
   * row menu / move popover / delete confirm.
   */
  selectOrg(orgId: string): void {
    this.cancelDelete();
    this.rowMenuId.set(null);
    this.movePopoverId.set(null);
    this.activeFolderId.set(null);
    this.activeTag.set(null);
    this.tagMeetings.set([]);
    this.tagLoading.set(false);
    this.activeOrgId.set(orgId);
  }

  /** Clear the org selection back to "All meetings" (mirrors `selectFolder(null)`). */
  clearOrgSelection(): void {
    this.activeOrgId.set(null);
  }

  /** A friendly role hint for the rail ("Owner" / "Member"). */
  orgRoleLabel(org: OrgStatus): string {
    return org.role === "owner" ? "Owner" : "Member";
  }

  // --- Own-meeting org-share badges (Library row + Detail) ------------------
  /**
   * Every active meeting→org share pairing across ALL of the caller's OWN
   * meetings (`listMeetingOrgShares`, bulk — avoids an N+1 per-row fetch).
   * Loaded alongside the org rail in {@link loadOrgs}; empty for a meeting
   * never shared, or masked away server-side for a locked one.
   */
  private readonly _myOrgShares = signal<MeetingOrgShareRow[]>([]);
  /** `meetingId` → the orgs it's shared into — O(1) lookup for the row badge. */
  readonly orgSharesByMeetingId = computed(() => {
    const map = new Map<string, MeetingOrgShareRow[]>();
    for (const row of this._myOrgShares()) {
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

  /** True when an org is rail-selected and its meeting-kind list has zero rows. */
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

  async ngOnInit(): Promise<void> {
    // Clean up any in-flight debounce timer when the view is torn down.
    this.destroyRef.onDestroy(() => {
      if (this.searchTimer) {
        clearTimeout(this.searchTimer);
      }
    });

    // Live-refresh: the background org-sync loop fires `org-feed-updated` on a
    // productive tick. Subscribe ONCE (push straight into a reload — NEVER
    // subscribe-into-a-field), and re-load on window focus too. Both cleaned up
    // on destroy (release the UnlistenFn + the focus handler).
    this.destroyRef.onDestroy(() => {
      this.orgFeedDestroyed = true;
      this.orgFeedUnlisten?.();
      this.orgFeedUnlisten = null;
      window.removeEventListener("focus", this.onOrgWindowFocus);
    });
    window.addEventListener("focus", this.onOrgWindowFocus);
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

    // Load the meetings list, the tag set, and the org rail in parallel; any one
    // failing must not break the others, so settle each independently.
    const [meetings] = await Promise.allSettled([
      this.ipc.listMeetings(),
      this.loadTags(),
      this.loadOrgs(),
    ]);
    if (meetings.status === "fulfilled") {
      this.meetings.set(meetings.value);
    }
    this.loading.set(false);
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
    // active folder so they never compose into an empty surprise. An org
    // selection is a THIRD mutually-exclusive scope — picking a tag drops it.
    if (tag !== null) {
      this.activeFolderId.set(null);
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

  // --- Folder filtering (left pane) ---------------------------------------

  /**
   * Select a folder (or `null` for "All notes" / the vault root). Mirrors the
   * tag-filter machinery: it dismisses any open delete confirm, clears the
   * mutually-exclusive tag filter, and (for a non-null folder) leaves the search
   * alone — the right pane re-derives `folderMeetings` reactively. A null target
   * (the tree's "All notes" row) returns to the full list. There is no async
   * fetch (folder filtering is client-side over `meetings`), so no latest-wins
   * race exists; the same idempotent-guard shape is kept for consistency.
   */
  selectFolder(folderId: string | null): void {
    // A re-select of the SAME folder while no org is active is a no-op — but a
    // re-select of "All meetings" WHILE an org is active still runs, to drop it.
    if (this.activeFolderId() === folderId && this.activeOrgId() === null) {
      return;
    }
    this.cancelDelete();
    this.rowMenuId.set(null);
    this.movePopoverId.set(null);
    // Folder + tag scopes are mutually exclusive — picking a folder clears the
    // tag selection (and its fetched list) so they never compose. An org
    // selection is a THIRD mutually-exclusive scope — always dropped here.
    if (folderId !== null) {
      this.activeTag.set(null);
      this.tagMeetings.set([]);
      this.tagLoading.set(false);
    }
    this.activeOrgId.set(null);
    this.activeFolderId.set(folderId);
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

  /** Re-seal every session-unlocked folder at once (privacy "panic" affordance). */
  async relockAll(): Promise<void> {
    if (this.relockingAll()) {
      return;
    }
    this.relockingAll.set(true);
    try {
      await this.folders.relockAll();
      this.toast.success("All folders re-sealed");
    } catch {
      this.toast.danger("Couldn’t re-seal folders. Please try again.");
    } finally {
      this.relockingAll.set(false);
    }
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

  /**
   * A note was dropped onto a folder (or "All notes"). Run the move through the
   * FoldersService (which owns the cross-encryption-boundary semantics + tree
   * reload), then reconcile the local list. A no-op when it's already there.
   */
  async onDropNote(payload: {
    meetingId: string;
    folderId: string | null;
  }): Promise<void> {
    const { meetingId, folderId } = payload;
    const current =
      this.meetings().find((m) => m.id === meetingId)?.folderId ?? null;
    if (current === folderId) {
      return; // already filed here — nothing to do.
    }
    try {
      await this.folders.moveNote(meetingId, folderId);
      await this.applyMove(meetingId, folderId);
      const name =
        folderId === null
          ? "All notes"
          : (this.folderById().get(folderId)?.name ?? "folder");
      this.toast.success(`Moved to ${name}`);
    } catch {
      this.toast.danger("Couldn’t move this note. Please try again.");
    }
  }

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
      this.activeFolderId.set(null);
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
