import {
  ChangeDetectionStrategy,
  Component,
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

@Component({
  selector: "app-root",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterOutlet, RouterLink, RouterLinkActive],
  template: `
    @if (!isBar()) {
      <header class="app-header">
        <div class="app-bar">
          <span class="brand">
            <span class="brand-dot"></span>
            MeetNotes
          </span>
          <nav class="app-nav">
            <a routerLink="/record" routerLinkActive="active">Record</a>
            <a routerLink="/library" routerLinkActive="active">Meetings</a>
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
        gap: var(--space-2);
        font-size: 0.9375rem;
        font-weight: 650;
        letter-spacing: -0.01em;
        color: var(--text-primary);
      }
      .brand-dot {
        width: 9px;
        height: 9px;
        border-radius: 50%;
        background: var(--accent-gradient);
        box-shadow: var(--shadow-accent);
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
export class AppComponent {
  private readonly router = inject(Router);

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
}
