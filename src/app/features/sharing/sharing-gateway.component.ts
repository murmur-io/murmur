import {
  ChangeDetectionStrategy,
  Component,
  inject,
  signal,
} from "@angular/core";
import { Router } from "@angular/router";
import { IpcService } from "../../core/ipc.service";
import { SharingAuthFlowComponent } from "./sharing-auth-flow.component";

/**
 * SharingGatewayComponent — the first-run SHARING gateway at route `/welcome`.
 *
 * A full-bleed welcome (a shell child route, so the packaged WKWebView
 * style-resolves it — trap T4: a screen must be router-mounted, never rendered
 * directly in AppComponent's static host). Shown by `app.component.ts` (and the
 * onboarding handoff) only when `!cfg.sharingChoiceMade && !accountStatus.loggedIn`.
 *
 * Two doors ARE the first-run decision — there is no silent skip:
 *   - "Use Murmur locally — no account" → `markSharingChoiceMade()` then `/record`.
 *   - "Create or sign in to a sharing account" → the reusable `<app-sharing-auth-flow>`.
 *
 * The gate NEVER traps: every IPC failure is caught and falls through to
 * `/record`, so a broken/unavailable backend can never strand the user here.
 *
 * The gateway hosts the flow inside its OWN full-bleed frosted `.card` panel
 * (this is a whole PAGE, in-flow — not a floating popover, so the frosted card
 * is correct; the OPAQUE-overlay rule T3 is for the Settings modal host).
 */
@Component({
  selector: "app-sharing-gateway",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [SharingAuthFlowComponent],
  template: `
    <div class="gw">
      <div class="gw-stage">
        @switch (mode()) {
          @case ("pick") {
            <div class="gw-hero">
              <span class="orb-brand" aria-hidden="true">
                <span class="orb-core"></span>
              </span>
              <h1 class="gw-title">
                <span class="brand-dot" aria-hidden="true"></span>
                Share your notes — or keep it all local
              </h1>
              <p class="gw-lede text-secondary">
                Murmur works fully offline. If you want to share a note as a
                private, end-to-end-encrypted link, you can add a sharing account
                — otherwise everything stays on this Mac.
              </p>
            </div>

            <div class="gw-cards">
              <button
                type="button"
                class="gw-card"
                (click)="chooseLocal()"
                [disabled]="busy()"
              >
                <span class="gw-card-icon" aria-hidden="true">
                  <svg viewBox="0 0 24 24" width="24" height="24" fill="none">
                    <rect
                      x="4"
                      y="5"
                      width="16"
                      height="10"
                      rx="1.8"
                      stroke="currentColor"
                      stroke-width="1.5"
                    />
                    <path
                      d="M8.5 19h7M12 15v4"
                      stroke="currentColor"
                      stroke-width="1.5"
                      stroke-linecap="round"
                    />
                  </svg>
                </span>
                <span class="gw-card-name">Use Murmur locally — no account</span>
                <span class="gw-card-sub text-secondary">
                  Everything stays on this Mac. You can add an account later in
                  Settings.
                </span>
                <span class="gw-card-cta">
                  {{ busy() ? "Starting…" : "Continue" }}
                  <span class="cta-arrow" aria-hidden="true">→</span>
                </span>
              </button>

              <button
                type="button"
                class="gw-card is-accent"
                (click)="openAccount()"
                [disabled]="busy()"
              >
                <span class="gw-card-icon" aria-hidden="true">
                  <svg viewBox="0 0 24 24" width="24" height="24" fill="none">
                    <path
                      d="M12 2.5 4.5 5.5v5.2c0 4.6 3.1 8.1 7.5 9.3 4.4-1.2 7.5-4.7 7.5-9.3V5.5L12 2.5Z"
                      stroke="currentColor"
                      stroke-width="1.4"
                      stroke-linejoin="round"
                    />
                    <path
                      d="M9 11.5 11 13.5 15 9"
                      stroke="currentColor"
                      stroke-width="1.6"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                    />
                  </svg>
                </span>
                <span class="gw-card-name">
                  Create or sign in to a sharing account
                </span>
                <span class="gw-card-sub text-secondary">
                  End-to-end-encrypted note sharing. Only ciphertext leaves this
                  Mac.
                </span>
                <span class="gw-card-cta">
                  Set up sharing
                  <span class="cta-arrow" aria-hidden="true">→</span>
                </span>
              </button>
            </div>
          }

          @case ("account") {
            <div class="gw-panel card">
              <app-sharing-auth-flow
                (completed)="onCompleted()"
                (dismissed)="onDismissed()"
              />
            </div>
          }
        }
      </div>
    </div>
  `,
  styles: [
    `
      .gw {
        display: flex;
        align-items: center;
        justify-content: center;
        min-height: calc(100vh - 120px);
        padding: var(--space-5) 0 var(--space-7);
      }
      .gw-stage {
        position: relative;
        width: 100%;
        max-width: 620px;
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
        animation: gw-in 460ms var(--ease-spring) both;
      }
      @keyframes gw-in {
        from {
          opacity: 0;
          transform: translateY(14px) scale(0.985);
        }
        to {
          opacity: 1;
          transform: translateY(0) scale(1);
        }
      }

      /* Hero. */
      .gw-hero {
        display: flex;
        flex-direction: column;
        align-items: center;
        text-align: center;
        gap: var(--space-3);
      }
      .orb-brand {
        position: relative;
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 88px;
        height: 88px;
        margin-bottom: var(--space-1);
      }
      .orb-core {
        width: 58px;
        height: 58px;
        border-radius: 50%;
        background: var(--accent-gradient);
        box-shadow:
          var(--shadow-accent),
          0 0 40px rgba(110, 118, 255, 0.55),
          inset 0 2px 6px rgba(255, 255, 255, 0.4);
        animation: orb-float 4s ease-in-out infinite;
      }
      .orb-brand::before,
      .orb-brand::after {
        content: "";
        position: absolute;
        inset: 0;
        border-radius: 50%;
        border: 1.5px solid var(--accent);
        opacity: 0.5;
        animation: orb-ring 3s ease-in-out infinite;
      }
      .orb-brand::after {
        animation-delay: 1.5s;
      }
      @keyframes orb-ring {
        0% {
          transform: scale(0.66);
          opacity: 0.6;
        }
        100% {
          transform: scale(1.1);
          opacity: 0;
        }
      }
      @keyframes orb-float {
        0%,
        100% {
          transform: translateY(0) scale(1);
        }
        50% {
          transform: translateY(-6px) scale(1.03);
        }
      }
      .gw-title {
        display: inline-flex;
        align-items: center;
        gap: var(--space-3);
        margin: 0;
        font-size: 1.6rem;
        font-weight: 650;
        letter-spacing: -0.025em;
        max-width: 22ch;
      }
      .brand-dot {
        width: 10px;
        height: 10px;
        min-width: 10px;
        border-radius: 50%;
        background: var(--accent-gradient);
        box-shadow: var(--shadow-accent);
      }
      .gw-lede {
        margin: 0;
        max-width: 46ch;
        font-size: 1.0125rem;
        line-height: 1.6;
      }

      /* The two choice cards. */
      .gw-cards {
        display: grid;
        grid-template-columns: 1fr 1fr;
        gap: var(--space-4);
      }
      @media (max-width: 560px) {
        .gw-cards {
          grid-template-columns: 1fr;
        }
      }
      .gw-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        text-align: left;
        padding: var(--space-5);
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-lg);
        background: var(--surface-raised);
        color: var(--text-primary);
        font-family: inherit;
        cursor: pointer;
        transition:
          border-color var(--transition),
          background var(--transition),
          box-shadow var(--transition),
          transform var(--transition-fast);
      }
      .gw-card:hover {
        border-color: var(--border-strong);
        background: var(--surface-hover);
        transform: translateY(-2px);
      }
      .gw-card:active {
        transform: translateY(0);
      }
      .gw-card:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .gw-card:disabled {
        opacity: 0.6;
        cursor: default;
        transform: none;
      }
      .gw-card.is-accent {
        border-color: transparent;
        background: var(--accent-soft);
        box-shadow: 0 0 0 1px var(--accent-ring);
      }
      .gw-card-icon {
        display: inline-flex;
        color: var(--accent-hover);
        margin-bottom: var(--space-1);
      }
      .gw-card-name {
        font-size: 1.02rem;
        font-weight: 620;
        letter-spacing: -0.01em;
        line-height: 1.35;
      }
      .gw-card-sub {
        font-size: 0.875rem;
        line-height: 1.5;
        flex: 1;
      }
      .gw-card-cta {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        margin-top: var(--space-2);
        color: var(--accent-hover);
        font-size: 0.9rem;
        font-weight: 600;
      }
      .cta-arrow {
        transition: transform var(--transition);
      }
      .gw-card:hover .cta-arrow {
        transform: translateX(3px);
      }

      /* Account panel — a full-bleed frosted card hosting the reusable flow.
         This is a whole PAGE (in-flow), so the frosted .card is correct here;
         the OPAQUE-overlay rule (T3) governs the Settings floating modal. */
      .gw-panel {
        padding: var(--space-6);
      }

      @media (prefers-reduced-motion: reduce) {
        .orb-core,
        .orb-brand::before,
        .orb-brand::after,
        .gw-stage {
          animation: none !important;
        }
      }
    `,
  ],
})
export class SharingGatewayComponent {
  private readonly ipc = inject(IpcService);
  private readonly router = inject(Router);

  /** `pick` = the two doors; `account` = the reusable multi-step flow. */
  readonly mode = signal<"pick" | "account">("pick");
  /** Debounces the local-choice button (its IPC + navigate). */
  readonly busy = signal(false);

  /** Door (a): resolve the decision as local-only, then enter the app. */
  async chooseLocal(): Promise<void> {
    if (this.busy()) {
      return;
    }
    this.busy.set(true);
    try {
      await this.ipc.markSharingChoiceMade();
    } catch {
      // Never trap the user on a persistence failure — the gate re-offers next
      // launch, which is acceptable (they made no lasting decision).
    }
    await this.router.navigate(["/record"]);
  }

  /** Door (b): open the account flow inside the gateway's card panel. */
  openAccount(): void {
    this.mode.set("account");
  }

  /** The flow logged the user in → mark the choice made, then enter the app. */
  async onCompleted(): Promise<void> {
    try {
      await this.ipc.markSharingChoiceMade();
    } catch {
      // Non-fatal — proceed into the app regardless.
    }
    await this.router.navigate(["/record"]);
  }

  /** The user backed out of the flow → return to the two doors. */
  onDismissed(): void {
    this.mode.set("pick");
  }
}
