import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../../settings.store";

/**
 * AI & Models → "Live during meetings" block (Task 5).
 *
 * Extracted verbatim from AiDefaultsBlockComponent as a standalone card.
 * Owns the two live-meeting toggles (in-meeting voice assistant +
 * proactive brain hints) and the cloud-egress consent warning that appears
 * when the in-meeting assistant is on, the live role resolves to a cloud
 * provider, and the user has not yet consented.
 *
 * Not an overlay — the consent banner is IN-FLOW (frosted `.banner`,
 * correct per angular-zoneless.md §T3).
 */
@Component({
  selector: "app-during-meetings-block",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  template: `
    <div class="card during-card" [formGroup]="form">
      <!-- ── Live during meetings ────────────────────────────────────── -->
      <div class="use-group">
        <span class="use-group-label text-muted">Live during meetings</span>

        <!-- The in-meeting voice assistant needs the on-device light engine for wake detection
             (brain_live), which the Cloud posture turns off — so it CANNOT run there. Hide it in
             Cloud rather than show a toggle that does nothing (the Cloud preset also forces it off
             backend-side). It stays available in Hybrid / Fully local. -->
        @if (posture() !== "cloud") {
          <label class="toggle-row">
            <span class="toggle-copy">
              <span class="toggle-title">In-meeting voice assistant</span>
              <span class="text-secondary toggle-sub">
                Listen for your wake phrase during a recording and answer
                grounded questions live, with sources. Off by default — it adds
                listening and (for cloud) sends audio-derived text mid-meeting.
              </span>
            </span>
            <input type="checkbox" formControlName="realtimeReactions" />
          </label>
        }

        <label class="toggle-row">
          <span class="toggle-copy">
            <span class="toggle-title">Proactive brain hints</span>
            <span class="text-secondary toggle-sub">
              While recording, surface a dismissible recall card when the
              conversation touches a past meeting, an open commitment, or a
              known fact. 100% on-device — no cloud calls; at most one card
              every two minutes.
            </span>
          </span>
          <input type="checkbox" formControlName="proactiveHintsEnabled" />
        </label>

        <!--
          Proactive cloud-egress consent (issue 20). The in-meeting assistant
          dispatches voice actions through the LIVE role's resolved target —
          since Stage 4 that need not be brainBackend/the default provider, so
          the condition keys on liveTargetIsCloud (the store's resolver
          mirror: explicit roleLiveConnection wins, "" falls back to the
          brainBackend mapping, ollama is cloud only off-loopback). Surface
          the requirement at enable time: realtime on, live target
          cloud-classified, not consented. Reuses the existing consent flow
          (allowCloudProcessing). In-flow warning, so the frosted banner is
          correct (no opaque overlay needed).
        -->
        @if (
          posture() !== "cloud" &&
          form.controls.realtimeReactions.value &&
          liveTargetIsCloud() &&
          !cloudConsented()
        ) {
          <div class="banner is-warning realtime-consent">
            <span class="realtime-consent-copy">
              ⚠ The in-meeting assistant sends live meeting context to your
              provider's cloud (redacted first). Allow cloud processing once,
              or live answers stay off.
            </span>
            <div class="cloud-consent-row">
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
              <span class="text-muted cloud-consent-hint">
                One-time, redacted first. Same consent as cloud summaries.
              </span>
            </div>
            @if (consentError(); as cerr) {
              <p class="text-danger privacy-note">{{ cerr }}</p>
            }
          </div>
        }
      </div>
    </div>
  `,
  styles: [
    `
      :host {
        display: contents;
      }

      .during-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }

      /* Light regrouping heading. */
      .use-group {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .use-group-label {
        font-size: 0.8125rem;
        font-weight: 550;
        letter-spacing: 0.01em;
        text-transform: uppercase;
      }

      /* #20 — proactive cloud-egress consent warning under the assistant toggle. */
      .realtime-consent {
        flex-direction: column;
        gap: var(--space-3);
      }
      .realtime-consent-copy {
        line-height: 1.55;
      }

      /* Toggle rows. */
      .toggle-row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-4);
        cursor: pointer;
      }
      .toggle-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .toggle-title {
        color: var(--text-primary);
        font-size: 0.95rem;
        font-weight: 550;
      }
      .toggle-sub {
        font-size: 0.85rem;
      }

      /* Cloud-processing consent — button + reassurance. */
      .cloud-consent-row {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
        margin-top: var(--space-1);
      }
      .cloud-consent-row .btn {
        flex: none;
      }
      .cloud-consent-hint {
        font-size: 0.85rem;
        line-height: 1.5;
      }
      .privacy-note {
        margin: 0;
        font-size: 0.9rem;
        line-height: 1.55;
      }

      /* Inline spinner on the Allow button. */
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
export class DuringMeetingsBlockComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;
  readonly cloudConsented = this.store.cloudConsented;
  readonly consenting = this.store.consenting;
  readonly consentError = this.store.consentError;
  readonly liveTargetIsCloud = this.store.liveTargetIsCloud;
  /** The derived brain posture — the in-meeting voice assistant is hidden under `cloud` (it needs the
   * on-device light engine, which Cloud turns off, so it can't be enabled there). */
  readonly posture = this.store.posture;

  allowCloudProcessing(): void {
    void this.store.allowCloudProcessing();
  }
}
