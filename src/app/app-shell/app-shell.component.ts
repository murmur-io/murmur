import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { NavigationEnd, Router, RouterLink, RouterOutlet } from "@angular/router";
import { filter, map } from "rxjs";

import { TabsService } from "../core/tabs.service";
import { IpcService } from "../core/ipc.service";
import type {
  WorkspaceOrganizeMove,
  WorkspaceOrganizePlan,
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
import { WorkspaceService } from "../features/workspace/workspace.service";
import { WorkspaceOrganizeSheetComponent } from "../features/workspace/workspace-organize-sheet/workspace-organize-sheet.component";
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
  { path: "/dashboards", label: "Dashboards", icon: "dashboards", group: "Work" },
  { path: "/reminders", label: "Reminders", icon: "reminders", group: "Work" },
  { path: "/brain", label: "Brain", icon: "brain", group: "Intelligence" },
  { path: "/analytics", label: "Analytics", icon: "analytics", group: "Insights" },
  { path: "/graph", label: "Graph", icon: "graph", group: "Insights" },
  { path: "/people", label: "People", icon: "people", group: "Insights" },
];

const BROWSE_GROUPS = ["Work", "Intelligence", "Insights"] as const;
const NARROW_SHELL_QUERY = "(max-width: 760px)";

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
    WorkspaceOrganizeSheetComponent,
    LockSharesDialogComponent,
    DocumentPreviewComponent,
    ReminderComposerComponent,
    TilePaletteComponent,
    AccountSessionBannerComponent,
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

  private readonly isSpaceLeafRoute = computed(() => {
    const path = this.currentPath();
    return (
      path.startsWith("/container/") ||
      path.startsWith("/meeting/") ||
      (path.startsWith("/notes/") && path !== "/notes/new") ||
      (path.startsWith("/tasks/") && path !== "/tasks/new") ||
      path.startsWith("/dashboards/")
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
      return "spaces";
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
  readonly workspaceOrganizePlan = signal<WorkspaceOrganizePlan | null>(null);
  readonly workspaceOrganizeDisabled = computed(
    () =>
      this.workspace.unfiledRecordings().total === 0 ||
      this.workspaceOrganizePlanning() ||
      this.workspaceOrganizeApplying(),
  );
  readonly canCreateWorkspaceFolder = computed(() => {
    const firstSpace = this.workspace.forest()[0];
    return Boolean(firstSpace && (!firstSpace.locked || firstSpace.unlocked));
  });
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
    return this.currentPath().startsWith("/ask") && this.contextPanel() === "none";
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

  newWorkspaceFolder(): void {
    void this.createFolderAtTop();
  }

  private async createFolderAtTop(): Promise<void> {
    const project = this.workspace.forest()[0];
    if (!project || (project.locked && !project.unlocked)) {
      return;
    }
    await this.workspace.createFolder(project.id, "New folder");
  }

  async planWorkspaceOrganization(): Promise<void> {
    if (this.workspaceOrganizeDisabled() || this.workspaceOrganizePlan()) {
      return;
    }
    this.workspaceOrganizePlanning.set(true);
    try {
      const plan = await this.ipc.planWorkspaceOrganization();
      if (plan.moves.length === 0 && plan.skipped.length === 0) {
        this.toast.info("Brain found no unfiled recordings to organize.");
        return;
      }
      this.workspaceOrganizePlan.set(plan);
    } catch {
      this.toast.danger(
        "Brain couldn’t plan the organization. Please try again.",
      );
    } finally {
      this.workspaceOrganizePlanning.set(false);
    }
  }

  async applyWorkspaceOrganization(
    moves: WorkspaceOrganizeMove[],
  ): Promise<void> {
    if (moves.length === 0 || this.workspaceOrganizeApplying()) {
      return;
    }
    this.workspaceOrganizeApplying.set(true);
    try {
      const result = await this.ipc.applyWorkspaceOrganization(moves);
      await this.workspace.reload();
      this.workspaceOrganizePlan.set(null);
      const applied = result.appliedIds.length;
      const failed = result.failures.length;
      if (failed > 0) {
        const appliedCopy =
          applied === 0
            ? "No recordings organized"
            : `${applied} ${applied === 1 ? "recording" : "recordings"} organized`;
        this.toast.danger(`${appliedCopy}; ${failed} failed`);
      } else {
        this.toast.success(
          `${applied} ${applied === 1 ? "recording" : "recordings"} organized`,
        );
      }
    } catch {
      await this.workspace.reload();
      this.toast.danger(
        "Couldn’t finish applying the Brain plan. Refresh to see what moved.",
      );
    } finally {
      this.workspaceOrganizeApplying.set(false);
    }
  }

  closeWorkspaceOrganization(): void {
    if (!this.workspaceOrganizeApplying()) {
      this.workspaceOrganizePlan.set(null);
    }
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
