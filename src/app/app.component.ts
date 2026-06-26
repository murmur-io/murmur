import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  effect,
  inject,
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
import { IpcService } from "./core/ipc.service";
import { FoldersService } from "./services/folders.service";
import { ScreenShareService } from "./services/screen-share.service";
import { ToastService } from "./services/toast.service";

@Component({
  selector: "app-root",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterOutlet, RouterLink, RouterLinkActive],
  template: `
    @if (!isBar()) {
      <header class="app-header">
        <div class="app-bar">
          <a class="brand" routerLink="/record" aria-label="Murmur — home">
            <span class="brand-mark" aria-hidden="true">
              <svg class="brand-wave" viewBox="0 0 28 28" fill="none">
                <defs>
                  <linearGradient id="murmurWave" x1="0" y1="0" x2="1" y2="1">
                    <stop offset="0" stop-color="#6e76ff" />
                    <stop offset="1" stop-color="#9d7bff" />
                  </linearGradient>
                </defs>
                <rect
                  class="bar b1"
                  x="4.4"
                  y="10"
                  width="2.4"
                  height="8"
                  rx="1.2"
                />
                <rect
                  class="bar b2"
                  x="8.4"
                  y="7"
                  width="2.4"
                  height="14"
                  rx="1.2"
                />
                <rect
                  class="bar b3"
                  x="12.4"
                  y="4"
                  width="2.4"
                  height="20"
                  rx="1.2"
                />
                <rect
                  class="bar b4"
                  x="16.4"
                  y="7"
                  width="2.4"
                  height="14"
                  rx="1.2"
                />
                <rect
                  class="bar b5"
                  x="20.4"
                  y="10"
                  width="2.4"
                  height="8"
                  rx="1.2"
                />
              </svg>
            </span>
            <span class="brand-word">murmur</span>
          </a>

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
                <rect
                  x="3.5"
                  y="7"
                  width="9"
                  height="6"
                  rx="1.4"
                  stroke="currentColor"
                  stroke-width="1.3"
                />
                <path
                  d="M5.5 7V5.4a2.5 2.5 0 0 1 4.9-0.65"
                  stroke="currentColor"
                  stroke-width="1.3"
                  stroke-linecap="round"
                />
              </svg>
              <span class="unlocked-count">{{ unlockedCount() }}</span>
              <span class="unlocked-label">unlocked</span>
            </span>
          }

          <nav class="app-nav">
            <a routerLink="/record" routerLinkActive="active">Record</a>
            <a routerLink="/library" routerLinkActive="active">Meetings</a>
            <a routerLink="/analytics" routerLinkActive="active">Analytics</a>
            <a routerLink="/graph" routerLinkActive="active">Graph</a>
            <a routerLink="/ask" routerLinkActive="active">Ask</a>
            <a routerLink="/settings" routerLinkActive="active">Settings</a>
          </nav>
        </div>
      </header>
    }
    <main class="app-main" [class.bare]="isBar()">
      <router-outlet></router-outlet>
    </main>

    <!-- Toast viewport (MAIN window only): renders the app-wide queue from
         ToastService — move-to-folder outcomes, screen-share re-lock notices. -->
    @if (!isBar() && toasts().length > 0) {
      <div class="toast-viewport" aria-live="polite" aria-atomic="false">
        @for (t of toasts(); track t.id) {
          <div class="toast" [class]="'is-' + t.kind" role="status">
            <span class="toast-msg">{{ t.message }}</span>
            <button
              type="button"
              class="toast-close"
              aria-label="Dismiss notification"
              (click)="dismissToast(t.id)"
            >
              <svg
                viewBox="0 0 16 16"
                width="13"
                height="13"
                aria-hidden="true"
              >
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
      :host {
        display: block;
        min-height: 100vh;
      }

      .app-header {
        position: sticky;
        top: 0;
        z-index: 10;
        background: rgba(7, 7, 11, 0.55);
        backdrop-filter: saturate(150%) blur(22px);
        -webkit-backdrop-filter: saturate(150%) blur(22px);
        border-bottom: 1px solid var(--border-subtle);
        box-shadow: 0 1px 0 rgba(255, 255, 255, 0.04);
      }

      .app-bar {
        display: flex;
        align-items: center;
        gap: var(--space-6);
        max-width: var(--content-max);
        margin: 0 auto;
        padding: var(--space-3) var(--space-5);
      }

      .brand {
        display: inline-flex;
        align-items: center;
        gap: 10px;
        text-decoration: none;
        border-radius: var(--radius-md);
        transition: transform var(--transition-fast);
      }
      .brand:hover {
        transform: translateY(-1px);
      }
      .brand:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .brand-mark {
        display: grid;
        place-items: center;
        width: 30px;
        height: 30px;
        border-radius: 9px;
        background: var(--accent-soft);
        border: 1px solid var(--glass-border);
        box-shadow:
          var(--glass-highlight),
          0 4px 16px rgba(110, 118, 255, 0.28);
      }
      .brand-wave {
        display: block;
        width: 22px;
        height: 22px;
      }
      .brand-wave .bar {
        fill: url(#murmurWave);
        transform-box: fill-box;
        transform-origin: center;
        animation: brand-wave 1.5s ease-in-out infinite;
      }
      .brand-wave .b2 {
        animation-delay: 0.12s;
      }
      .brand-wave .b3 {
        animation-delay: 0.24s;
      }
      .brand-wave .b4 {
        animation-delay: 0.36s;
      }
      .brand-wave .b5 {
        animation-delay: 0.48s;
      }
      @keyframes brand-wave {
        0%,
        100% {
          transform: scaleY(0.5);
        }
        50% {
          transform: scaleY(1);
        }
      }
      .brand-word {
        font-size: 1.06rem;
        font-weight: 700;
        letter-spacing: -0.02em;
        background: linear-gradient(180deg, #ffffff 0%, #b9bbe6 130%);
        -webkit-background-clip: text;
        background-clip: text;
        -webkit-text-fill-color: transparent;
        color: var(--text-primary);
      }
      @media (prefers-reduced-motion: reduce) {
        .brand-wave .bar {
          animation: none;
        }
        .brand:hover {
          transform: none;
        }
      }

      .app-nav {
        display: flex;
        align-items: center;
        gap: var(--space-1);
        margin-left: auto;
      }

      .app-nav a {
        display: inline-flex;
        align-items: center;
        padding: var(--space-2) var(--space-3);
        border-radius: var(--radius-md);
        color: var(--text-secondary);
        font-size: 0.9rem;
        font-weight: 550;
        letter-spacing: -0.01em;
        transition:
          color var(--transition),
          background var(--transition);
      }
      .app-nav a:hover {
        color: var(--text-primary);
        background: var(--surface-hover);
      }
      .app-nav a:focus-visible {
        outline: none;
        color: var(--text-primary);
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .app-nav a.active {
        color: var(--accent-hover);
        background: var(--accent-soft);
      }

      /* --- Session-privacy pill (N folders unlocked this session) --------- */
      .unlocked-pill {
        display: inline-flex;
        align-items: center;
        gap: var(--space-1);
        height: 26px;
        padding: 0 var(--space-3) 0 var(--space-2);
        border: 1px solid transparent;
        border-radius: var(--radius-pill);
        background: var(--accent-soft);
        color: var(--accent-hover);
        font-size: 0.78rem;
        font-weight: 600;
        letter-spacing: -0.01em;
        line-height: 1;
        white-space: nowrap;
        animation: rise 240ms var(--transition) both;
      }
      .unlocked-glyph {
        display: block;
        flex: none;
      }
      .unlocked-count {
        font-variant-numeric: tabular-nums;
      }
      .unlocked-label {
        color: var(--accent-hover);
        opacity: 0.85;
      }

      .app-main {
        max-width: var(--content-max);
        margin: 0 auto;
        padding: var(--space-6) var(--space-5) var(--space-8);
      }
      /* Floating-bar window: no chrome, fill the transparent window edge-to-edge. */
      .app-main.bare {
        max-width: none;
        margin: 0;
        padding: 0;
        min-height: 100vh;
      }

      /* --- Toast viewport (bottom-right, frosted; stacks oldest → newest) --- */
      .toast-viewport {
        position: fixed;
        right: var(--space-5);
        bottom: var(--space-5);
        z-index: 60;
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        max-width: min(360px, calc(100vw - var(--space-6)));
        pointer-events: none;
      }
      .toast {
        display: flex;
        align-items: flex-start;
        gap: var(--space-3);
        padding: var(--space-3) var(--space-3) var(--space-3) var(--space-4);
        border: 1px solid var(--glass-border);
        border-left-width: 3px;
        border-radius: var(--radius-md);
        background: var(--surface-overlay);
        color: var(--text-primary);
        font-size: 0.875rem;
        line-height: 1.45;
        box-shadow: var(--shadow-md), var(--glass-highlight);
        pointer-events: auto;
        animation: rise 220ms var(--ease-spring) both;
      }
      .toast.is-info {
        border-left-color: var(--accent);
      }
      .toast.is-success {
        border-left-color: var(--success);
      }
      .toast.is-danger {
        border-left-color: var(--danger);
      }
      .toast-msg {
        flex: 1 1 auto;
        min-width: 0;
      }
      .toast-close {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        flex: none;
        width: 24px;
        height: 24px;
        margin-top: -2px;
        padding: 0;
        border: none;
        border-radius: var(--radius-sm);
        background: transparent;
        color: var(--text-muted);
        cursor: pointer;
        transition:
          color var(--transition),
          background var(--transition);
      }
      .toast-close:hover {
        color: var(--text-primary);
        background: var(--surface-hover);
      }
      .toast-close:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
    `,
  ],
})
export class AppComponent implements OnInit {
  private readonly router = inject(Router);
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);
  private readonly screenShare = inject(ScreenShareService);
  private readonly toast = inject(ToastService);

  /** True in the floating-bar window (route /bar) — the app chrome is hidden there. */
  readonly isBar = toSignal(
    this.router.events.pipe(
      filter((e): e is NavigationEnd => e instanceof NavigationEnd),
      map(() => this.router.url.startsWith("/bar")),
    ),
    { initialValue: location.pathname.startsWith("/bar") },
  );

  /** How many locked folders are unlocked (plaintext-exposed) this session. */
  readonly unlockedCount = this.folders.unlockedCount;

  /** The app-wide toast queue, rendered in the main-window viewport. */
  readonly toasts = this.toast.toasts;

  /** Make the bar window's document transparent (no aurora/grain behind the pill). */
  private readonly _bodyClass = effect(() => {
    document.body.classList.toggle("bar-shell", this.isBar());
  });

  /** Dismiss a toast by id (also cancels its auto-dismiss timer in the service). */
  dismissToast(id: number): void {
    this.toast.dismiss(id);
  }

  /**
   * First-run gate (MAIN window only). On startup, if the user hasn't completed
   * onboarding, send them to the wizard. The floating-bar window is never gated —
   * it just mirrors recording state and must stay chromeless.
   *
   * The main window also arms the screen-share privacy guard and primes the
   * folder tree (so the "N unlocked" pill + locked-meeting masking have state).
   * The bar window does neither — it stays chromeless and side-effect-free.
   */
  async ngOnInit(): Promise<void> {
    if (this.isBar()) return;

    // Arm the screen-share guard + prime the folder store (main window only).
    // Best-effort and non-blocking: a folders/listen failure must not trap the
    // user on a blank app, so each is fire-and-forget with its own catch.
    void this.screenShare.init();
    void this.folders.load();

    try {
      const cfg = await this.ipc.getConfig();
      if (!cfg.onboarded) {
        await this.router.navigateByUrl("/onboarding");
      }
    } catch {
      // Config unavailable — don't trap the user; the app loads normally.
    }
  }
}
