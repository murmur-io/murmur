import {
  ChangeDetectionStrategy,
  Component,
  inject,
  signal,
} from "@angular/core";
import { SettingsStore } from "../../settings.store";

/**
 * AI & Models → Block C: WHERE YOUR TEXT GOES. A two-line privacy strip
 * (what always stays on-device; where the default connection sends redacted
 * text — hidden entirely when the default is a local Ollama), plus the
 * cloud-processing consent state with the existing Allow flow AND the new
 * Revoke (an inline two-step confirm, NOT a browser confirm() and NOT a
 * floating overlay). Reads the same store state as the Privacy section's
 * canonical consent card, so the two surfaces can't diverge.
 */
@Component({
  selector: "app-ai-privacy-strip",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="card strip-card">
      <div class="strip-head">
        <h3>Where your text goes</h3>
      </div>

      <div class="strip-lines">
        <div class="strip-line">
          <span class="strip-dot is-local" aria-hidden="true"></span>
          <span class="text-secondary">
            <strong class="strip-strong">Stays on this Mac:</strong>
            transcription, embeddings, name redaction, proactive hints, local
            models
          </span>
        </div>

        @if (defaultEgressDestination(); as dest) {
          <div class="strip-line">
            <span class="strip-dot is-cloud" aria-hidden="true"></span>
            <span class="text-secondary">
              <strong class="strip-strong">Leaves this Mac (redacted first):</strong>
              {{ dest.connection }} → {{ dest.destination }}
            </span>
          </div>
        }
      </div>

      <div class="strip-consent">
        <span class="text-secondary">Cloud processing</span>
        @if (cloudConsented()) {
          <span class="pill is-success">
            <span class="pill-dot"></span>
            Allowed ✓
          </span>
          @if (confirmingRevoke()) {
            <span class="revoke-confirm">
              <span class="text-secondary revoke-question">
                Really revoke? Cloud AI will stop working until re-allowed.
              </span>
              <button
                type="button"
                class="btn revoke-btn"
                (click)="confirmRevoke()"
                [disabled]="revoking()"
              >
                @if (revoking()) {
                  <span class="spin-ring" aria-hidden="true"></span>
                  Revoking…
                } @else {
                  Revoke consent
                }
              </button>
              <button
                type="button"
                class="btn btn-ghost"
                (click)="cancelRevoke()"
                [disabled]="revoking()"
              >
                Keep
              </button>
            </span>
          } @else {
            <button
              type="button"
              class="btn btn-ghost revoke-btn"
              (click)="startRevoke()"
            >
              Revoke
            </button>
          }
        } @else {
          <span class="pill">
            <span class="pill-dot"></span>
            Not granted
          </span>
          <button
            type="button"
            class="btn btn-primary"
            (click)="allowCloudProcessing()"
            [disabled]="consenting()"
          >
            @if (consenting()) {
              <span class="spin-ring" aria-hidden="true"></span>
              Enabling…
            } @else {
              Allow
            }
          </button>
          <span class="text-muted strip-hint">
            One-time, redacted first — required before any cloud provider runs.
          </span>
        }
      </div>
      @if (consentError(); as cerr) {
        <p class="text-danger strip-error">{{ cerr }}</p>
      }
      @if (revokeError(); as rerr) {
        <p class="text-danger strip-error">{{ rerr }}</p>
      }
    </div>
  `,
  styles: [
    `
      :host {
        display: contents;
      }

      .strip-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .strip-head h3 {
        margin: 0;
      }

      .strip-lines {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .strip-line {
        display: flex;
        align-items: baseline;
        gap: var(--space-2);
        font-size: 0.9rem;
        line-height: 1.55;
      }
      .strip-strong {
        color: var(--text-primary);
        font-weight: 550;
      }
      .strip-dot {
        width: 8px;
        height: 8px;
        min-width: 8px;
        border-radius: 50%;
        transform: translateY(-1px);
      }
      .strip-dot.is-local {
        background: var(--success);
      }
      .strip-dot.is-cloud {
        background: var(--accent);
      }

      .strip-consent {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
        padding-top: var(--space-3);
        border-top: 1px solid var(--border-subtle);
      }
      .strip-hint {
        font-size: 0.85rem;
        line-height: 1.5;
      }
      .strip-error {
        margin: 0;
        font-size: 0.85rem;
      }

      /* Inline two-step revoke — stays in flow (no popover, no overlay). */
      .revoke-confirm {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        flex-wrap: wrap;
      }
      .revoke-question {
        font-size: 0.85rem;
        line-height: 1.5;
      }
      .revoke-btn {
        color: var(--danger);
      }

      .spin-ring {
        width: 15px;
        height: 15px;
        border-radius: 50%;
        border: 2px solid rgba(255, 255, 255, 0.35);
        border-top-color: var(--text-on-accent);
        animation: spin 0.8s linear infinite;
        margin-right: var(--space-2);
        vertical-align: -2px;
        display: inline-block;
      }
      @keyframes spin {
        to {
          transform: rotate(360deg);
        }
      }
      @media (prefers-reduced-motion: reduce) {
        .spin-ring {
          animation: none;
        }
      }
    `,
  ],
})
export class AiPrivacyStripComponent {
  private readonly store = inject(SettingsStore);

  readonly cloudConsented = this.store.cloudConsented;
  readonly consenting = this.store.consenting;
  readonly consentError = this.store.consentError;
  readonly revoking = this.store.revoking;
  readonly revokeError = this.store.revokeError;
  readonly defaultEgressDestination = this.store.defaultEgressDestination;

  /** True while the inline "Really revoke?" confirm step is showing. */
  readonly confirmingRevoke = signal(false);

  allowCloudProcessing(): void {
    void this.store.allowCloudProcessing();
  }

  startRevoke(): void {
    this.confirmingRevoke.set(true);
  }

  cancelRevoke(): void {
    this.confirmingRevoke.set(false);
  }

  async confirmRevoke(): Promise<void> {
    await this.store.revokeCloudProcessing();
    this.confirmingRevoke.set(false);
  }
}
