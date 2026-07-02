import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { RouterLink, RouterLinkActive, RouterOutlet } from "@angular/router";
import { FoldersService } from "./services/folders.service";
import { ToastService, type Toast } from "./services/toast.service";

/**
 * The app shell chrome — brand header, nav tabs, page layout (router-outlet) and
 * the toast viewport.
 *
 * Why this is a separate child component (and not just AppComponent's template):
 * it is the Tauri WKWebView FOUC fix. AppComponent's host is the STATIC
 * `<app-root>` element present in index.html at parse time; in this WKWebView,
 * styles for elements rendered as direct descendants of that pre-existing host
 * were never applied on cold launch (header/nav/layout rendered as raw unstyled
 * HTML) — even with the matching encapsulation id and the rule present in a
 * <style>. Every component with a DYNAMICALLY-created host (route pages, cards,
 * banners) renders correctly styled. So the shell lives here, under a host
 * Angular creates at runtime, and AppComponent renders `<app-shell>` only after
 * bootstrap — giving it the same reliable style resolution as a route page.
 */
@Component({
  selector: "app-shell",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterOutlet, RouterLink, RouterLinkActive],
  template: `
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
            <a routerLink="/brain" routerLinkActive="active">Brain</a>
            <a routerLink="/ask" routerLinkActive="active">Ask</a>
            <a routerLink="/settings" routerLinkActive="active">Settings</a>
          </nav>
        </div>
      </header>
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

  /** How many locked folders are unlocked (plaintext-exposed) this session. */
  readonly unlockedCount = this.folders.unlockedCount;

  /** The app-wide toast queue, rendered in the main-window viewport. */
  readonly toasts = this.toast.toasts;

  /** Dismiss a toast by id (also cancels its auto-dismiss timer in the service). */
  dismissToast(id: number): void {
    this.toast.dismiss(id);
  }

  /** Run a toast's inline action, then dismiss the toast. */
  runToastAction(t: Toast): void {
    t.action?.run();
    this.dismissToast(t.id);
  }
}
