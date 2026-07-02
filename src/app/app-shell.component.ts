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
import { FoldersService } from "./services/folders.service";
import { ToastService, type Toast } from "./services/toast.service";

/** localStorage key for the sidebar collapsed/expanded preference. */
const SIDEBAR_KEY = "murmur-sidebar-collapsed";

/** A primary navigation destination in the sidebar. `icon` selects the inline SVG. */
interface NavItem {
  readonly path: string;
  readonly label: string;
  readonly icon: "record" | "meetings" | "analytics" | "graph" | "brain" | "ask";
}

/**
 * The app shell chrome — a macOS-native LEFT SIDEBAR (Apple Notes / Notion
 * flavour): brand + primary nav + Settings icon, no top header bar. The window
 * uses the Overlay title-bar style (tauri.conf.json), so the traffic lights
 * float over the sidebar's top drag strip.
 *
 * Why this is a separate child component (and not just AppComponent's template):
 * it is the Tauri WKWebView FOUC fix. AppComponent's host is the STATIC
 * `<app-root>` element present in index.html at parse time; in this WKWebView,
 * styles for elements rendered as direct descendants of that pre-existing host
 * were never applied on cold launch (chrome rendered as raw unstyled HTML) —
 * even with the matching encapsulation id and the rule present in a <style>.
 * Every component with a DYNAMICALLY-created host (route pages, cards, banners)
 * renders correctly styled. So the shell lives here, under a host Angular
 * creates at runtime, and AppComponent renders `<app-shell>` only after
 * bootstrap. For the same reason the shell's CSS is GLOBAL (styles.css, matched
 * by class) rather than component-encapsulated — see the block there.
 */
@Component({
  selector: "app-shell",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterOutlet, RouterLink, RouterLinkActive],
  template: `
    <!-- Primary rail is HIDDEN under /settings: settings drills down to its own
         two-column [section rail | content] layout (see settings.component). The
         @if removes the aside from the DOM so the outlet's flex row fills. -->
    @if (!inSettings()) {
    <aside class="app-sidebar" [class.collapsed]="collapsed()">
      <!-- Top drag strip: reserves room for the overlay traffic lights and lets
           the user move the window (data-tauri-drag-region). Empty on purpose so
           it never swallows a click meant for an interactive control. -->
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

      <nav class="sidebar-nav" aria-label="Primary">
        @for (item of navItems; track item.path) {
          <a
            [routerLink]="item.path"
            routerLinkActive="active"
            [attr.title]="collapsed() ? item.label : null"
            [attr.aria-label]="item.label"
          >
            <span class="nav-icon" aria-hidden="true">
              @switch (item.icon) {
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
              }
            </span>
            <span class="nav-label">{{ item.label }}</span>
          </a>
        }
      </nav>

      <div class="sidebar-footer">
        <!-- Subtle session-privacy indicator: how many locked folders are
             currently unlocked (plaintext-exposed) for this session. Reads
             straight off the folders signal store; absent at zero. -->
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
          [attr.title]="collapsed() ? 'Settings' : null"
        >
          <span class="nav-icon" aria-hidden="true">
            <svg viewBox="0 0 20 20" fill="none">
              <circle cx="10" cy="10" r="2.6" stroke="currentColor" stroke-width="1.4" />
              <path d="M10 2.5v1.9M10 15.6v1.9M17.5 10h-1.9M4.4 10H2.5M15.3 4.7l-1.35 1.35M6.05 13.95 4.7 15.3M15.3 15.3l-1.35-1.35M6.05 6.05 4.7 4.7" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
            </svg>
          </span>
          <span class="nav-label">Settings</span>
        </a>

        <button
          type="button"
          class="sidebar-collapse"
          (click)="toggleCollapsed()"
          [attr.aria-label]="collapsed() ? 'Expand sidebar' : 'Collapse sidebar'"
          [attr.aria-pressed]="collapsed()"
          [attr.title]="collapsed() ? 'Expand sidebar' : 'Collapse sidebar'"
        >
          <span class="nav-icon" aria-hidden="true">
            <svg viewBox="0 0 20 20" fill="none">
              <rect x="3" y="4.25" width="14" height="11.5" rx="2.2" stroke="currentColor" stroke-width="1.4" />
              <path d="M8 4.5v11" stroke="currentColor" stroke-width="1.4" />
              <path
                class="collapse-chevron"
                d="M11.4 7.6 13.8 10l-2.4 2.4"
                stroke="currentColor"
                stroke-width="1.4"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
          </span>
          <span class="nav-label">Collapse</span>
        </button>
      </div>
    </aside>
    }

    <main class="app-main">
      <router-outlet></router-outlet>
    </main>

    <!-- Toast viewport: renders the app-wide queue from ToastService —
         move-to-folder outcomes, screen-share re-lock notices. -->
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
      /* Compact inline action inside a toast (sits before the ✕ close). Uses the
         global .btn/.btn-primary primitives; only the size is trimmed here so it
         fits the toast strip. */
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
   * `router.url` so `inSettings` is correct on a cold deep-link to `/settings`
   * (before the first `NavigationEnd`). The subscription is framework-managed by
   * `toSignal` — no hand-rolled `.subscribe()`.
   */
  private readonly currentUrl = toSignal(
    this.router.events.pipe(
      filter((e): e is NavigationEnd => e instanceof NavigationEnd),
      map(() => this.router.url),
    ),
    { initialValue: this.router.url },
  );

  /**
   * True while on any `/settings*` route. When true the primary rail is removed
   * from the DOM so the settings drill-down owns the full width with its own
   * two-column layout.
   */
  readonly inSettings = computed(() => this.currentUrl().startsWith("/settings"));

  /** Primary sidebar destinations (Settings lives in the footer as its own icon). */
  readonly navItems: readonly NavItem[] = [
    { path: "/record", label: "Record", icon: "record" },
    { path: "/library", label: "Meetings", icon: "meetings" },
    { path: "/analytics", label: "Analytics", icon: "analytics" },
    { path: "/graph", label: "Graph", icon: "graph" },
    { path: "/brain", label: "Brain", icon: "brain" },
    { path: "/ask", label: "Ask", icon: "ask" },
  ];

  /** Sidebar collapsed (icons-only) state, seeded from and persisted to localStorage. */
  private readonly _collapsed = signal(this.readStoredCollapsed());
  readonly collapsed = this._collapsed.asReadonly();

  /** How many locked folders are unlocked (plaintext-exposed) this session. */
  readonly unlockedCount = this.folders.unlockedCount;

  /** The app-wide toast queue, rendered in the main-window viewport. */
  readonly toasts = this.toast.toasts;

  /** Persist the collapsed choice whenever it changes (no signal write here). */
  private readonly _persistCollapsed = effect(() => {
    const value = this._collapsed();
    try {
      localStorage.setItem(SIDEBAR_KEY, value ? "1" : "0");
    } catch {
      // Private-mode / storage-disabled — the preference is simply not persisted.
    }
  });

  /** Toggle the sidebar between full (icon + label) and collapsed (icons only). */
  toggleCollapsed(): void {
    this._collapsed.update((c) => !c);
  }

  /** Dismiss a toast by id (also cancels its auto-dismiss timer in the service). */
  dismissToast(id: number): void {
    this.toast.dismiss(id);
  }

  /** Run a toast's inline action, then dismiss the toast. */
  runToastAction(t: Toast): void {
    t.action?.run();
    this.dismissToast(t.id);
  }

  /** Read the persisted collapsed preference; default expanded. */
  private readStoredCollapsed(): boolean {
    try {
      return localStorage.getItem(SIDEBAR_KEY) === "1";
    } catch {
      return false;
    }
  }
}
