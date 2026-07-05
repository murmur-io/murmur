import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  input,
  output,
} from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import type { ProviderStatus } from "../../../../core/models";
import { SettingsStore } from "../../settings.store";

/** Render-ready view-model for one connection card (built by the parent). */
export interface ConnectionCardVm {
  readonly id: string;
  readonly name: string;
  readonly status: ProviderStatus | null;
  readonly expanded: boolean;
  readonly cloud: boolean;
  /** True when the current posture actively routes work to this engine now. */
  readonly inUse: boolean;
}

/**
 * ONE provider connection card: name + status pill (from the availability
 * fan-out), a privacy line, Test, and a Configure DISCLOSURE (in-flow expand,
 * default collapsed — not a floating overlay) holding exactly the controls
 * the old Providers section had for this connection. The gateway disclosure
 * is the ENTIRE old gateway card, now always reachable (no
 * `providerId === 'gateway'` gate). Expand/Test state lives in the parent;
 * the form + key controls come from the shell-provided SettingsStore.
 */
@Component({
  selector: "app-ai-connection-card",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  template: `
    <div class="conn-card" [formGroup]="form">
      <div class="conn-row">
        <div class="conn-main">
          <span class="conn-name">{{ card().name }}</span>
          @if (card().status?.available) {
            <span class="pill is-success">
              <span class="pill-dot"></span>
              Ready
            </span>
          } @else if (card().status) {
            <span class="pill is-warning">
              <span class="pill-dot"></span>
              Needs setup
            </span>
          } @else {
            <span class="pill">
              <span class="pill-dot"></span>
              Not set up
            </span>
          }
          @if (card().inUse) {
            <span class="pill conn-inuse">
              <span class="pill-dot"></span>
              In use now
            </span>
          }
        </div>
        <div class="conn-actions">
          <button
            type="button"
            class="btn btn-ghost btn-sm"
            (click)="probe.emit()"
            [disabled]="testing()"
          >
            {{ testing() ? "Testing…" : "Test" }}
          </button>
          <button
            type="button"
            class="btn btn-sm"
            (click)="toggleConfigure.emit()"
            [attr.aria-expanded]="card().expanded"
          >
            Configure
            <svg
              class="conn-chevron"
              [class.is-open]="card().expanded"
              viewBox="0 0 16 16"
              width="12"
              height="12"
              fill="none"
              stroke="currentColor"
              stroke-width="1.8"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path d="M4 6.5 8 10.5 12 6.5" />
            </svg>
          </button>
        </div>
      </div>

      <span class="conn-privacy text-muted">
        {{
          card().cloud
            ? "Cloud — redacted first"
            : "On this Mac — nothing leaves"
        }}
      </span>
      @if (card().id === "ollama") {
        <span class="conn-reason text-muted">
          Your own local model server — separate from the built-in models.
        </span>
      }
      @if (unavailableReason(); as reason) {
        <span class="conn-reason text-muted">{{ reason }}</span>
      }

      @if (card().expanded) {
        <div class="conn-config">
          @switch (card().id) {
            @case ("claude_code") {
              <label class="field">
                <span class="field-label">Claude binary</span>
                <input formControlName="claudeBinary" />
              </label>

              <label class="toggle-row">
                <span class="toggle-copy">
                  <span class="toggle-title"
                    >Pass shell environment to the Claude CLI</span
                  >
                  <span class="text-secondary toggle-sub">
                    Restores older-version behavior: an ANTHROPIC_API_KEY (and
                    proxy / base-URL vars) set in your shell reach the claude
                    CLI again, so it can authenticate via your env key. Off by
                    default for security — your database encryption keys are
                    never passed through.
                  </span>
                </span>
                <input type="checkbox" formControlName="claudeCodeInheritEnv" />
              </label>
            }
            @case ("anthropic") {
              <div class="key-status">
                <span class="text-secondary">API key</span>
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
            }
            @case ("ollama") {
              <label class="field">
                <span class="field-label">Ollama base URL</span>
                <input formControlName="ollamaBaseUrl" />
                <span class="field-help text-muted">
                  A non-localhost URL makes Ollama count as cloud — the card
                  moves to the Cloud group and its text is redacted and
                  consent-gated like any other cloud provider.
                </span>
              </label>

              <label class="field">
                <span class="field-label">Ollama model</span>
                <input formControlName="ollamaModel" />
              </label>
            }
            @case ("gateway") {
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
                  https://…/v1) — or the full chat-completions endpoint if your
                  gateway uses a custom route (e.g. a Kong serverless route
                  like https://…/test).
                </span>
              </label>

              <div class="field">
                <span class="field-label">Model</span>
                <div class="gateway-model-row">
                  @if (gatewayModels().length > 0) {
                    <select
                      formControlName="gatewayModel"
                      class="gateway-model-select"
                    >
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
                    Couldn't load models — check the base URL and key, or type
                    the model id manually.
                  </span>
                } @else {
                  <span class="field-help text-muted">
                    Sent as the <code>model</code> field in every request —
                    leave blank to let the gateway choose.
                  </span>
                }
              </div>

              <!-- Gateway health probe -->
              <div class="gateway-health-row">
                <span class="text-secondary">Gateway status</span>
                <div class="gateway-health-status">
                  @if (gatewayHealth(); as h) {
                    @if (h.reachable) {
                      <span class="pill is-success">
                        <span class="pill-dot"></span>
                        {{ h.modelCount }}
                        {{ h.modelCount === 1 ? "model" : "models" }} reachable
                      </span>
                    } @else {
                      <span class="pill">
                        <span class="pill-dot gateway-dot-unreachable"></span>
                        Gateway unreachable
                      </span>
                    }
                  } @else {
                    <span class="text-muted gateway-health-hint"
                      >Not checked</span
                    >
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
                <p class="text-danger gateway-key-error">
                  {{ gatewayKeyError() }}
                </p>
              }

              <!-- Destination banner: calmer note for localhost, warning for remote -->
              @if (gatewayDestination(); as dest) {
                @if (dest.isRemote) {
                  <div class="banner is-warning gateway-banner">
                    <span class="banner-icon" aria-hidden="true">!</span>
                    <span>
                      Content will be sent to
                      <strong>{{ dest.host }}</strong> over the network —
                      always scrubbed by the redaction firewall first and
                      requires cloud-egress consent.
                    </span>
                  </div>
                } @else {
                  <div class="banner gateway-banner">
                    <span class="banner-icon" aria-hidden="true">i</span>
                    <span>
                      Localhost gateway — a local gateway can still forward to
                      the cloud, so content is still redacted and
                      consent-gated.
                    </span>
                  </div>
                }
              }
            }
          }
        </div>
      }
    </div>
  `,
  styles: [
    `
      /* One connection = an inset row card (the brain-model-row language). */
      .conn-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        padding: var(--space-3) var(--space-4);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
      }
      .conn-row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
        flex-wrap: wrap;
      }
      .conn-main {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        flex-wrap: wrap;
        min-width: 0;
      }
      .conn-name {
        color: var(--text-primary);
        font-weight: 550;
        font-size: 0.95rem;
      }
      /* "In use now" — accent (not the green Ready) so availability vs active read apart. */
      .conn-inuse {
        color: var(--accent-hover);
        background: var(--accent-soft);
        border-color: transparent;
      }
      .conn-actions {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        flex: none;
      }
      .conn-privacy,
      .conn-reason {
        font-size: 0.8125rem;
        line-height: 1.5;
      }
      .conn-chevron {
        margin-left: var(--space-1);
        transition: transform var(--transition-fast);
      }
      .conn-chevron.is-open {
        transform: rotate(180deg);
      }
      .btn-sm {
        height: 30px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
      }

      /* The Configure DISCLOSURE — in-flow (expands the card), NOT a floating
         overlay, so the frosted/inset surfaces are correct (no opacity trap). */
      .conn-config {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
        margin-top: var(--space-2);
        padding-top: var(--space-3);
        border-top: 1px solid var(--border-subtle);
      }

      /* --- Stacked label + control (shared section language) --- */
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
        min-width: 0;
      }
      .row .btn {
        flex: none;
      }
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
      .key-status {
        display: flex;
        align-items: center;
        gap: var(--space-3);
      }

      /* --- AI Gateway rows (moved verbatim from the old Providers section) --- */
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
      .gateway-health-row {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
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
      .gateway-key-error {
        margin: 0;
        font-size: 0.85rem;
      }
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
export class AiConnectionCardComponent {
  private readonly store = inject(SettingsStore);

  /** The render-ready card (status + group + disclosure state), parent-built. */
  readonly card = input.required<ConnectionCardVm>();
  /** True while the parent has a Test probe in flight (disables the button). */
  readonly testing = input(false);

  /** Open/close this card's Configure disclosure (state lives in the parent). */
  readonly toggleConfigure = output<void>();
  /** Run the availability probe for this connection (parent orchestrates). */
  readonly probe = output<void>();

  readonly form = this.store.form;
  readonly keyControl = this.store.keyControl;
  readonly gatewayKeyControl = this.store.gatewayKeyControl;
  readonly hasKey = this.store.hasKey;
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

  /** The reason line, shown only for a probed-but-unavailable connection. */
  readonly unavailableReason = computed(() => {
    const c = this.card();
    return c.status && !c.status.available && c.status.reason
      ? c.status.reason
      : null;
  });

  saveKey(): void {
    void this.store.saveKey();
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
