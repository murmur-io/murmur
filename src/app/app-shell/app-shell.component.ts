import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import {
  NavigationEnd,
  Router,
  RouterLink,
  RouterOutlet,
} from "@angular/router";
import { filter, map } from "rxjs";

import { TabsService } from "../core/tabs.service";
import { AskHistoryPrivacyBarrierService } from "../core/ask-history-privacy-barrier.service";
import { IpcService } from "../core/ipc.service";
import type {
  WorkspaceOrganizeFailure,
  WorkspaceOrganizeMove,
} from "../core/models";
import {
  MurIconComponent,
  type ShellIcon,
} from "../design-system/icon/icon.component";
import { MurQuickSearchComponent } from "../design-system/quick-search/quick-search.component";
import { MurSidebarComponent } from "../design-system/sidebar/sidebar.component";
import { MurTabStripComponent } from "../design-system/tab-strip/tab-strip.component";
import { DocumentPreviewComponent } from "../features/brain/document-preview/document-preview.component";
import { TilePaletteComponent } from "../features/dashboards/tile-palette/tile-palette.component";
import { LockSharesDialogComponent } from "../features/folders/lock-shares-dialog/lock-shares-dialog.component";
import { ReminderComposerComponent } from "../features/reminders/reminder-composer/reminder-composer.component";
import { ReminderComposerService } from "../features/reminders/reminder-composer/reminder-composer.service";
import { RemindersStore } from "../features/reminders/reminders.store";
import { AccountSessionBannerComponent } from "../features/sharing/account-session-banner/account-session-banner.component";
import { FilingRecoveryBannerComponent } from "../features/workspace/filing-recovery-banner/filing-recovery-banner.component";
import { WorkspaceService } from "../features/workspace/workspace.service";
import {
  WorkspaceCreateSheetComponent,
  type WorkspaceCreateRequest,
} from "../features/workspace/workspace-create-sheet/workspace-create-sheet.component";
import { workspaceDestinations } from "../features/workspace/workspace-destination";
import { WorkspaceOrganizeSheetComponent } from "../features/workspace/workspace-organize-sheet/workspace-organize-sheet.component";
import type {
  WorkspaceOrganizeAttemptReceipt,
  WorkspaceOrganizeViewPlan,
} from "../features/workspace/workspace-organize-sheet/workspace-organize-sheet.component";
import { WorkspaceTreeComponent } from "../features/workspace/workspace-tree/workspace-tree.component";
import { DocumentPreviewService } from "../services/document-preview.service";
import { FolderLockFlowService } from "../services/folder-lock-flow.service";
import { FoldersService } from "../services/folders.service";
import { NotesService } from "../services/notes.service";
import { TilePaletteService } from "../services/tile-palette.service";
import { ToastService, type Toast } from "../services/toast.service";

interface BrowseItem {
  readonly path: string;
  readonly label: string;
  readonly icon: ShellIcon;
  readonly group: "Work" | "Intelligence" | "Insights";
}

const BROWSE_ITEMS: readonly BrowseItem[] = [
  { path: "/library", label: "Meetings", icon: "meetings", group: "Work" },
  { path: "/notes", label: "Notes", icon: "notes", group: "Work" },
  { path: "/tasks", label: "Tasks", icon: "tasks", group: "Work" },
  {
    path: "/dashboards",
    label: "Dashboards",
    icon: "dashboards",
    group: "Work",
  },
  { path: "/reminders", label: "Reminders", icon: "reminders", group: "Work" },
  { path: "/brain", label: "Brain", icon: "brain", group: "Intelligence" },
  {
    path: "/analytics",
    label: "Analytics",
    icon: "analytics",
    group: "Insights",
  },
  { path: "/graph", label: "Graph", icon: "graph", group: "Insights" },
  { path: "/people", label: "People", icon: "people", group: "Insights" },
];

const BROWSE_GROUPS = ["Work", "Intelligence", "Insights"] as const;
/**
 * The sidebar opens EXPANDED by default. It is now the ONLY navigation surface —
 * destinations, the workspace tree and the create actions all live in it — so a
 * collapsed first launch would hide the whole app behind a single icon.
 */
const SIDEBAR_EXPANDED_KEY = "murmur.shell.sidebarExpanded";
const BROWSE_EXPANDED_KEY = "murmur.shell.browseExpanded";

@Component({
  selector: "app-shell",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    RouterOutlet,
    RouterLink,
    MurIconComponent,
    MurQuickSearchComponent,
    MurSidebarComponent,
    MurTabStripComponent,
    WorkspaceTreeComponent,
    WorkspaceCreateSheetComponent,
    WorkspaceOrganizeSheetComponent,
    LockSharesDialogComponent,
    DocumentPreviewComponent,
    ReminderComposerComponent,
    TilePaletteComponent,
    AccountSessionBannerComponent,
    FilingRecoveryBannerComponent,
  ],
  host: {
    // Publishes the sidebar's CURRENT width to `--shell-content-inset` in
    // styles.css, so a `position: fixed` view (/settings) knows where the
    // content pane really starts instead of hardcoding a rail width.
    "[class.sidebar-collapsed]": "!sidebarExpanded()",
    "(document:keydown)": "onGlobalKeydown($event)",
    "(window:keydown.escape)": "onWindowEscape($event)",
  },
  templateUrl: "./app-shell.component.html",
  styleUrl: "./app-shell.component.scss",
})
export class AppShellComponent {
  private readonly folders = inject(FoldersService);
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);
  private readonly privacyBarrier = inject(AskHistoryPrivacyBarrierService);
  private readonly notesService = inject(NotesService);
  private readonly toast = inject(ToastService);
  private readonly router = inject(Router);
  private readonly tabs = inject(TabsService);
  private readonly reminders = inject(RemindersStore);
  private readonly reminderComposer = inject(ReminderComposerService);
  protected readonly workspace = inject(WorkspaceService);

  readonly reminderCount = this.reminders.dueInboxCount;
  readonly lockFlow = inject(FolderLockFlowService);
  readonly docPreview = inject(DocumentPreviewService);
  readonly tilePalette = inject(TilePaletteService);

  private readonly currentUrl = toSignal(
    this.router.events.pipe(
      filter((event): event is NavigationEnd => event instanceof NavigationEnd),
      map(() => this.router.url),
    ),
    { initialValue: this.router.url },
  );

  readonly currentPath = computed(() => this.currentUrl().split(/[?#]/)[0]);

  private readonly _sidebarExpanded = signal(
    readStoredBoolean(SIDEBAR_EXPANDED_KEY, true),
  );
  /**
   * Expanded shows labels, the Browse group and the workspace tree; collapsed
   * narrows to icons only. Persisted, because a nav width the user has to re-set
   * on every launch is worse than no toggle at all.
   */
  readonly sidebarExpanded = this._sidebarExpanded.asReadonly();

  private readonly _browseExpanded = signal(
    readStoredBoolean(BROWSE_EXPANDED_KEY, false),
  );
  /** Disclosure state of the Browse group (Meetings, Notes, Tasks, …). */
  readonly browseExpanded = this._browseExpanded.asReadonly();

  /** The create menu behind the caret beside "New note". */
  readonly createMenuOpen = signal(false);

  readonly browseItems = BROWSE_ITEMS;
  readonly browseGroups = BROWSE_GROUPS;
  readonly searchOpen = signal(false);
  readonly unlockedCount = this.folders.unlockedCount;
  readonly relockingAll = signal(false);
  readonly toasts = this.toast.toasts;
  readonly workspaceOrganizePlanning = signal(false);
  readonly workspaceOrganizeApplying = signal(false);
  readonly workspaceOrganizePlan = signal<WorkspaceOrganizeViewPlan | null>(
    null,
  );
  private workspaceOrganizePlanGeneration = 0;
  readonly workspaceCreateOpen = signal(false);
  readonly workspaceCreateBusy = signal(false);
  readonly workspaceCreateError = signal<string | null>(null);
  readonly workspaceDestinations = computed(() =>
    workspaceDestinations(this.workspace.forest()),
  );
  readonly workspaceOrganizeDisabled = computed(
    () =>
      this.workspace.unfiledRecordings().total === 0 ||
      this.workspaceOrganizePlanning() ||
      this.workspaceOrganizeApplying(),
  );
  readonly relockAllAriaLabel = computed(() => {
    const count = this.unlockedCount();
    const noun = count === 1 ? "folder" : "folders";
    return `Re-seal all ${count} unlocked ${noun} now`;
  });

  private readonly _syncTabsStripHeight = effect(() => {
    const height = this.tabs.tabs().length > 0 ? "48px" : "0px";
    document.documentElement.style.setProperty("--tabs-strip-height", height);
  });

  constructor() {
    const unregisterPrivacy = this.privacyBarrier.registerInvalidator(() =>
      this.scrubWorkspaceOrganization(),
    );
    // Listener installation stays demand-driven in plan/apply below. Starting
    // it from the always-mounted shell would turn later route consumers into
    // implicit retries after a registration failure.
    this.destroyRef.onDestroy(() => {
      unregisterPrivacy();
      this.scrubWorkspaceOrganization();
    });
    void this.reminders.initSummary();
  }

  isCaptureActive(): boolean {
    return this.currentPath() === "/record";
  }

  isAskActive(): boolean {
    return this.currentPath().startsWith("/ask");
  }

  isSettingsActive(): boolean {
    return this.currentPath().startsWith("/settings");
  }

  isBrowseItemActive(path: string): boolean {
    return this.currentPath() === path;
  }

  itemsForGroup(group: BrowseItem["group"]): readonly BrowseItem[] {
    return this.browseItems.filter((item) => item.group === group);
  }

  openSearch(): void {
    this.searchOpen.set(true);
  }

  newNote(): void {
    this.searchOpen.set(false);
    this.closeCreateMenu();
    void this.router.navigate(["/notes/new"]);
  }

  /**
   * The footer's Quick note button — one click from anywhere to a blank note,
   * with no menu in the way. It lands on the SAME `/notes/new` create flow the
   * Create menu's "New note" uses (that route creates the draft and replaces
   * the URL with the real id), so there is one note-creation path, not two.
   */
  quickNote(): void {
    this.newNote();
  }

  openWorkspaceCreate(): void {
    this.workspaceCreateError.set(null);
    this.workspaceCreateOpen.set(true);
  }

  closeWorkspaceCreate(): void {
    if (this.workspaceCreateBusy()) {
      return;
    }
    this.workspaceCreateOpen.set(false);
    this.workspaceCreateError.set(null);
  }

  async createInWorkspace(request: WorkspaceCreateRequest): Promise<void> {
    if (this.workspaceCreateBusy()) {
      return;
    }
    this.workspaceCreateBusy.set(true);
    this.workspaceCreateError.set(null);
    try {
      if (request.kind === "space") {
        const id = await this.workspace.createSpace(request.name);
        this.workspaceCreateOpen.set(false);
        await this.router.navigate(["/container", id]);
        this.toast.success(`Created Workspace “${request.name}”`);
        return;
      }
      const target = request.target;
      if (!target) {
        this.workspaceCreateError.set("Choose a destination first.");
        return;
      }
      if (request.kind === "note") {
        const id = await this.workspace.createNote(
          target.container.id,
          request.name,
        );
        this.workspaceCreateOpen.set(false);
        await this.router.navigate(["/notes", id]);
      } else if (request.kind === "dashboard") {
        const id = await this.workspace.createDashboard(
          target.container.id,
          request.name,
        );
        this.workspaceCreateOpen.set(false);
        await this.router.navigate(["/dashboards", id]);
      } else {
        const id = await this.workspace.createFolder(
          target.container,
          request.name,
        );
        this.workspaceCreateOpen.set(false);
        await this.router.navigate(["/container", id]);
      }
      this.toast.success(`Created ${request.name} in ${target.label}`);
    } catch (error) {
      const detail = this.workspaceErrorMessage(error);
      this.workspaceCreateError.set(
        detail ||
          `Couldn’t create this ${request.kind}. Please check the name and try again.`,
      );
    } finally {
      this.workspaceCreateBusy.set(false);
    }
  }

  private workspaceErrorMessage(error: unknown): string | null {
    const raw =
      typeof error === "string"
        ? error
        : error && typeof error === "object" && "message" in error
          ? String((error as { message: unknown }).message)
          : "";
    const normalized = raw.replace(/^invalid argument:\s*/i, "").trim();
    return normalized ? normalized.slice(0, 240) : null;
  }

  toggleSidebar(): void {
    const next = !this._sidebarExpanded();
    this._sidebarExpanded.set(next);
    writeStoredBoolean(SIDEBAR_EXPANDED_KEY, next);
    if (!next) {
      this.createMenuOpen.set(false);
    }
  }

  /**
   * Reveal the sidebar from a collapsed rail row. The Workspaces and Shared
   * trees cannot render in a 100px column, so their collapsed rows open the
   * sidebar instead of navigating — the section they name is then right there.
   */
  expandSidebar(): void {
    if (!this._sidebarExpanded()) {
      this.toggleSidebar();
    }
  }

  toggleBrowse(): void {
    // Collapsed there is no room to render the group, so reveal the sidebar
    // first rather than toggling something the user cannot see.
    if (!this._sidebarExpanded()) {
      this.toggleSidebar();
      this._browseExpanded.set(true);
      writeStoredBoolean(BROWSE_EXPANDED_KEY, true);
      return;
    }
    const next = !this._browseExpanded();
    this._browseExpanded.set(next);
    writeStoredBoolean(BROWSE_EXPANDED_KEY, next);
  }

  toggleCreateMenu(): void {
    this.createMenuOpen.update((open) => !open);
  }

  closeCreateMenu(): void {
    this.createMenuOpen.set(false);
  }

  newDashboard(): void {
    this.closeCreateMenu();
    this.openWorkspaceCreate();
  }

  newReminder(): void {
    this.closeCreateMenu();
    this.reminderComposer.openCreate();
  }

  async planWorkspaceOrganization(
    guidance: string | null = null,
    replace = false,
  ): Promise<void> {
    if (
      this.workspaceOrganizeDisabled() ||
      (this.workspaceOrganizePlan() && !replace)
    ) {
      return;
    }
    const generation = ++this.workspaceOrganizePlanGeneration;
    this.workspaceOrganizePlanning.set(true);
    try {
      const privacyReady = await this.privacyBarrier.ensureReady();
      if (generation !== this.workspaceOrganizePlanGeneration) {
        return;
      }
      if (!privacyReady) {
        this.scrubWorkspaceOrganization();
        return;
      }
      const plan = await this.ipc.planWorkspaceOrganization(guidance);
      if (generation !== this.workspaceOrganizePlanGeneration) {
        return;
      }
      this.workspaceOrganizePlan.set({
        ...plan,
        receipt: null,
        applyError: null,
      });
    } catch {
      if (generation === this.workspaceOrganizePlanGeneration) {
        this.toast.danger(
          "Brain couldn’t plan the organization. Please try again.",
        );
      }
    } finally {
      if (generation === this.workspaceOrganizePlanGeneration) {
        this.workspaceOrganizePlanning.set(false);
      }
    }
  }

  async replanWorkspaceOrganization(guidance: string): Promise<void> {
    await this.planWorkspaceOrganization(guidance || null, true);
  }

  async applyWorkspaceOrganization(
    moves: WorkspaceOrganizeMove[],
  ): Promise<void> {
    if (
      moves.length === 0 ||
      this.workspaceOrganizePlanning() ||
      this.workspaceOrganizeApplying()
    ) {
      return;
    }
    const plan = this.workspaceOrganizePlan();
    if (!plan) {
      return;
    }
    const generation = this.workspaceOrganizePlanGeneration;
    this.workspaceOrganizeApplying.set(true);
    try {
      const privacyReady = await this.privacyBarrier.ensureReady();
      if (generation !== this.workspaceOrganizePlanGeneration) {
        return;
      }
      if (!privacyReady) {
        this.scrubWorkspaceOrganization();
        return;
      }
      const result = await this.ipc.applyWorkspaceOrganization(moves);
      if (generation !== this.workspaceOrganizePlanGeneration) {
        return;
      }
      await this.workspace.reload();
      if (generation !== this.workspaceOrganizePlanGeneration) {
        return;
      }
      const receipt = this.mergeWorkspaceOrganizeReceipt(
        plan.receipt,
        moves,
        result.appliedIds,
        result.failures,
      );
      if (receipt.failures.length === 0) {
        this.workspaceOrganizePlan.set(null);
      } else {
        this.workspaceOrganizePlan.set({
          ...plan,
          receipt,
          applyError: null,
        });
      }
      const applied = result.appliedIds.length;
      const failed = receipt?.failures.length ?? result.failures.length;
      if (failed > 0) {
        const appliedCopy =
          applied === 0
            ? "No recordings organized"
            : `${applied} ${applied === 1 ? "recording" : "recordings"} organized`;
        this.toast.danger(
          `${appliedCopy}; ${failed} still need attention. Review the filing result.`,
        );
      } else {
        this.toast.success(
          `${applied} ${applied === 1 ? "recording" : "recordings"} organized`,
        );
      }
    } catch {
      if (generation !== this.workspaceOrganizePlanGeneration) {
        return;
      }
      await this.workspace.reload();
      if (generation !== this.workspaceOrganizePlanGeneration) {
        return;
      }
      this.workspaceOrganizePlan.update((plan) =>
        plan
          ? {
              ...plan,
              applyError:
                "The filing request didn’t finish. Review the selected moves and try again.",
            }
          : plan,
      );
      this.toast.danger(
        "Couldn’t finish applying the Brain plan. Refresh to see what moved.",
      );
    } finally {
      if (generation === this.workspaceOrganizePlanGeneration) {
        this.workspaceOrganizeApplying.set(false);
      }
    }
  }

  closeWorkspaceOrganization(): void {
    if (this.workspaceOrganizePlanning() || this.workspaceOrganizeApplying()) {
      return;
    }
    this.scrubWorkspaceOrganization();
  }

  /** Synchronous privacy boundary for the global Brain organizer review. */
  private scrubWorkspaceOrganization(): void {
    ++this.workspaceOrganizePlanGeneration;
    this.workspaceOrganizePlanning.set(false);
    this.workspaceOrganizeApplying.set(false);
    this.workspaceOrganizePlan.set(null);
  }

  private mergeWorkspaceOrganizeReceipt(
    previous: WorkspaceOrganizeAttemptReceipt | null | undefined,
    attemptedMoves: readonly WorkspaceOrganizeMove[],
    appliedIds: readonly string[],
    failures: readonly WorkspaceOrganizeFailure[],
  ): WorkspaceOrganizeAttemptReceipt {
    const moves = new Map(
      (previous?.moves ?? []).map((move) => [move.itemId, move]),
    );
    const applied = new Set(previous?.appliedIds ?? []);
    const unresolved = new Map(
      (previous?.failures ?? []).map((failure) => [failure.itemId, failure]),
    );

    for (const move of attemptedMoves) {
      moves.set(move.itemId, move);
      unresolved.delete(move.itemId);
    }
    for (const itemId of appliedIds) {
      applied.add(itemId);
      unresolved.delete(itemId);
    }
    for (const failure of failures) {
      if (!applied.has(failure.itemId)) {
        unresolved.set(failure.itemId, failure);
      }
    }

    return {
      moves: [...moves.values()],
      appliedIds: [...applied],
      failures: [...unresolved.values()],
    };
  }

  async relockAll(): Promise<void> {
    if (this.relockingAll()) {
      return;
    }
    this.relockingAll.set(true);
    try {
      await this.folders.relockAll();
      await this.notesService.loadFolders();
      await this.workspace.reload();
      this.toast.success("All folders re-sealed");
    } catch {
      this.toast.danger("Couldn’t re-seal folders. Please try again.");
    } finally {
      this.relockingAll.set(false);
    }
  }

  onGlobalKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape" && this.searchOpen()) {
      this.searchOpen.set(false);
      return;
    }
    if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey) {
      return;
    }
    const key = event.key.toLowerCase();
    if (key === "k") {
      event.preventDefault();
      this.searchOpen.update((value) => !value);
    } else if (key === "n" || key === "t") {
      event.preventDefault();
      this.newNote();
    } else if (key === "w") {
      const activeId = this.tabs.activeTabId();
      if (activeId) {
        event.preventDefault();
        void this.tabs.closeTab(activeId);
      }
    }
  }

  onWindowEscape(event: Event): void {
    event.preventDefault();
  }

  onOutletDetach(component: unknown): void {
    const tabAware = component as { onTabBackgrounded?: () => void } | null;
    tabAware?.onTabBackgrounded?.();
  }

  dismissToast(id: number): void {
    this.toast.dismiss(id);
  }

  runToastAction(toast: Toast): void {
    toast.action?.run();
    this.dismissToast(toast.id);
  }
}

function readStoredBoolean(key: string, fallback: boolean): boolean {
  try {
    const raw = localStorage.getItem(key);
    return raw === null ? fallback : raw === "true";
  } catch {
    return fallback;
  }
}

function writeStoredBoolean(key: string, value: boolean): void {
  try {
    localStorage.setItem(key, String(value));
  } catch {
    // Sidebar collapse is a convenience; storage failure must not break navigation.
  }
}
