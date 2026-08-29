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

type ContextPanel = "spaces" | "browse" | "none";

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
const NARROW_SHELL_QUERY = "(max-width: 760px)";
const SPACES_COLLAPSED_KEY = "murmur.shell.spacesCollapsed";

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
    "[class.context-visible]": "contextPanel() !== 'none'",
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

  private readonly _contextOverride = signal<ContextPanel | null>(null);
  private readonly _spacesCollapsed = signal(
    readStoredBoolean(SPACES_COLLAPSED_KEY, false),
  );

  private readonly isSpaceLeafRoute = computed(() => {
    const path = this.currentPath();
    return (
      path.startsWith("/container/") ||
      path.startsWith("/meeting/") ||
      (path.startsWith("/notes/") && path !== "/notes/new") ||
      (path.startsWith("/tasks/") && path !== "/tasks/new") ||
      path.startsWith("/dashboards/") ||
      // Shared Brains and a received container are now ROWS in the Spaces
      // sidebar rather than a separate destination, so opening one must keep
      // the tree beside it — the same as opening any Space of the user's own.
      path === "/shared-brains" ||
      path.startsWith("/shared/")
    );
  });

  private readonly isBrowseRoute = computed(() =>
    BROWSE_ITEMS.some((item) => this.currentPath() === item.path),
  );

  readonly contextPanel = computed<ContextPanel>(() => {
    const path = this.currentPath();
    if (path.startsWith("/settings") || path.startsWith("/org-item/")) {
      return "none";
    }
    const override = this._contextOverride();
    if (override) {
      return override;
    }
    if (this.isSpaceLeafRoute()) {
      return this._spacesCollapsed() ? "none" : "spaces";
    }
    if (this.isBrowseRoute()) {
      return "browse";
    }
    return "none";
  });

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

  async showSpaces(): Promise<void> {
    if (this.workspace.forestEmpty()) {
      await this.workspace.reload();
    }
    const firstSpace = this.workspace.forest()[0];
    const path = this.currentPath();
    const fixedDrilldown =
      path.startsWith("/settings") || path.startsWith("/org-item/");
    const narrowViewport = window.matchMedia(NARROW_SHELL_QUERY).matches;
    this.setSpacesCollapsed(false);
    if ((fixedDrilldown || narrowViewport) && firstSpace) {
      this._contextOverride.set(null);
      await this.router.navigate(["/container", firstSpace.id]);
      return;
    }
    this._contextOverride.set("spaces");
    if (fixedDrilldown) {
      await this.router.navigate(["/record"]);
    }
  }

  showBrowse(): void {
    this._contextOverride.set(null);
    void this.router.navigate(["/library"]);
  }

  clearContextOverride(): void {
    this._contextOverride.set(null);
  }

  isCaptureActive(): boolean {
    return this.currentPath() === "/record" && this.contextPanel() === "none";
  }

  isAskActive(): boolean {
    return (
      this.currentPath().startsWith("/ask") && this.contextPanel() === "none"
    );
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
    this._contextOverride.set(null);
    void this.router.navigate(["/notes/new"]);
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
        this.toast.success(`Created Space “${request.name}”`);
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

  collapseSpaces(): void {
    this.setSpacesCollapsed(true);
    this._contextOverride.set("none");
  }

  private setSpacesCollapsed(collapsed: boolean): void {
    this._spacesCollapsed.set(collapsed);
    writeStoredBoolean(SPACES_COLLAPSED_KEY, collapsed);
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
