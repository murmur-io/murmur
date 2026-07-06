import {
  ChangeDetectionStrategy,
  Component,
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
  RouterLinkActive,
  RouterOutlet,
} from "@angular/router";
import { filter, map } from "rxjs";
import { isDrilldownRoute } from "../core/nav-history.service";
import {
  MurIconComponent,
  type ShellIcon,
} from "../design-system/icon/icon.component";
import { MurKbdComponent } from "../design-system/kbd/kbd.component";
import { MurQuickSearchComponent } from "../design-system/quick-search/quick-search.component";
import { MurSidebarComponent } from "../design-system/sidebar/sidebar.component";
import { FoldersService } from "../services/folders.service";
import { ToastService, type Toast } from "../services/toast.service";

/** localStorage key for the chrome mode: "1" = pill bar, "0" = sidebar. */
const SIDEBAR_KEY = "murmur-sidebar-collapsed";

/** A primary navigation destination. `icon` selects the inline SVG. */
interface NavItem {
  readonly path: string;
  readonly label: string;
  readonly icon: ShellIcon;
}

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
  ],
  host: {
    // Scoped to !inDrilldown so the pill-clearance padding never leaks onto
    // drill-down routes (which render no pill bar).
    "[class.pill-mode]": "pillMode() && !inDrilldown()",
    "(document:keydown)": "onGlobalKeydown($event)",
  },
  templateUrl: "./app-shell.component.html",
  styleUrl: "./app-shell.component.scss",
})
export class AppShellComponent {
  private readonly folders = inject(FoldersService);
  private readonly toast = inject(ToastService);
  private readonly router = inject(Router);

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

  /** Primary destinations (Settings lives in the chrome footer / pill end). */
  readonly navItems: readonly NavItem[] = [
    { path: "/record", label: "Record", icon: "record" },
    { path: "/library", label: "Meetings", icon: "meetings" },
    { path: "/analytics", label: "Analytics", icon: "analytics" },
    { path: "/graph", label: "Graph", icon: "graph" },
    { path: "/people", label: "People", icon: "people" },
    { path: "/brain", label: "Brain", icon: "brain" },
    { path: "/ask", label: "Ask", icon: "ask" },
  ];

  /**
   * Chrome mode: false = floating sidebar, true = top pill bar. Persisted
   * under the pre-existing key (old "collapsed" preference maps naturally
   * onto the compact pill chrome).
   */
  private readonly _pillMode = signal(this.readStoredPillMode());
  readonly pillMode = this._pillMode.asReadonly();

  /** Whether the ⌘K quick-search spotlight is open. */
  readonly searchOpen = signal(false);

  /** How many locked folders are unlocked (plaintext-exposed) this session. */
  readonly unlockedCount = this.folders.unlockedCount;

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

  /** Toggle between the floating sidebar and the top pill bar. */
  togglePillMode(): void {
    this._pillMode.update((c) => !c);
  }

  /** Open the ⌘K spotlight. */
  openSearch(): void {
    this.searchOpen.set(true);
  }

  /** Quick action: jump to Record to start a new note (⌘N). */
  newNote(): void {
    this.searchOpen.set(false);
    void this.router.navigate(["/record"]);
  }

  /**
   * Global shortcuts: ⌘K/Ctrl+K toggles search, ⌘N/Ctrl+N new note. Escape
   * closes the spotlight at the DOCUMENT level — the scrim's own handler only
   * fires while focus sits inside it (e.g. it dies once focus falls to <body>).
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
    } else if (key === "n") {
      e.preventDefault();
      this.newNote();
    }
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
}
