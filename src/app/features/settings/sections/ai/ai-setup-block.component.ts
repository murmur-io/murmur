import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
} from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../../settings.store";

/**
 * AI & Models → "Your setup" (NEW, posture-adaptive) — the second section of
 * the posture-driven redesign (docs/superpowers/specs/2026-07-05-…). Posture
 * picks the lane; this block shows ONLY what that lane needs to configure:
 *
 *   - Cloud            → the Default AI engine card.
 *   - Hybrid           → engine card + the reactions-only on-device picker.
 *   - Fully local      → the full on-device effort/language picker.
 *   - Custom           → engine card + on-device picker + a "Custom mix" note.
 *
 * The Default-engine + Default-model controls are the SAME `providerId` /
 * `providerModel` / `providerEffort` FormControls the old Advanced → Default
 * engine block owned (moved here verbatim, one source of truth — no new second
 * writer of any config key). The on-device model choice is delegated to the
 * self-contained `<app-model-effort-picker>` (Claude-style effort slider +
 * language toggle), which drives the existing store `useBrainModel` /
 * `downloadBrainModel` actions. Backend untouched.
 *
 * Rendering is posture-driven via a `setupCards()` computed → `@for` + inner
 * `@switch` so each card's markup is authored exactly once (no `ng-template` —
 * this codebase has zero, and `@for`/`@switch` keep it DRY). In-flow cards, not
 * overlays (T3).
 */
@Component({
  selector: "app-ai-setup-block",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  template: `
    @for (card of setupCards(); track card) {
      @switch (card) {
        @case ("engine") {
          <div class="card setup-card" [formGroup]="form">
            <div class="setup-head">
              <h3>Your default AI engine</h3>
              <p class="text-secondary setup-sub">
                Writes your notes, answers and briefs. Cloud engines are redacted
                before anything leaves.
              </p>
            </div>

            <label class="field">
              <span class="field-label">Engine</span>
              <select formControlName="providerId" (change)="onEngineChanged($event)">
                <option value="claude_code">Claude Code (default)</option>
                <option value="anthropic">Anthropic API</option>
                <option value="ollama">Ollama</option>
                <option value="gateway">Kong AI Gateway (OpenAI-compatible)</option>
              </select>
            </label>

            <!--
              Model + reasoning-effort. providerModel steers ONLY the claude_code
              /anthropic arms (gateway/ollama read their own connection-card
              model), so the picker renders for those two — for gateway/ollama we
              point at the connection card, now under Advanced.
            -->
            @switch (form.controls.providerId.value) {
              @case ("gateway") {
                <p class="field-help text-muted">
                  The model for Kong AI Gateway is set in its connection card
                  under Advanced.
                </p>
              }
              @case ("ollama") {
                <p class="field-help text-muted">
                  The model for Ollama is set in its connection card under
                  Advanced.
                </p>
              }
              @default {
                <div class="field">
                  <span class="field-label">Model</span>
                  <div class="model-row">
                    @if (defaultModelCatalog().length > 0) {
                      <select formControlName="providerModel" class="model-select">
                        <option value="">Default (provider's pick)</option>
                        @for (id of defaultModelCatalog(); track id) {
                          <option [value]="id">{{ id }}</option>
                        }
                        @if (defaultModelIsCustom()) {
                          <option [value]="form.controls.providerModel.value">
                            {{ form.controls.providerModel.value }} (custom)
                          </option>
                        }
                      </select>
                    } @else {
                      <input
                        formControlName="providerModel"
                        placeholder="Model id (blank = provider's pick)"
                        autocomplete="off"
                        spellcheck="false"
                        class="model-input"
                      />
                    }
                    <button
                      type="button"
                      class="btn btn-ghost model-refresh"
                      (click)="refreshDefaultModels()"
                      [disabled]="defaultModelsLoading()"
                      title="Fetch this provider's model list"
                    >
                      @if (defaultModelsLoading()) {
                        Loading…
                      } @else {
                        ↻ Refresh
                      }
                    </button>
                  </div>
                </div>
              }
            }

            @if (form.controls.providerId.value === "anthropic") {
              <label class="field">
                <span class="field-label">Reasoning effort</span>
                <select formControlName="providerEffort">
                  <option value="">Default</option>
                  <option value="low">Low</option>
                  <option value="medium">Medium</option>
                  <option value="high">High</option>
                </select>
              </label>
            }

            <div class="setup-foot">
              @if (providerIsCloud()) {
                <span class="pill">
                  <span class="pill-dot"></span>
                  Cloud · redacted first
                </span>
              } @else {
                <span class="pill is-success">
                  <span class="pill-dot"></span>
                  On this Mac · nothing leaves
                </span>
              }
              <button type="button" class="setup-link" (click)="expandAdvanced()">
                Set up &amp; test engines →
              </button>
            </div>
          </div>
        }

        @case ("reactions") {
          <div class="card setup-card">
            <div class="setup-head">
              <h3>On-device model</h3>
              <p class="text-secondary setup-sub">
                Realtime reactions run live on this Mac — nothing leaves.
              </p>
            </div>
            @for (l of reactionsLines(); track l.role) {
              <div class="od-line">
                <div class="od-info">
                  <span class="od-role">{{ l.role }}</span>
                  <span class="od-model text-muted">{{ l.model?.name ?? "—" }}</span>
                </div>
                @if (l.model; as m) {
                  @if (brainDownloadingId() === m.id) {
                    <span class="od-dl text-muted">Downloading… {{ brainPct() }}</span>
                  } @else if (m.downloaded) {
                    <span class="pill is-success">
                      <span class="pill-dot"></span>
                      Ready
                    </span>
                  } @else {
                    <button
                      type="button"
                      class="btn btn-primary btn-sm"
                      (click)="download(m.id)"
                      [disabled]="brainDownloadingId() !== null"
                    >
                      Download {{ sizeLabel(m.approxSizeBytes) }}
                    </button>
                  }
                }
              </div>
            }
            <button type="button" class="setup-link" (click)="expandAdvanced()">
              Customize models →
            </button>
          </div>
        }

        @case ("local") {
          <div class="card setup-card">
            <div class="setup-head">
              <h3>On-device model</h3>
              <p class="text-secondary setup-sub">
                Everything runs on this Mac — nothing leaves.
              </p>
            </div>
            @for (l of localLines(); track l.role) {
              <div class="od-line">
                <div class="od-info">
                  <span class="od-role">{{ l.role }}</span>
                  <span class="od-model text-muted">{{ l.model?.name ?? "—" }}</span>
                </div>
                @if (l.model; as m) {
                  @if (brainDownloadingId() === m.id) {
                    <span class="od-dl text-muted">Downloading… {{ brainPct() }}</span>
                  } @else if (m.downloaded) {
                    <span class="pill is-success">
                      <span class="pill-dot"></span>
                      Ready
                    </span>
                  } @else {
                    <button
                      type="button"
                      class="btn btn-primary btn-sm"
                      (click)="download(m.id)"
                      [disabled]="brainDownloadingId() !== null"
                    >
                      Download {{ sizeLabel(m.approxSizeBytes) }}
                    </button>
                  }
                }
              </div>
            }
            <button type="button" class="setup-link" (click)="expandAdvanced()">
              Customize models →
            </button>
          </div>
        }

        @case ("custom-note") {
          <p class="custom-note text-muted">
            Custom mix — you've routed features individually. See
            <strong>What runs where</strong> below, tune under <strong>Advanced</strong>.
          </p>
        }
      }
    }
  `,
  styles: [
    `
      :host {
        display: contents;
      }

      .setup-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .setup-head {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .setup-head h3 {
        margin: 0;
      }
      .setup-sub {
        margin: 0;
        font-size: 0.875rem;
        line-height: 1.55;
      }

      /* Stacked label + control (mirrors the other AI blocks). */
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
        margin: 0;
        font-size: 0.8125rem;
        line-height: 1.5;
      }

      /* Select-or-input + catalog refresh. */
      .model-row {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex-wrap: wrap;
      }
      .model-select,
      .model-input {
        flex: 1 1 220px;
        min-width: 0;
      }
      .model-refresh {
        flex: none;
        white-space: nowrap;
      }

      .setup-foot {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
      }
      .setup-link {
        background: none;
        border: none;
        padding: 0;
        cursor: pointer;
        color: var(--text-secondary);
        font: inherit;
        font-size: 0.85rem;
        font-weight: 550;
        transition: color var(--transition);
      }
      .setup-link:hover {
        color: var(--text-primary);
      }

      /* Compact on-device status line (full picker lives under Advanced). */
      .od-line {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
        padding: var(--space-2) 0;
        border-top: 1px solid var(--border-subtle);
      }
      .od-line:first-of-type {
        border-top: none;
      }
      .od-info {
        display: flex;
        flex-direction: column;
        gap: 2px;
        min-width: 0;
      }
      .od-role {
        font-size: 0.9rem;
        font-weight: 600;
        color: var(--text-primary);
      }
      .od-model {
        font-size: 0.8125rem;
      }
      .od-dl {
        font-size: 0.8125rem;
        white-space: nowrap;
      }
      .btn-sm {
        height: 32px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
        white-space: nowrap;
      }

      .custom-note {
        margin: 0 0 var(--space-4);
        font-size: 0.85rem;
        line-height: 1.55;
      }
      .custom-note strong {
        color: var(--text-secondary);
        font-weight: 600;
      }
    `,
  ],
})
export class AiSetupBlockComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;

  // ── posture / engine wires ────────────────────────────────────────────────
  readonly posture = this.store.posture;
  readonly providerIsCloud = this.store.providerIsCloud;
  readonly defaultModelCatalog = this.store.defaultModelCatalog;
  readonly defaultModelsLoading = this.store.defaultModelsLoading;
  readonly defaultModelIsCustom = this.store.defaultModelIsCustom;

  // ── on-device status wires (the full picker lives under Advanced) ────────────
  readonly brainDownloadingId = this.store.brainDownloadingId;
  readonly brainPct = this.store.brainPct;

  /**
   * Fully-local status lines — the EFFECTIVE on-device models (`selectedHeavy` /
   * `selectedLight` mirror the true `brain_heavy_model_id` / `brain_light_model_id`,
   * else the registry default). A compact read-out; the effort slider + language +
   * GGUF live under Advanced → On this Mac → Configure.
   */
  readonly localLines = computed(() => {
    const models = this.store.brainModels();
    return [
      { role: "Notes & Ask", model: models.find((m) => m.selectedHeavy) ?? null },
      {
        role: "Live reactions",
        model: models.find((m) => m.selectedLight) ?? null,
      },
    ];
  });

  /** Hybrid status — only the realtime-reactions (light) model runs on-device. */
  readonly reactionsLines = computed(() => [
    {
      role: "Live reactions",
      model: this.store.brainModels().find((m) => m.selectedLight) ?? null,
    },
  ]);

  /**
   * Which cards this posture shows, in order — the `@for`+`@switch` driver.
   * Cloud=engine; Hybrid=engine+reactions; Fully local=local; Custom=both+note.
   * Null (pre-load) → no cards (the map card below owns the loading state).
   */
  readonly setupCards = computed<readonly string[]>(() => {
    switch (this.posture()) {
      case "cloud":
        return ["engine"];
      case "hybrid":
        return ["engine", "reactions"];
      case "fully_local":
        return ["local"];
      case "custom":
        return ["engine", "local", "custom-note"];
      default:
        return [];
    }
  });

  /** Prefetch the newly-picked engine's model catalog (claude_code/anthropic). */
  onEngineChanged(e: Event): void {
    const id = (e.target as HTMLSelectElement).value;
    if (id === "claude_code" || id === "anthropic") {
      void this.store.ensureModels(id);
    }
  }

  /** Re-fetch the Default-model catalog for the current provider. */
  refreshDefaultModels(): void {
    void this.store.refreshModels(this.form.controls.providerId.value);
  }

  /** Open the Advanced disclosure (keys/URLs/Test + the on-device picker live there). */
  expandAdvanced(): void {
    this.store.expandAdvanced();
  }

  /** Download an on-device model that isn't present yet (posture usually pre-fetches it). */
  download(id: string): void {
    if (id) void this.store.downloadBrainModel(id);
  }

  /** Human "1.1 GB" / "620 MB" size label from a byte count (binary). */
  sizeLabel(bytes: number): string {
    const gb = 1024 * 1024 * 1024;
    return bytes >= gb
      ? (bytes / gb).toFixed(1) + " GB"
      : Math.round(bytes / (1024 * 1024)) + " MB";
  }
}
