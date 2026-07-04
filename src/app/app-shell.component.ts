import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  input,
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
import { isDrilldownRoute } from "./core/nav-history.service";
import { QuickSearchComponent } from "./quick-search.component";
import { FoldersService } from "./services/folders.service";
import { ToastService, type Toast } from "./services/toast.service";

/** localStorage key for the chrome mode: "1" = pill bar, "0" = sidebar. */
const SIDEBAR_KEY = "murmur-sidebar-collapsed";

/** Every glyph the shell chrome can render (nav + quick actions + chrome). */
type ShellIcon =
  | "record"
  | "meetings"
  | "analytics"
  | "graph"
  | "people"
  | "brain"
  | "ask"
  | "settings"
  | "search"
  | "plus"
  | "sidebar";

/** A primary navigation destination. `icon` selects the inline SVG. */
interface NavItem {
  readonly path: string;
  readonly label: string;
  readonly icon: ShellIcon;
}

/**
 * PROTOTYPE (Apple TV shell) — one inline-SVG glyph, shared by the floating
 * sidebar and the pill bar so the big icon @switch lives in exactly one place.
 */
@Component({
  selector: "app-nav-icon",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <span class="nav-icon" aria-hidden="true">
      @switch (icon()) {
        @case ("record") {
          <svg viewBox="0 0 20 20" fill="none">
            <circle cx="10" cy="10" r="4.5" fill="currentColor" />
            <circle cx="10" cy="10" r="7.25" stroke="currentColor" stroke-width="1.4" opacity="0.5" />
          </svg>
        }
        @case ("meetings") {
          <svg viewBox="0 0 20 20" fill="none">
            <rect x="3.25" y="3.75" width="13.5" height="12.5" rx="2.2" stroke="currentColor" stroke-width="1.4" />
            <path d="M6.5 7.5h7M6.5 10.5h7M6.5 13.25h4" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
          </svg>
        }
        @case ("analytics") {
          <svg viewBox="0 0 20 20" fill="none">
            <path d="M4 16V9M8 16V5m4 11v-7m4 7V7" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
          </svg>
        }
        @case ("graph") {
          <svg viewBox="0 0 20 20" fill="none">
            <circle cx="5" cy="6" r="2.1" stroke="currentColor" stroke-width="1.4" />
            <circle cx="15" cy="7.5" r="2.1" stroke="currentColor" stroke-width="1.4" />
            <circle cx="9" cy="15" r="2.1" stroke="currentColor" stroke-width="1.4" />
            <path d="M6.7 7.3l5.2 6.2M6.9 6.6l6-.8" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
          </svg>
        }
        @case ("people") {
          <svg viewBox="0 0 20 20" fill="none">
            <circle cx="7.3" cy="7.5" r="2.6" stroke="currentColor" stroke-width="1.4" />
            <path d="M2.8 15.5c.4-2.4 2.3-3.9 4.5-3.9s4.1 1.5 4.5 3.9" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
            <path d="M13 5.2a2.3 2.3 0 0 1 0 4.4M14.4 11.9c1.6.4 2.7 1.7 3 3.4" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
          </svg>
        }
        @case ("brain") {
          <svg viewBox="0 0 20 20" fill="none">
            <path d="M10 4.2c-1.9-1.6-5-.6-5 1.9 0 .5-1.2.9-1.2 2.6 0 1.1.8 1.6.8 2.3 0 1.9 2.1 3 4 2.3M10 4.2c1.9-1.6 5-.6 5 1.9 0 .5 1.2.9 1.2 2.6 0 1.1-.8 1.6-.8 2.3 0 1.9-2.1 3-4 2.3M10 4.2v11.4" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" />
          </svg>
        }
        @case ("ask") {
          <svg viewBox="0 0 20 20" fill="none">
            <path d="M16.5 10.5A6 6 0 1 1 9.5 4.6" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
            <path d="M7.9 8.1a2.2 2.2 0 1 1 3.1 2.4c-.7.4-1 .8-1 1.5" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
            <circle cx="10" cy="14.4" r="0.95" fill="currentColor" />
          </svg>
        }
        @case ("settings") {
          <svg viewBox="0 0 20 20" fill="none">
            <circle cx="10" cy="10" r="2.6" stroke="currentColor" stroke-width="1.4" />
            <path d="M10 2.5v1.9M10 15.6v1.9M17.5 10h-1.9M4.4 10H2.5M15.3 4.7l-1.35 1.35M6.05 13.95 4.7 15.3M15.3 15.3l-1.35-1.35M6.05 6.05 4.7 4.7" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
          </svg>
        }
        @case ("search") {
          <svg viewBox="0 0 20 20" fill="none">
            <circle cx="9" cy="9" r="5.2" stroke="currentColor" stroke-width="1.5" />
            <path d="m13.2 13.2 3.3 3.3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
          </svg>
        }
        @case ("plus") {
          <svg viewBox="0 0 20 20" fill="none">
            <path d="M10 4.5v11M4.5 10h11" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
          </svg>
        }
        @case ("sidebar") {
          <svg viewBox="0 0 20 20" fill="none">
            <rect x="3" y="4.25" width="14" height="11.5" rx="2.2" stroke="currentColor" stroke-width="1.4" />
            <path d="M8 4.5v11" stroke="currentColor" stroke-width="1.4" />
          </svg>
        }
      }
    </span>
  `,
})
export class NavIconComponent {
  readonly icon = input.required<ShellIcon>();
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
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    RouterOutlet,
    RouterLink,
    RouterLinkActive,
    NavIconComponent,
    QuickSearchComponent,
  ],
  host: {
    // Scoped to !inDrilldown so the pill-clearance padding never leaks onto
    // drill-down routes (which render no pill bar).
    "[class.pill-mode]": "pillMode() && !inDrilldown()",
    "(document:keydown)": "onGlobalKeydown($event)",
  },
  template: `
    <!-- Chrome is HIDDEN under any drill-down route (/settings, /library):
         each drills down to its own two-column [rail | content] layout. -->
    @if (!inDrilldown()) {
      @if (!pillMode()) {
        <aside class="app-sidebar">
          <!-- Top drag strip: reserves room for the overlay traffic lights and
               lets the user move the window. -->
          <div class="sidebar-drag" data-tauri-drag-region></div>

          <a class="brand" routerLink="/record" aria-label="Murmur — home">
            <span class="brand-mark" aria-hidden="true">
              <svg class="brand-wave" viewBox="0 0 28 28" fill="none">
                <defs>
                  <linearGradient id="murmurWave" x1="0" y1="0" x2="1" y2="1">
                    <stop offset="0" stop-color="#6e76ff" />
                    <stop offset="1" stop-color="#9d7bff" />
                  </linearGradient>
                </defs>
                <rect class="bar b1" x="4.4" y="10" width="2.4" height="8" rx="1.2" />
                <rect class="bar b2" x="8.4" y="7" width="2.4" height="14" rx="1.2" />
                <rect class="bar b3" x="12.4" y="4" width="2.4" height="20" rx="1.2" />
                <rect class="bar b4" x="16.4" y="7" width="2.4" height="14" rx="1.2" />
                <rect class="bar b5" x="20.4" y="10" width="2.4" height="8" rx="1.2" />
              </svg>
            </span>
            <span class="brand-word">murmur</span>
          </a>

          <!-- Quick actions (Apple TV puts Search first in the rail). -->
          <div class="sidebar-quick">
            <button type="button" class="quick-row" (click)="openSearch()">
              <app-nav-icon icon="search" />
              <span class="nav-label">Search</span>
              <kbd class="quick-kbd">⌘K</kbd>
            </button>
            <button type="button" class="quick-row" (click)="newNote()">
              <app-nav-icon icon="plus" />
              <span class="nav-label">New note</span>
              <kbd class="quick-kbd">⌘N</kbd>
            </button>
          </div>

          <nav class="sidebar-nav" aria-label="Primary">
            @for (item of navItems; track item.path) {
              <a
                [routerLink]="item.path"
                routerLinkActive="active"
                [attr.aria-label]="item.label"
              >
                <app-nav-icon [icon]="item.icon" />
                <span class="nav-label">{{ item.label }}</span>
              </a>
            }
          </nav>

          <div class="sidebar-footer">
            <!-- Session-privacy indicator: locked folders currently unlocked. -->
            @if (unlockedCount() > 0) {
              <span
                class="unlocked-pill"
                role="status"
                [attr.aria-label]="
                  unlockedCount() +
                  ' locked folder' +
                  (unlockedCount() === 1 ? '' : 's') +
                  ' unlocked this session'
                "
                [attr.title]="
                  'These folders are decrypted into plaintext until you re-lock or a screen share starts.'
                "
              >
                <svg
                  class="unlocked-glyph"
                  viewBox="0 0 16 16"
                  width="12"
                  height="12"
                  fill="none"
                  aria-hidden="true"
                >
                  <rect x="3.5" y="7" width="9" height="6" rx="1.4" stroke="currentColor" stroke-width="1.3" />
                  <path d="M5.5 7V5.4a2.5 2.5 0 0 1 4.9-0.65" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
                </svg>
                <span class="unlocked-count">{{ unlockedCount() }}</span>
                <span class="nav-label unlocked-label">unlocked</span>
              </span>
            }

            <a
              class="sidebar-settings"
              routerLink="/settings"
              routerLinkActive="active"
              aria-label="Settings"
            >
              <app-nav-icon icon="settings" />
              <span class="nav-label">Settings</span>
            </a>

            <button
              type="button"
              class="sidebar-collapse"
              (click)="togglePillMode()"
              aria-label="Collapse the sidebar into the top bar"
              title="Collapse into the top bar"
            >
              <app-nav-icon icon="sidebar" />
              <span class="nav-label">Collapse to bar</span>
            </button>
          </div>
        </aside>
      } @else {
        <!-- Pill mode: window-drag strip along the top edge, BEHIND the pill. -->
        <div class="pill-drag" data-tauri-drag-region></div>

        <nav class="pill-bar" aria-label="Primary">
          <button
            type="button"
            class="pill-item"
            (click)="togglePillMode()"
            aria-label="Show sidebar"
            title="Show sidebar"
          >
            <app-nav-icon icon="sidebar" />
          </button>

          <span class="pill-sep" aria-hidden="true"></span>

          @for (item of navItems; track item.path) {
            <a
              class="pill-item"
              [routerLink]="item.path"
              routerLinkActive="active"
              [attr.aria-label]="item.label"
              [attr.title]="item.label"
            >
              <app-nav-icon [icon]="item.icon" />
              <span class="nav-label">{{ item.label }}</span>
            </a>
          }

          <span class="pill-sep" aria-hidden="true"></span>

          <button
            type="button"
            class="pill-item"
            (click)="openSearch()"
            aria-label="Search (⌘K)"
            title="Search (⌘K)"
          >
            <app-nav-icon icon="search" />
          </button>
          <button
            type="button"
            class="pill-item"
            (click)="newNote()"
            aria-label="New note (⌘N)"
            title="New note (⌘N)"
          >
            <app-nav-icon icon="plus" />
          </button>
          <a
            class="pill-item"
            routerLink="/settings"
            routerLinkActive="active"
            aria-label="Settings"
            title="Settings"
          >
            <app-nav-icon icon="settings" />
          </a>

          @if (unlockedCount() > 0) {
            <span
              class="pill-unlocked"
              role="status"
              [attr.aria-label]="unlockedCount() + ' unlocked this session'"
              title="Locked folders decrypted for this session"
            >
              <svg viewBox="0 0 16 16" width="12" height="12" fill="none" aria-hidden="true">
                <rect x="3.5" y="7" width="9" height="6" rx="1.4" stroke="currentColor" stroke-width="1.3" />
                <path d="M5.5 7V5.4a2.5 2.5 0 0 1 4.9-0.65" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
              </svg>
              <span class="unlocked-count">{{ unlockedCount() }}</span>
            </span>
          }
        </nav>
      }
    }

    <main class="app-main">
      <router-outlet></router-outlet>
    </main>

    <!-- ⌘K quick-search spotlight (works from every route, drill-downs too). -->
    @if (searchOpen()) {
      <app-quick-search (closed)="searchOpen.set(false)" />
    }

    <!-- Toast viewport: renders the app-wide queue from ToastService. -->
    @if (toasts().length > 0) {
      <div class="toast-viewport" aria-live="polite" aria-atomic="false">
        @for (t of toasts(); track t.id) {
          <div class="toast" [class]="'is-' + t.kind" role="status">
            <span class="toast-msg">{{ t.message }}</span>
            @if (t.action; as action) {
              <button
                type="button"
                class="btn btn-primary toast-action"
                (click)="runToastAction(t)"
              >
                {{ action.label }}
              </button>
            }
            <button
              type="button"
              class="toast-close"
              aria-label="Dismiss notification"
              (click)="dismissToast(t.id)"
            >
              <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
                <path
                  d="M4 4l8 8M12 4l-8 8"
                  stroke="currentColor"
                  stroke-width="1.6"
                  stroke-linecap="round"
                />
              </svg>
            </button>
          </div>
        }
      </div>
    }
  `,
  styles: [
    `
      /* Compact inline action inside a toast (sits before the ✕ close). */
      .toast-action {
        flex: none;
        height: auto;
        padding: var(--space-1) var(--space-3);
        font-size: 0.82rem;
        white-space: nowrap;
      }
    `,
  ],
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
