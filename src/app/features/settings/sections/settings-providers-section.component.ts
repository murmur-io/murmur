import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../settings.store";

/**
 * Settings → providers section (Stage-1 split): the `@case ("providers")` block of the
 * former settings.component.ts monolith, moved VERBATIM. State/actions live in
 * the shell-provided SettingsStore so section switches never drop them.
 */
@Component({
  selector: "app-settings-providers-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  template: `
    <div class="section-stack" [formGroup]="form">
              <!-- Provider configuration -->
              <div class="card">
                <fieldset>
                  <legend>Provider configuration</legend>

                  <label class="field">
                    <span class="field-label">Anthropic model</span>
                    <input formControlName="anthropicModel" />
                  </label>

                  <label class="field">
                    <span class="field-label">Ollama base URL</span>
                    <input formControlName="ollamaBaseUrl" />
                  </label>

                  <label class="field">
                    <span class="field-label">Ollama model</span>
                    <input formControlName="ollamaModel" />
                  </label>

                  <label class="field">
                    <span class="field-label">Claude binary</span>
                    <input formControlName="claudeBinary" />
                  </label>

                  <label class="toggle-row">
                    <span class="toggle-copy">
                      <span class="toggle-title">Pass shell environment to the Claude CLI</span>
                      <span class="text-secondary toggle-sub">
                        Restores older-version behavior: an ANTHROPIC_API_KEY (and proxy /
                        base-URL vars) set in your shell reach the claude CLI again, so it
                        can authenticate via your env key. Off by default for security — your
                        database encryption keys are never passed through.
                      </span>
                    </span>
                    <input type="checkbox" formControlName="claudeCodeInheritEnv" />
                  </label>
                </fieldset>
              </div>

              <!-- AI Gateway configuration (shown only when "gateway" provider is selected) -->
              @if (form.controls.providerId.value === 'gateway') {
                <div class="card gateway-card">
                  <fieldset>
                    <legend>AI Gateway</legend>

                    <label class="field">
                      <span class="field-label">Base URL</span>
                      <input
                        formControlName="gatewayBaseUrl"
                        placeholder="http://localhost:4000/v1"
                        autocomplete="off"
                        spellcheck="false"
                      />
                      @if (gatewayUrlWarning()) {
                        <span class="field-help text-danger">
                          Use https:// (http:// is allowed only for localhost).
                        </span>
                      }
                      <span class="field-help text-muted">
                        Enter your gateway's OpenAI-compatible base URL (e.g.
                        https://…/v1) — or the full chat-completions endpoint if
                        your gateway uses a custom route (e.g. a Kong serverless
                        route like https://…/test).
                      </span>
                    </label>

                    <div class="field">
                      <span class="field-label">Model</span>
                      <div class="gateway-model-row">
                        @if (gatewayModels().length > 0) {
                          <select formControlName="gatewayModel" class="gateway-model-select">
                            <option value="">Gateway default</option>
                            @for (m of gatewayModels(); track m.id) {
                              <option [value]="m.id">{{ m.id }}</option>
                            }
                            <!--
                              If the currently-saved model is not in the catalog (e.g. the
                              catalog changed), keep it selectable so a manually-typed value
                              is never silently lost. gatewayModelIsCustom() is a computed
                              to avoid arrow-function syntax in the template.
                            -->
                            @if (gatewayModelIsCustom()) {
                              <option [value]="form.controls.gatewayModel.value">
                                {{ form.controls.gatewayModel.value }} (custom)
                              </option>
                            }
                          </select>
                        } @else {
                          <input
                            formControlName="gatewayModel"
                            placeholder="gpt-4o (leave blank to use the gateway default)"
                            autocomplete="off"
                            spellcheck="false"
                            class="gateway-model-input"
                          />
                        }
                        <button
                          type="button"
                          class="btn btn-ghost gateway-model-refresh"
                          (click)="refreshGatewayModels()"
                          [disabled]="gatewayModelsLoading()"
                          title="Fetch models from the gateway's /v1/models endpoint"
                        >
                          @if (gatewayModelsLoading()) {
                            Loading…
                          } @else {
                            ↻ Refresh models
                          }
                        </button>
                      </div>
                      @if (gatewayModelError()) {
                        <span class="field-help text-muted">
                          Couldn't load models — check the base URL and key, or type the
                          model id manually.
                        </span>
                      } @else {
                        <span class="field-help text-muted">
                          Sent as the <code>model</code> field in every request — leave
                          blank to let the gateway choose.
                        </span>
                      }
                    </div>

                    <!-- AI Gateway (Phase 4) — health probe -->
                    <div class="gateway-health-row">
                      <span class="text-secondary">Gateway status</span>
                      <div class="gateway-health-status">
                        @if (gatewayHealth(); as h) {
                          @if (h.reachable) {
                            <span class="pill is-success">
                              <span class="pill-dot"></span>
                              {{ h.modelCount }} {{ h.modelCount === 1 ? 'model' : 'models' }} reachable
                            </span>
                          } @else {
                            <span class="pill">
                              <span class="pill-dot gateway-dot-unreachable"></span>
                              Gateway unreachable
                            </span>
                          }
                        } @else {
                          <span class="text-muted gateway-health-hint">Not checked</span>
                        }
                        <button
                          type="button"
                          class="btn btn-ghost gateway-health-btn"
                          (click)="checkGatewayHealth()"
                          [disabled]="gatewayHealthChecking()"
                        >
                          @if (gatewayHealthChecking()) {
                            Checking…
                          } @else {
                            Check
                          }
                        </button>
                      </div>
                    </div>

                    <!-- Gateway API key (optional) -->
                    <div class="key-status">
                      <span class="text-secondary">
                        API key
                        <span class="text-muted">(optional)</span>
                      </span>
                      @if (hasGatewayKey()) {
                        <span class="pill is-success">
                          <span class="pill-dot"></span>
                          Set
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
                        [formControl]="gatewayKeyControl"
                        placeholder="sk-… or any bearer token"
                        autocomplete="new-password"
                      />
                      <button
                        type="button"
                        class="btn"
                        (click)="saveGatewayKey()"
                        [disabled]="!gatewayKeyControl.value.trim()"
                      >
                        Save key
                      </button>
                      @if (hasGatewayKey()) {
                        <button
                          type="button"
                          class="btn btn-ghost"
                          (click)="removeGatewayKey()"
                        >
                          Clear
                        </button>
                      }
                    </span>
                    @if (gatewayKeyError()) {
                      <p class="text-danger gateway-key-error">{{ gatewayKeyError() }}</p>
                    }
                  </fieldset>

                  <!-- Destination banner: calmer note for localhost, warning for remote -->
                  @if (gatewayDestination(); as dest) {
                    @if (dest.isRemote) {
                      <div class="banner is-warning gateway-banner">
                        <span class="banner-icon" aria-hidden="true">!</span>
                        <span>
                          Content will be sent to <strong>{{ dest.host }}</strong> over
                          the network — always scrubbed by the redaction firewall first
                          and requires cloud-egress consent.
                        </span>
                      </div>
                    } @else {
                      <div class="banner gateway-banner">
                        <span class="banner-icon" aria-hidden="true">i</span>
                        <span>
                          Localhost gateway — a local gateway can still forward to the
                          cloud, so content is still redacted and consent-gated.
                        </span>
                      </div>
                    }
                  }
                </div>
              }

              <!-- Anthropic API key -->
              <div class="card">
                <fieldset>
                  <legend>Anthropic API key</legend>
                  <div class="key-status">
                    <span class="text-secondary">Status</span>
                    @if (hasKey()) {
                      <span class="pill is-success">
                        <span class="pill-dot"></span>
                        Set
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
                      [formControl]="keyControl"
                      placeholder="sk-ant-…"
                    />
                    <button type="button" class="btn" (click)="saveKey()">
                      Save key
                    </button>
                  </span>
                </fieldset>
              </div>

              <!-- Provider availability -->
              <div class="card">
                <div class="provider-avail-head">
                  <h3>Provider availability</h3>
                  <button type="button" class="btn btn-sm" (click)="refreshProviders()">
                    Check providers
                  </button>
                </div>
                <ul class="provider-list">
                  @for (p of providers(); track p.id) {
                    <li class="provider-row">
                      <span class="provider-name">{{ p.id }}</span>
                      @if (p.available) {
                        <span class="pill is-success">
                          <span class="pill-dot"></span>
                          Available
                        </span>
                      } @else {
                        <span class="provider-unavailable">
                          <span class="pill is-danger">
                            <span class="pill-dot"></span>
                            Unavailable
                          </span>
                          @if (p.reason) {
                            <span class="text-muted provider-reason">{{
                              p.reason
                            }}</span>
                          }
                        </span>
                      }
                    </li>
                  }
                </ul>
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

      /* --- Stacked label + control --- */
      .field {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .field-label {
        color: var(--text-secondary);
        font-size: 0.9rem;
        font-weight: 550;
      }

      /* One-line helper that tracks the selected summary style. */
      .field-help {
        font-size: 0.8125rem;
        line-height: 1.5;
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

      /* --- API-key status --- */
      .key-status {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        margin-bottom: var(--space-2);
      }

      /* --- Provider availability list --- */
      .provider-avail-head {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
        margin-bottom: var(--space-2);
      }
      .provider-avail-head h3 {
        margin: 0;
      }
      .provider-list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
      }
      .provider-row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
        padding: var(--space-3) 0;
        border-bottom: 1px solid var(--border-subtle);
      }
      .provider-row:last-child {
        border-bottom: none;
      }
      .provider-name {
        color: var(--text-primary);
        font-weight: 550;
        font-family: var(--font-mono);
        font-size: 0.875rem;
      }
      .provider-unavailable {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        flex-wrap: wrap;
        justify-content: flex-end;
      }
      .provider-reason {
        font-size: 0.8125rem;
      }

      /* --- AI Gateway (Phase 3) — live model picker row --- */
      .gateway-model-row {
        display: flex;
        gap: var(--space-2);
        align-items: center;
        flex-wrap: wrap;
      }
      .gateway-model-select,
      .gateway-model-input {
        flex: 1 1 auto;
        min-width: 0;
      }
      .gateway-model-refresh {
        flex: none;
        height: 36px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
        white-space: nowrap;
      }

      /* --- AI Gateway (Phase 4) — health probe row --- */
      .gateway-health-row {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
        margin-bottom: var(--space-2);
      }
      .gateway-health-status {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex-wrap: wrap;
      }
      .gateway-health-hint {
        font-size: 0.8125rem;
      }
      .gateway-dot-unreachable {
        background: var(--text-muted);
      }
      .gateway-health-btn {
        height: 28px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
        white-space: nowrap;
      }

      /* --- Banner icon (matches the record screen) --- */
      .banner-icon {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 24px;
        height: 24px;
        min-width: 24px;
        border-radius: 50%;
        background: rgba(255, 255, 255, 0.08);
        font-weight: 700;
        font-size: 0.85rem;
        line-height: 1;
      }
    `,
  ],
})
export class SettingsProvidersSectionComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;
  readonly keyControl = this.store.keyControl;
  readonly gatewayKeyControl = this.store.gatewayKeyControl;
  readonly hasKey = this.store.hasKey;
  readonly providers = this.store.providers;
  readonly gatewayUrlWarning = this.store.gatewayUrlWarning;
  readonly gatewayModels = this.store.gatewayModels;
  readonly gatewayModelsLoading = this.store.gatewayModelsLoading;
  readonly gatewayModelError = this.store.gatewayModelError;
  readonly gatewayModelIsCustom = this.store.gatewayModelIsCustom;
  readonly gatewayHealth = this.store.gatewayHealth;
  readonly gatewayHealthChecking = this.store.gatewayHealthChecking;
  readonly hasGatewayKey = this.store.hasGatewayKey;
  readonly gatewayKeyError = this.store.gatewayKeyError;
  readonly gatewayDestination = this.store.gatewayDestination;

  saveKey(): void {
    void this.store.saveKey();
  }

  refreshProviders(): void {
    void this.store.refreshProviders();
  }

  refreshGatewayModels(): void {
    void this.store.refreshGatewayModels();
  }

  checkGatewayHealth(): void {
    void this.store.checkGatewayHealth();
  }

  saveGatewayKey(): void {
    void this.store.saveGatewayKey();
  }

  removeGatewayKey(): void {
    void this.store.removeGatewayKey();
  }
}
