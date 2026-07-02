import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../settings.store";

/**
 * Settings → connectors section (Stage-1 split): the `@case ("connectors")` block of the
 * former settings.component.ts monolith, moved VERBATIM. State/actions live in
 * the shell-provided SettingsStore so section switches never drop them.
 */
@Component({
  selector: "app-settings-connectors-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  template: `
    <div class="section-stack" [formGroup]="form">
              <div class="card connectors-card">
                <div class="brain-copy">
                  <h3>Connectors</h3>
                  <p class="text-secondary brain-sub">
                    Let the brain reach beyond your notes. Connectors are
                    <strong>off by default</strong> — each one that leaves this Mac asks
                    for an explicit, one-time consent first.
                  </p>
                </div>

                <!-- Web search (Brave) connector -->
                <label class="toggle-row">
                  <span class="toggle-copy">
                    <span class="toggle-title">Web search</span>
                    <span class="text-secondary toggle-sub">
                      When enabled (and allowed below, with a key), the assistant can
                      look facts up on the web and cite them. Answers stay grounded in
                      your notes first; web results are added as “via web” sources.
                    </span>
                  </span>
                  <input type="checkbox" formControlName="webSearchEnabled" />
                </label>

                @if (form.controls.webSearchEnabled.value) {
                  <!-- Egress banner — make the new off-device path impossible to miss. -->
                  <p class="banner is-warning connector-egress" role="note">
                    <strong>This sends data off your Mac.</strong> When the brain runs a
                    web search, your (redacted) query leaves the device and goes to the
                    search provider (Brave). Only the query is sent — never your notes or
                    transcript. Disable this, or skip the consent below, to keep
                    everything local.
                  </p>

                  <!-- BYO API key (Brave) -->
                  <fieldset class="connector-fieldset">
                    <legend>Brave Search API key</legend>
                    <div class="key-status">
                      <span class="text-secondary">Status</span>
                      @if (hasWebKey()) {
                        <span class="pill is-success">
                          <span class="pill-dot"></span>
                          Key set ✓
                        </span>
                      } @else {
                        <span class="pill">
                          <span class="pill-dot"></span>
                          Not set
                        </span>
                      }
                    </div>
                    <span class="row">
                      <input
                        type="password"
                        [formControl]="webKeyControl"
                        placeholder="Brave Search API key"
                        autocomplete="off"
                      />
                      <button
                        type="button"
                        class="btn"
                        (click)="saveWebKey()"
                        [disabled]="savingWebKey()"
                      >
                        {{ savingWebKey() ? "Saving…" : "Save key" }}
                      </button>
                    </span>
                    <span class="field-help text-muted">
                      Bring your own key — it's stored in your macOS Keychain, never
                      logged, and never leaves with your notes.
                    </span>
                    @if (webKeyError(); as wkerr) {
                      <p class="text-danger brain-error">{{ wkerr }}</p>
                    }
                  </fieldset>

                  <!-- One-time egress consent (mirrors the Cloud-processing UX) -->
                  <div class="privacy-section connector-consent">
                    <span class="privacy-section-label text-muted"
                      >Allow web search</span
                    >
                    <p class="text-secondary privacy-note">
                      Your search query leaves this device for the search provider
                      (redacted first). Until you allow this once, web search stays off
                      and no query is ever sent.
                    </p>
                    @if (webConsented()) {
                      <span class="pill is-success cloud-consent-pill">
                        <span class="pill-dot"></span>
                        Web search allowed
                      </span>
                    } @else {
                      <div class="cloud-consent-row">
                        <button
                          type="button"
                          class="btn btn-primary"
                          (click)="allowWebSearch()"
                          [disabled]="webConsenting()"
                        >
                          @if (webConsenting()) {
                            <span class="spin-ring" aria-hidden="true"></span>
                            Enabling…
                          } @else {
                            Allow web search
                          }
                        </button>
                        <span class="text-muted cloud-consent-hint">
                          One-time. The brain works fully offline on your notes without
                          it.
                        </span>
                      </div>
                    }
                    @if (webConsentError(); as wcerr) {
                      <p class="text-danger privacy-note">{{ wcerr }}</p>
                    }
                  </div>
                }
              </div>
    </div>
  `,
  styles: [
    `
      /* Stage-1 split: the host stays layout-transparent so this section's
         cards remain direct flex items of the shell's .section-body (identical
         spacing to the pre-split monolith); .section-stack reproduces the
         .section-body column gap between this section's own cards. */
      :host {
        display: contents;
      }
      .section-stack {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
      }

      /* --- Connectors card (web search — NEW EGRESS) --- */
      .connectors-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .connector-egress {
        margin: 0;
        font-size: 0.875rem;
        line-height: 1.55;
      }
      .connector-fieldset {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .connector-consent {
        margin-top: var(--space-1);
      }

      .brain-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .brain-copy h3 {
        margin: 0;
      }
      .brain-sub {
        margin: 0;
        font-size: 0.875rem;
        line-height: 1.55;
      }

      .brain-error {
        margin: 0;
        font-size: 0.85rem;
      }

      /* --- Capture-system-audio toggle row --- */
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

      /* --- Cards stack their fieldset flush (card already provides padding) --- */
      .card fieldset {
        border: none;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .card fieldset legend {
        padding: 0;
        margin-bottom: var(--space-4);
        float: left;
        width: 100%;
        font-size: 0.8125rem;
      }

      /* --- API-key status --- */
      .key-status {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        margin-bottom: var(--space-2);
      }

      .row {
        display: flex;
        gap: var(--space-2);
      }
      .row input {
        flex: 1;
      }
      .row .btn {
        flex: none;
      }

      /* One-line helper that tracks the selected summary style. */
      .field-help {
        font-size: 0.8125rem;
        line-height: 1.5;
      }

      /* Each subsection: a small uppercase label over its explanatory note. */
      .privacy-section {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .privacy-section-label {
        font-size: 0.8125rem;
        font-weight: 550;
        letter-spacing: 0.01em;
        text-transform: uppercase;
      }
      .privacy-note {
        margin: 0;
        font-size: 0.9rem;
        line-height: 1.55;
      }

      /* Cloud-processing consent — button + reassurance, or the granted pill. */
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
      .cloud-consent-pill {
        align-self: flex-start;
      }

      /* Inline spinner on the Download button (matches the onboarding wizard). */
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
export class SettingsConnectorsSectionComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;
  readonly webKeyControl = this.store.webKeyControl;
  readonly hasWebKey = this.store.hasWebKey;
  readonly savingWebKey = this.store.savingWebKey;
  readonly webKeyError = this.store.webKeyError;
  readonly webConsented = this.store.webConsented;
  readonly webConsenting = this.store.webConsenting;
  readonly webConsentError = this.store.webConsentError;

  saveWebKey(): void {
    void this.store.saveWebKey();
  }

  allowWebSearch(): void {
    void this.store.allowWebSearch();
  }
}
