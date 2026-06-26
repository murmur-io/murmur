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
          <nav class="app-nav">
            <a routerLink="/record" routerLinkActive="active">Record</a>
            <a routerLink="/library" routerLinkActive="active">Meetings</a>
            <a routerLink="/analytics" routerLinkActive="active">Analytics</a>
            <a routerLink="/ask" routerLinkActive="active">Ask</a>
            <a routerLink="/settings" routerLinkActive="active">Settings</a>
          </nav>
        </div>
      </header>
    }
    <main class="app-main" [class.bare]="isBar()">
      <router-outlet></router-outlet>
    </main>
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
    `,
  ],
})
export class AppComponent implements OnInit {
  private readonly router = inject(Router);
  private readonly ipc = inject(IpcService);

  /** True in the floating-bar window (route /bar) — the app chrome is hidden there. */
  readonly isBar = toSignal(
    this.router.events.pipe(
      filter((e): e is NavigationEnd => e instanceof NavigationEnd),
      map(() => this.router.url.startsWith("/bar")),
    ),
    { initialValue: location.pathname.startsWith("/bar") },
  );

  /** Make the bar window's document transparent (no aurora/grain behind the pill). */
  private readonly _bodyClass = effect(() => {
    document.body.classList.toggle("bar-shell", this.isBar());
  });

  /**
   * First-run gate (MAIN window only). On startup, if the user hasn't completed
   * onboarding, send them to the wizard. The floating-bar window is never gated —
   * it just mirrors recording state and must stay chromeless.
   */
  async ngOnInit(): Promise<void> {
    if (this.isBar()) return;
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
