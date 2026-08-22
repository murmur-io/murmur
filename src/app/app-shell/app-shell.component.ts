import {
  ChangeDetectionStrategy,
  Component,
  Injector,
  afterNextRender,
  computed,
  effect,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import {
  NavigationEnd,
  Router,
  RouterLink,
  RouterLinkActive,
  RouterOutlet,
} from "@angular/router";
import { filter, map } from "rxjs";
import { isDrilldownRoute } from "../core/nav-history.service";
import { TabsService } from "../core/tabs.service";
import {
  MurIconComponent,
  type ShellIcon,
} from "../design-system/icon/icon.component";
import { MurKbdComponent } from "../design-system/kbd/kbd.component";
import { MurQuickSearchComponent } from "../design-system/quick-search/quick-search.component";
import { MurSidebarComponent } from "../design-system/sidebar/sidebar.component";
import { MurSidebarSectionComponent } from "../design-system/sidebar-section/sidebar-section.component";
import { MurTabStripComponent } from "../design-system/tab-strip/tab-strip.component";
import { DocumentPreviewComponent } from "../features/brain/document-preview/document-preview.component";
import { TilePaletteComponent } from "../features/dashboards/tile-palette/tile-palette.component";
import { LockSharesDialogComponent } from "../features/folders/lock-shares-dialog/lock-shares-dialog.component";
import { MeetingsSidebarTreeComponent } from "../features/folders/meetings-sidebar-tree/meetings-sidebar-tree.component";
import { NotesSidebarTreeComponent } from "../features/notes/notes-sidebar-tree/notes-sidebar-tree.component";
import { ReminderComposerComponent } from "../features/reminders/reminder-composer/reminder-composer.component";
import { RemindersStore } from "../features/reminders/reminders.store";
import { ChromeService } from "../services/chrome.service";
import { DocumentPreviewService } from "../services/document-preview.service";
import { FolderLockFlowService } from "../services/folder-lock-flow.service";
import { FoldersService } from "../services/folders.service";
import { NotesService } from "../services/notes.service";
import { TilePaletteService } from "../services/tile-palette.service";
import { ToastService, type Toast } from "../services/toast.service";
import { AccountSessionBannerComponent } from "../features/sharing/account-session-banner/account-session-banner.component";

/** localStorage key for the chrome mode: "1" = pill bar, "0" = sidebar. */
const SIDEBAR_KEY = "murmur-sidebar-collapsed";

/** localStorage key for the Insights sidebar group: "1" = expanded. */
const INSIGHTS_KEY = "murmur-sidebar-insights";

/**
 * localStorage key for the Notes nav row's note-folder tree: "1" = expanded.
 * Default OPEN (Obsidian shows its vault tree by default) — unlike Insights,
 * which defaults collapsed.
 */
const NOTES_TREE_KEY = "murmur-sidebar-notes-tree";

/**
 * localStorage key for the Meetings nav row's folder tree: "1" = expanded.
 * Default OPEN — mirrors `NOTES_TREE_KEY` (Stage 2 of the always-visible-
 * sidebar work, 2026-07-12).
 */
const MEETINGS_TREE_KEY = "murmur-sidebar-meetings-tree";

/** A primary navigation destination. `icon` selects the inline SVG. */
interface NavItem {
  readonly path: string;
  readonly label: string;
  readonly icon: ShellIcon;
}

/** A labeled cluster of destinations in the EXPANDED sidebar only. */
interface NavGroup {
  readonly label: string | null;
  readonly collapsible?: boolean;
  readonly items: readonly NavItem[];
}

/**
 * QUIET GLASS sidebar grouping: Record (the app's primary act) solo on top,
 * then labeled clusters; the analytical destinations collapse under Insights.
 * The PILL BAR is untouched — it keeps the flat item list (NAV_ITEMS below).
 */
const NAV_GROUPS: readonly NavGroup[] = [
  {
    label: null,
    items: [{ path: "/record", label: "Record", icon: "record" }],
  },
  {
    label: "Workspace",
    items: [
      { path: "/library", label: "Meetings", icon: "meetings" },
      { path: "/notes", label: "Notes", icon: "notes" },
      { path: "/dashboards", label: "Dashboards", icon: "dashboards" },
      { path: "/tasks", label: "Tasks", icon: "tasks" },
      { path: "/reminders", label: "Reminders", icon: "reminders" },
    ],
  },
  {
    label: "Assistant",
    items: [
      { path: "/ask", label: "Ask", icon: "ask" },
      { path: "/brain", label: "Brain", icon: "brain" },
    ],
  },
  {
    label: "Insights",
    collapsible: true,
    items: [
      { path: "/analytics", label: "Analytics", icon: "analytics" },
      { path: "/graph", label: "Graph", icon: "graph" },
      { path: "/people", label: "People", icon: "people" },
    ],
  },
];

/**
 * The flat destination list for the PILL BAR — kept in the pill bar's
 * PRE-EXISTING order (the grouping above reorders only the expanded sidebar;
 * the collapsed chrome must not silently reshuffle).
 */
const NAV_ITEMS: readonly NavItem[] = [
  { path: "/record", label: "Record", icon: "record" },
  { path: "/library", label: "Meetings", icon: "meetings" },
  { path: "/notes", label: "Notes", icon: "notes" },
  { path: "/dashboards", label: "Dashboards", icon: "dashboards" },
  { path: "/tasks", label: "Tasks", icon: "tasks" },
  { path: "/reminders", label: "Reminders", icon: "reminders" },
  { path: "/analytics", label: "Analytics", icon: "analytics" },
  { path: "/graph", label: "Graph", icon: "graph" },
  { path: "/people", label: "People", icon: "people" },
  { path: "/brain", label: "Brain", icon: "brain" },
  { path: "/ask", label: "Ask", icon: "ask" },
];

/** Routes that live inside the collapsible Insights group. */
const INSIGHT_PATHS = NAV_GROUPS.filter((g) => g.collapsible).flatMap((g) =>
  g.items.map((i) => i.path),
);

/**
 * PROTOTYPE (Apple TV iPadOS shell) — the app chrome in two liquid-glass modes:
 *
 * - **Sidebar mode** — a FLOATING rounded glass panel (inset from the window
 *   edges, Apple TV's opened sidebar), with quick actions (Search ⌘K, New
 *   note ⌘N) pinned under the brand.
 * - **Pill mode** — the sidebar collapses into a floating top-center pill bar
 *   (Apple TV's collapsed state): sidebar toggle · icon nav (the active route
 *   shows its label) · search · new note · settings.
 *
 * ⌘K opens the quick-search spotlight and ⌘N starts a new note from anywhere
 * (both also on Ctrl for completeness). Light/dark ride the existing theme
 * tokens untouched.
 *
 * Why this is a separate child component (and not just AppComponent's
 * template): it is the Tauri WKWebView FOUC fix — see the original rationale
 * below; the shell CSS stays GLOBAL in styles.css, matched by class.
 */
@Component({
  selector: "app-shell",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    RouterOutlet,
    RouterLink,
    RouterLinkActive,
    MurIconComponent,
    MurKbdComponent,
    MurQuickSearchComponent,
    MurSidebarComponent,
    MurTabStripComponent,
    MeetingsSidebarTreeComponent,
    NotesSidebarTreeComponent,
    MurSidebarSectionComponent,
    LockSharesDialogComponent,
    DocumentPreviewComponent,
    ReminderComposerComponent,
    TilePaletteComponent,
    AccountSessionBannerComponent,
  ],
  host: {
    // Scoped to !inDrilldown so the pill-clearance padding never leaks onto
    // drill-down routes (which render no pill bar). Rail-style collapse keeps
    // the sidebar in the flex row, so it needs no clearance either.
    "[class.pill-mode]": "barMode() && !inDrilldown()",
    // True exactly when the floating sidebar/rail actually renders (mirrors
    // the template's own `@if (!inDrilldown()) { @if (!barMode()) { … } }`
    // gate) — drives `.main-col`'s top offset (styles.css) so the tab strip
    // lines up with the sidebar's first content row instead of sitting
    // noticeably higher (2026-07-12 fix). Deliberately excludes drill-down
    // AND pill/bar mode: neither renders the floating sidebar this alignment
    // targets, and drill-down routes read `--tabs-strip-height` (a literal,
    // not this margin) for their own fixed-host offset — adding the margin
    // there too would desync the two.
    "[class.sidebar-visible]": "!inDrilldown() && !barMode()",
    // True on drill-down routes (/settings, /org-item), where NO sidebar/pill
    // chrome renders and `.main-col` spans from the window's LEFT edge —
    // `mur-tab-strip` reads this via `:host-context` to reserve clearance for
    // the overlay macOS traffic lights, which otherwise sit on the first tab
    // (2026-07-17 fix; see tab-strip.component.scss).
    "[class.drilldown]": "inDrilldown()",
    // (The former `notes-wide-route` binding is GONE, 2026-07-12: `.app-main`
    // is `max-width: none; width: 100%` for EVERY main route now — see the
    // `.app-main` comment in styles.css. Views that want a narrower reading
    // column own that cap in their own component scss.)
    "(document:keydown)": "onGlobalKeydown($event)",
    "(window:keydown.escape)": "onWindowEscape($event)",
  },
  templateUrl: "./app-shell.component.html",
  styleUrl: "./app-shell.component.scss",
})
export class AppShellComponent {
  private readonly folders = inject(FoldersService);
  private readonly notesService = inject(NotesService);
  private readonly toast = inject(ToastService);
  private readonly router = inject(Router);
  private readonly chrome = inject(ChromeService);
  private readonly tabs = inject(TabsService);
  private readonly injector = inject(Injector);
  private readonly reminders = inject(RemindersStore);
  readonly reminderCount = this.reminders.dueInboxCount;

  /**
   * Shared lock×shares flow (probe → warn/revoke dialog → lock) — rendered
   * exactly ONCE here (2026-07-12 fix), regardless of which sidebar tree
   * (`NotesSidebarTreeComponent` / `MeetingsSidebarTreeComponent`'s folder
   * rows) triggered it. Both used to render their OWN `<app-lock-shares-
   * dialog>` bound to this same root-singleton service — harmless while only
   * one tree existed, but the main sidebar now ALWAYS mounts both
   * simultaneously, so a single lock request rendered TWO dialogs (caught by
   * `e2e/org/org-surfaces.spec.ts`'s strict-mode-violation failure).
   */
  readonly lockFlow = inject(FolderLockFlowService);

  /**
   * The app-wide read-only document/note preview modal, hosted ONCE here so it's
   * reachable from every route (a Related/Suggested chip, a `[[wikilink]]`, a
   * full-brain-graph node — none of which have a document route). The template
   * binds its `target` into the SINGLE `<app-document-preview>` host below; every
   * "open a document" surface calls {@link DocumentPreviewService.open}.
   */
  readonly docPreview = inject(DocumentPreviewService);

  /**
   * The Add-a-tile palette's open state. The palette is rendered HERE (see the
   * template) rather than by the board — `TilePaletteService` documents why.
   */
  readonly tilePalette = inject(TilePaletteService);

  constructor() {
    void this.reminders.initSummary();
  }

  /**
   * The current URL, updated on every completed navigation. Seeded from
   * `router.url` so `inDrilldown` is correct on a cold deep-link.
   */
  private readonly currentUrl = toSignal(
    this.router.events.pipe(
      filter((e): e is NavigationEnd => e instanceof NavigationEnd),
      map(() => this.router.url),
    ),
    { initialValue: this.router.url },
  );

  /** True while on any drill-down route (`/settings*`, `/library*`). */
  readonly inDrilldown = computed(() => isDrilldownRoute(this.currentUrl()));

  /**
   * True on EVERY Notes route: `/notes` (the table list) and
   * `/notes/:id`/`/notes/new` (the editor). Drives
   * `NotesSidebarTreeComponent`'s `sectionActive` input — see
   * {@link isMeetingsRoute} for the full rationale (the two are symmetric).
   * Covers `/notes/new` too (`NoteEditorComponent` handles that path
   * directly, `app.routes.ts`). (Its former second job — the
   * `notes-wide-route` width escape — is gone: `.app-main` is uncapped on
   * every main route now, see styles.css.)
   */
  readonly isNotesRoute = computed(() => {
    // Strip a query/fragment before comparing so `/notes?x=y` still counts as
    // the exact list route rather than falling through to neither branch.
    const path = this.currentUrl().split(/[?#]/)[0];
    return path === "/notes" || path.startsWith("/notes/");
  });

  /**
   * True on EVERY Meetings route: `/library` (the list) and `/meeting/:id`
   * (the detail view) — mirrors {@link isNotesRoute}. Drives
   * `MeetingsSidebarTreeComponent`'s `sectionActive` input (2026-07-12 fix):
   * the tree's folder rows remembered the last-selected folder via the SHARED
   * `FoldersService.activeFolderId` regardless of which page was open, so
   * "All meetings"/a folder kept showing the active/selected pill even while
   * looking at `/notes` or `/record` — misleadingly claiming something was
   * "current" when nothing about Meetings was. Gating the VISUAL selected
   * state on this (while still remembering the folder internally so
   * returning to `/library` reselects it) makes both trees symmetric: fused
   * header+root pill while that section is the current route, fully plain
   * otherwise — never "sometimes fused, sometimes disconnected" depending on
   * which page happens to be open.
   */
  readonly isMeetingsRoute = computed(() => {
    const path = this.currentUrl().split(/[?#]/)[0];
    return path === "/library" || path.startsWith("/library/") || path.startsWith("/meeting/");
  });

  /** Primary destinations (Settings lives in the chrome footer / pill end). */
  readonly navItems: readonly NavItem[] = NAV_ITEMS;

  /** Sidebar-only grouping of the same destinations (pill bar stays flat). */
  readonly navGroups: readonly NavGroup[] = NAV_GROUPS;

  /** Persisted Insights-group preference (default collapsed). */
  private readonly _insightsOpen = signal(this.readStoredInsightsOpen());

  /**
   * Persisted Notes note-folder-tree preference (default EXPANDED — Obsidian
   * always shows its vault tree). Nested under the "Notes" nav row via
   * {@link NotesSidebarTreeComponent}.
   */
  private readonly _notesTreeOpen = signal(this.readStoredNotesTreeOpen());
  readonly notesTreeOpen = this._notesTreeOpen.asReadonly();

  /**
   * Persisted Meetings folder-tree preference (default EXPANDED). Nested
   * under the "Meetings" nav row via {@link MeetingsSidebarTreeComponent}
   * (Stage 2, 2026-07-12 — mirrors the Notes tree above).
   */
  private readonly _meetingsTreeOpen = signal(this.readStoredMeetingsTreeOpen());
  readonly meetingsTreeOpen = this._meetingsTreeOpen.asReadonly();

  /**
   * References to the two FOLDER-TREE-BODY components — resolve to
   * `undefined` while their section is collapsed (each only renders inside
   * `<mur-sidebar-section>`'s own `@if (expanded())`, projected as content).
   * The section header's compact "+" icon forwards into
   * {@link newNoteFolder}/{@link newMeetingFolder}; the header's vault-root
   * drop target forwards into {@link onMeetingsHeaderDrop}.
   */
  private readonly notesSidebarTree = viewChild(NotesSidebarTreeComponent);
  private readonly meetingsSidebarTree = viewChild(MeetingsSidebarTreeComponent);

  /**
   * Whether the Insights group renders expanded: the stored preference, OR
   * forced open while the CURRENT route lives inside it (the active pill must
   * never be hidden by a collapsed group).
   */
  readonly insightsExpanded = computed(
    () =>
      this._insightsOpen() ||
      INSIGHT_PATHS.some((p) => this.currentUrl().startsWith(p)),
  );

  /**
   * Chrome mode: false = floating sidebar, true = top pill bar. Persisted
   * under the pre-existing key (old "collapsed" preference maps naturally
   * onto the compact pill chrome).
   */
  private readonly _pillMode = signal(this.readStoredPillMode());
  readonly pillMode = this._pillMode.asReadonly();

  /**
   * How a COLLAPSED sidebar renders (Settings → Appearance → Sidebar):
   * `bar` (default) = the floating top pill bar; `rail` = a slim icon-only
   * rail docked at the left edge. The collapsed FLAG and its persistence
   * (murmur-sidebar-collapsed) are unchanged — only the rendering differs.
   */
  readonly collapseStyle = this.chrome.collapseStyle;

  /** Collapsed AND the top-bar style — render the pill bar. */
  readonly barMode = computed(
    () => this.pillMode() && this.collapseStyle() === "bar",
  );

  /** Collapsed AND the rail style — render the icon-only sidebar rail. */
  readonly railMode = computed(
    () => this.pillMode() && this.collapseStyle() === "rail",
  );

  /** Whether the ⌘K quick-search spotlight is open. */
  readonly searchOpen = signal(false);

  /**
   * How many locked folders are unlocked (plaintext-exposed) this session —
   * drives the footer "N unlocked · Lock all" button. `FoldersService`'s tree
   * comes from `list_folders`, which returns EVERY folder (meeting AND note —
   * no `kind` filter in the SQL), each with its session `unlocked` flag, so this
   * one count already covers both trees. (Do NOT add `NotesService`'s note-folder
   * count on top — note folders are already in this tree, so that double-counts.)
   */
  readonly unlockedCount = this.folders.unlockedCount;

  /** True while the footer "Lock all" re-seal is in flight (guards double-clicks). */
  readonly relockingAll = signal(false);

  /** The app-wide toast queue, rendered in the main-window viewport. */
  readonly toasts = this.toast.toasts;

  /** Persist the chrome-mode choice whenever it changes. */
  private readonly _persistPillMode = effect(() => {
    const value = this._pillMode();
    try {
      localStorage.setItem(SIDEBAR_KEY, value ? "1" : "0");
    } catch {
      // Private-mode / storage-disabled — the preference is not persisted.
    }
  });

  /** Persist the Insights-group preference whenever it changes. */
  private readonly _persistInsightsOpen = effect(() => {
    const value = this._insightsOpen();
    try {
      localStorage.setItem(INSIGHTS_KEY, value ? "1" : "0");
    } catch {
      // Private-mode / storage-disabled — the preference is not persisted.
    }
  });

  /** Persist the Notes-tree preference whenever it changes. */
  private readonly _persistNotesTreeOpen = effect(() => {
    const value = this._notesTreeOpen();
    try {
      localStorage.setItem(NOTES_TREE_KEY, value ? "1" : "0");
    } catch {
      // Private-mode / storage-disabled — the preference is not persisted.
    }
  });

  /** Persist the Meetings-tree preference whenever it changes. */
  private readonly _persistMeetingsTreeOpen = effect(() => {
    const value = this._meetingsTreeOpen();
    try {
      localStorage.setItem(MEETINGS_TREE_KEY, value ? "1" : "0");
    } catch {
      // Private-mode / storage-disabled — the preference is not persisted.
    }
  });

  /**
   * `--tabs-strip-height` on `<html>` — the ONE signal-driven source of
   * truth every drill-down's fixed `position: fixed` host (library / notes
   * home / note editor / settings) reads for its own `top` offset, so it
   * structurally leaves `mur-tab-strip`'s real height uncovered instead of
   * floating a per-route pixel guess on top of it (fixed 2026-07-12). A
   * fixed design constant, not content-measured — `mur-tab-strip`'s own
   * rendered height never varies (one row, no wrapping), so this is a plain
   * signal-driven toggle, same directness `GlassService` already uses for
   * `--glass-user-alpha`, not a `ResizeObserver`. MUST stay in px-sync with
   * `tab-strip.component.scss`'s `.tab-strip { height: … }`. */
  private readonly _syncTabsStripHeight = effect(() => {
    const height = this.tabs.tabs().length > 0 ? "48px" : "0px";
    document.documentElement.style.setProperty("--tabs-strip-height", height);
  });

  /** Toggle between the floating sidebar and the top pill bar. */
  togglePillMode(): void {
    this._pillMode.update((c) => !c);
  }

  /**
   * Toggle the Insights group. Uses the RENDERED state as the base so a click
   * always visibly inverts what the user sees (the group may be auto-expanded
   * by the active route while the stored preference says collapsed).
   */
  toggleInsights(): void {
    this._insightsOpen.set(!this.insightsExpanded());
  }

  /** Toggle the Notes nav row's nested note-folder tree. */
  toggleNotesTree(): void {
    this._notesTreeOpen.update((v) => !v);
  }

  /** Toggle the Meetings nav row's nested folder tree. */
  toggleMeetingsTree(): void {
    this._meetingsTreeOpen.update((v) => !v);
  }

  /**
   * The "Notes" section header's compact "+" icon — opens the note-folder
   * tree's inline "New folder" field. Expands the tree first if it's
   * collapsed (the tree component only exists in the DOM while open, so
   * {@link notesSidebarTree} resolves to `undefined` until then) —
   * `afterNextRender` defers the forward to AFTER that `@if` flips and the
   * component actually mounts.
   */
  newNoteFolder(): void {
    if (this.notesTreeOpen()) {
      this.notesSidebarTree()?.startCreateFolder();
      return;
    }
    this._notesTreeOpen.set(true);
    afterNextRender(() => this.notesSidebarTree()?.startCreateFolder(), {
      injector: this.injector,
    });
  }

  /** The "Meetings" section header's compact "+" icon — mirrors {@link newNoteFolder}. */
  newMeetingFolder(): void {
    if (this.meetingsTreeOpen()) {
      this.meetingsSidebarTree()?.openCreateFolder();
      return;
    }
    this._meetingsTreeOpen.set(true);
    afterNextRender(() => this.meetingsSidebarTree()?.openCreateFolder(), {
      injector: this.injector,
    });
  }

  /**
   * The "Notes" section HEADER was clicked (2026-07-12: the header IS the
   * "all items" affordance now — the separate "All notes" root row was
   * removed as a redundant layer). Clears the note-folder filter; the
   * header's own routerLink handles the `/notes` navigation.
   */
  onNotesHeaderSelect(): void {
    void this.notesService.selectFolder(null);
  }

  /** The "Meetings" section header was clicked — mirrors {@link onNotesHeaderSelect}. */
  onMeetingsHeaderSelect(): void {
    this.folders.selectFolder(null);
  }

  /**
   * A note was dropped onto the Meetings section HEADER (the vault-root drop
   * target moved onto the header when the "All meetings" root row was
   * removed, 2026-07-12) — forwards into
   * `MeetingsSidebarTreeComponent.onDropNote` (which owns the toast +
   * folder-name lookup) the same way the "+" forwarding does. Unlike the
   * "+", the header renders even while the tree is COLLAPSED — expand it
   * first in that case so the tree-body component exists to run the move
   * (and so the user sees where the note landed).
   */
  onMeetingsHeaderDrop(meetingId: string): void {
    if (this.meetingsTreeOpen()) {
      void this.meetingsSidebarTree()?.onDropNote({ meetingId, folderId: null });
      return;
    }
    this._meetingsTreeOpen.set(true);
    afterNextRender(
      () =>
        void this.meetingsSidebarTree()?.onDropNote({
          meetingId,
          folderId: null,
        }),
      { injector: this.injector },
    );
  }

  /** Open the ⌘K spotlight. */
  openSearch(): void {
    this.searchOpen.set(true);
  }

  /** Quick action: create a new standalone note and open its editor (⌘N). */
  newNote(): void {
    this.searchOpen.set(false);
    void this.router.navigate(["/notes/new"]);
  }

  /**
   * The footer "N unlocked · Lock all" button (2026-07-14): re-seal EVERY
   * session-unlocked folder now and zeroize the cached key — the sidebar's
   * single privacy action (the former top-of-tree "Lock all" pill was removed).
   * `FoldersService.relockAll` reloads the meetings tree; we then reload the
   * Notes tree too so a re-sealed note-folder's per-row state updates as well
   * (the backend `relock_all` re-seals both — folder ids share one session set).
   */
  async relockAll(): Promise<void> {
    if (this.relockingAll()) {
      return;
    }
    this.relockingAll.set(true);
    try {
      await this.folders.relockAll();
      await this.notesService.loadFolders();
      this.toast.success("All folders re-sealed");
    } catch {
      this.toast.danger("Couldn’t re-seal folders. Please try again.");
    } finally {
      this.relockingAll.set(false);
    }
  }

  /**
   * Global shortcuts: ⌘K/Ctrl+K toggles search, ⌘N/Ctrl+N new note, ⌘T/Ctrl+T
   * a new note TAB (same target as ⌘N — the browser-tab-bar convention, kept
   * as a distinct shortcut from ⌘N since the tab strip is its own mental
   * model), ⌘W/Ctrl+W closes the active tab (mirrors `mur-tab-strip`'s own ×
   * button) and is a NO-OP — never a window-close — when no tab is open
   * (`preventDefault()` only fires once a tab is actually found to close).
   * NOTE: no native macOS window/app menu is registered anywhere in
   * `src-tauri` (grepped `lib.rs` for `Menu`/`Accelerator` — the only `Menu`
   * usage is the unrelated tray/status-bar icon), so there is no known
   * competing native ⌘W accelerator for this JS handler to lose a race
   * against; this was NOT verified against the real signed/packaged app
   * (only `ng serve` + Playwright, which never exercises native window-menu
   * chrome at all) — confirm empirically in a live dev/signed build before
   * relying on this. Escape closes the spotlight at the DOCUMENT level — the
   * scrim's own handler only fires while focus sits inside it (e.g. it dies
   * once focus falls to <body>).
   */
  onGlobalKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape" && this.searchOpen()) {
      this.searchOpen.set(false);
      return;
    }
    if (!(e.metaKey || e.ctrlKey) || e.altKey || e.shiftKey) return;
    const key = e.key.toLowerCase();
    if (key === "k") {
      e.preventDefault();
      this.searchOpen.update((v) => !v);
    } else if (key === "n" || key === "t") {
      e.preventDefault();
      this.newNote();
    } else if (key === "w") {
      const activeId = this.tabs.activeTabId();
      if (activeId) {
        e.preventDefault();
        void this.tabs.closeTab(activeId);
      }
    }
  }

  /**
   * Consume Escape at the main-window shell boundary after document-level
   * overlay handlers have run. In native macOS fullscreen, an unconsumed Escape
   * falls through to window chrome (which can hide/minimize Murmur); the app's
   * own transient UI still receives the event first during normal bubbling.
   * The separate `/bar` window is outside this shell and keeps its intentional
   * Escape-to-hide behavior.
   */
  onWindowEscape(event: Event): void {
    event.preventDefault();
  }

  /**
   * Fires on every `<router-outlet>` DETACH-for-reuse (a tab switch away from
   * a `/meeting/:id` or `/notes/:id` route kept alive by
   * `TabRouteReuseStrategy` — NOT just a real destroy). Notifies the
   * backgrounded component so it can pause audio (tabs plan risk #3) and
   * collapse unbounded transcript DOM (perf-audit fix 2). Duck-typed, since
   * the shell (eagerly loaded) can't import a lazily-loaded feature
   * component's type — see `DetailComponent.onTabBackgrounded`.
   */
  onOutletDetach(component: unknown): void {
    const tabAware = component as { onTabBackgrounded?: () => void } | null;
    tabAware?.onTabBackgrounded?.();
  }

  /** Dismiss a toast by id (also cancels its auto-dismiss timer). */
  dismissToast(id: number): void {
    this.toast.dismiss(id);
  }

  /** Run a toast's inline action, then dismiss the toast. */
  runToastAction(t: Toast): void {
    t.action?.run();
    this.dismissToast(t.id);
  }

  /** Read the persisted chrome-mode preference; default sidebar. */
  private readStoredPillMode(): boolean {
    try {
      return localStorage.getItem(SIDEBAR_KEY) === "1";
    } catch {
      return false;
    }
  }

  /** Read the persisted Insights-group preference; default collapsed. */
  private readStoredInsightsOpen(): boolean {
    try {
      return localStorage.getItem(INSIGHTS_KEY) === "1";
    } catch {
      return false;
    }
  }

  /** Read the persisted Notes-tree preference; default EXPANDED. */
  private readStoredNotesTreeOpen(): boolean {
    try {
      return localStorage.getItem(NOTES_TREE_KEY) !== "0";
    } catch {
      return true;
    }
  }

  /** Read the persisted Meetings-tree preference; default EXPANDED. */
  private readStoredMeetingsTreeOpen(): boolean {
    try {
      return localStorage.getItem(MEETINGS_TREE_KEY) !== "0";
    } catch {
      return true;
    }
  }
}
