import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
} from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import type { BrainModelDto, ModelClass } from "../../../../core/models";
import { SettingsStore } from "../../settings.store";

/**
 * One on-device model picker's derived view — everything the template needs to
 * render a class's `<select>` + its download/ready/progress state + RAM nudge,
 * computed once from the store's brain-model signals. Shared by the Hybrid
 * "realtime reactions" light card and the Fully-local heavy+light pickers so the
 * two never duplicate the resolution logic (spec §"On-device model picker").
 */
interface PickerVm {
  readonly cls: ModelClass;
  /** The model id the select currently reflects (selected, else auto-picked). */
  readonly selectedId: string;
  readonly selected: BrainModelDto | null;
  readonly options: readonly { readonly id: string; readonly label: string }[];
  /** This picker's chosen model is the one currently downloading. */
  readonly downloading: boolean;
  /** Chosen model is on disk (show a "Ready" pill). */
  readonly ready: boolean;
  /** Chosen model is not on disk yet (show a Download button). */
  readonly needsDownload: boolean;
  /** Warn: the chosen model may be tight on this Mac's RAM. */
  readonly ramWarn: boolean;
  readonly ramGb: number;
  readonly name: string;
}

/**
 * AI & Models → "Your setup" (NEW, posture-adaptive) — the second section of
 * the posture-driven redesign (docs/superpowers/specs/2026-07-05-…). Posture
 * picks the lane; this block shows ONLY what that lane needs to configure:
 *
 *   - Cloud            → the Default AI engine card.
 *   - Hybrid           → engine card + an on-device light-model card (reactions).
 *   - Fully local      → an on-device models card (heavy + light pickers + GGUF).
 *   - Custom           → engine card + on-device models card + a "Custom mix" note.
 *
 * The Default-engine + Default-model controls are the SAME `providerId` /
 * `providerModel` / `providerEffort` FormControls the old Advanced → Default
 * engine block owned (moved here verbatim, one source of truth — no new second
 * writer of any config key). The on-device pickers drive the existing store
 * `useBrainModel` / `downloadBrainModel` actions. Backend untouched.
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
              <h3>On-device model — realtime reactions</h3>
              <p class="text-secondary setup-sub">
                A small model runs live during meetings for instant reactions &amp;
                fact-spotting. Stays on this Mac.
              </p>
            </div>
            @if (lightPicker(); as vm) {
              <div class="inline-row">
                <select
                  class="picker-select"
                  [value]="vm.selectedId"
                  (change)="pick($any($event.target).value)"
                >
                  @for (o of vm.options; track o.id) {
                    <option [value]="o.id">{{ o.label }}</option>
                  }
                </select>
                @if (vm.downloading) {
                  <div class="brain-progress" role="status">
                    <div class="brain-progress-track" aria-hidden="true">
                      <div
                        class="brain-progress-fill"
                        [style.width.%]="brainDownloadFrac() * 100"
                      ></div>
                    </div>
                    <span class="brain-progress-label text-muted">
                      {{ brainPct() }}
                    </span>
                  </div>
                } @else if (vm.needsDownload) {
                  <button
                    type="button"
                    class="btn btn-primary btn-sm"
                    (click)="download(vm.selectedId)"
                    [disabled]="brainDownloadingId() !== null"
                  >
                    Download
                  </button>
                } @else if (vm.ready) {
                  <span class="pill is-success">
                    <span class="pill-dot"></span>
                    Ready
                  </span>
                }
              </div>
              @if (vm.ramWarn) {
                <span class="ram-warn">
                  ⚠ {{ vm.name }} needs ≥{{ vm.ramGb }} GB RAM — may be slow
                  alongside recording on a smaller Mac.
                </span>
              }
            }
          </div>
        }

        @case ("local") {
          <div class="card setup-card">
            <div class="setup-head">
              <h3>Your on-device models</h3>
              <p class="text-secondary setup-sub">
                Everything runs here. Pick the models Murmur uses — bigger = better
                notes but slower &amp; more RAM.
              </p>
            </div>

            @if (heavyPicker(); as vm) {
              <div class="field">
                <span class="field-label">
                  Notes &amp; Ask
                  <span class="text-muted picker-sub">
                    — heavy model, runs after the meeting
                  </span>
                </span>
                <div class="inline-row">
                  <select
                    class="picker-select"
                    [value]="vm.selectedId"
                    (change)="pick($any($event.target).value)"
                  >
                    @for (o of vm.options; track o.id) {
                      <option [value]="o.id">{{ o.label }}</option>
                    }
                  </select>
                  @if (vm.downloading) {
                    <div class="brain-progress" role="status">
                      <div class="brain-progress-track" aria-hidden="true">
                        <div
                          class="brain-progress-fill"
                          [style.width.%]="brainDownloadFrac() * 100"
                        ></div>
                      </div>
                      <span class="brain-progress-label text-muted">
                        {{ brainPct() }}
                      </span>
                    </div>
                  } @else if (vm.needsDownload) {
                    <button
                      type="button"
                      class="btn btn-primary btn-sm"
                      (click)="download(vm.selectedId)"
                      [disabled]="brainDownloadingId() !== null"
                    >
                      Download
                    </button>
                  } @else if (vm.ready) {
                    <span class="pill is-success">
                      <span class="pill-dot"></span>
                      Ready
                    </span>
                  }
                </div>
                @if (vm.ramWarn) {
                  <span class="ram-warn">
                    ⚠ {{ vm.name }} needs ≥{{ vm.ramGb }} GB RAM — may be slow
                    alongside recording on a smaller Mac.
                  </span>
                }
              </div>
            }

            @if (lightPicker(); as vm) {
              <div class="field">
                <span class="field-label">
                  Live &amp; realtime reactions
                  <span class="text-muted picker-sub">
                    — light model, runs during the meeting
                  </span>
                </span>
                <div class="inline-row">
                  <select
                    class="picker-select"
                    [value]="vm.selectedId"
                    (change)="pick($any($event.target).value)"
                  >
                    @for (o of vm.options; track o.id) {
                      <option [value]="o.id">{{ o.label }}</option>
                    }
                  </select>
                  @if (vm.downloading) {
                    <div class="brain-progress" role="status">
                      <div class="brain-progress-track" aria-hidden="true">
                        <div
                          class="brain-progress-fill"
                          [style.width.%]="brainDownloadFrac() * 100"
                        ></div>
                      </div>
                      <span class="brain-progress-label text-muted">
                        {{ brainPct() }}
                      </span>
                    </div>
                  } @else if (vm.needsDownload) {
                    <button
                      type="button"
                      class="btn btn-primary btn-sm"
                      (click)="download(vm.selectedId)"
                      [disabled]="brainDownloadingId() !== null"
                    >
                      Download
                    </button>
                  } @else if (vm.ready) {
                    <span class="pill is-success">
                      <span class="pill-dot"></span>
                      Ready
                    </span>
                  }
                </div>
                @if (vm.ramWarn) {
                  <span class="ram-warn">
                    ⚠ {{ vm.name }} needs ≥{{ vm.ramGb }} GB RAM — may be slow
                    alongside recording on a smaller Mac.
                  </span>
                }
              </div>
            }

            @if (posture() === "fully_local") {
              <label class="field">
                <span class="field-label">Custom GGUF model</span>
                <input
                  [value]="customGgufValue()"
                  (input)="setCustomGguf($any($event.target).value)"
                  placeholder="/path/to/model.gguf or a registry id"
                  autocomplete="off"
                  spellcheck="false"
                />
                <span class="field-help text-muted">
                  Point at your own .gguf file, or type a registry id. Saved with
                  your settings.
                </span>
              </label>
            }

            @if (brainError(); as berr) {
              <p class="text-danger setup-error">{{ berr }}</p>
            }
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
      .picker-sub {
        font-weight: 400;
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

      /* On-device picker row: select + state. */
      .inline-row {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex-wrap: wrap;
      }
      .picker-select {
        flex: 1 1 240px;
        min-width: 0;
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

      .ram-warn {
        font-size: 0.8125rem;
        line-height: 1.5;
        color: var(--warning);
      }
      .setup-error {
        margin: 0;
        font-size: 0.85rem;
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

      /* Download progress (reused shape from local-models-list). */
      .brain-progress {
        display: flex;
        flex-direction: column;
        gap: 3px;
        min-width: 120px;
      }
      .brain-progress-track {
        height: 6px;
        border-radius: 3px;
        background: var(--surface-raised);
        overflow: hidden;
      }
      .brain-progress-fill {
        height: 100%;
        background: var(--accent);
        border-radius: 3px;
        transition: width var(--transition);
      }
      .brain-progress-label {
        font-size: 0.75rem;
      }

      .btn-sm {
        height: 32px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
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

  // ── on-device model wires ─────────────────────────────────────────────────
  readonly brainDownloadingId = this.store.brainDownloadingId;
  readonly brainDownloadFrac = this.store.brainDownloadFrac;
  readonly brainPct = this.store.brainPct;
  readonly brainError = this.store.brainError;
  readonly customGgufValue = this.store.customGgufValue;

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

  /** The heavy-class on-device picker (Notes & Ask). */
  readonly heavyPicker = computed(() => this.buildPicker("heavy"));
  /** The light-class on-device picker (Live & realtime reactions). */
  readonly lightPicker = computed(() => this.buildPicker("light"));

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

  /** Open the Advanced disclosure (keys/URLs/Test live there). */
  expandAdvanced(): void {
    this.store.expandAdvanced();
  }

  /** Make a picker's chosen model the active on-device model. */
  pick(id: string): void {
    if (id) void this.store.useBrainModel(id);
  }

  /** Download a picker's chosen (not-yet-present) model. */
  download(id: string): void {
    if (id) void this.store.downloadBrainModel(id);
  }

  /** Route a typed custom GGUF value to the right store control. */
  setCustomGguf(v: string): void {
    this.store.setCustomGguf(v);
  }

  /**
   * Build a class's picker view from the store's brain-model signals. The select
   * reflects the SELECTED model of that class, else the auto-pick (smallest that
   * fits RAM), else the first available — so it always shows the model that would
   * actually run. Runs inside a `computed`, so reads are tracked.
   */
  private buildPicker(cls: ModelClass): PickerVm {
    const models = this.store.brainModels().filter((m) => m.class === cls);
    const selectedId =
      models.find((m) => m.selected)?.id ??
      this.store.autoPickForClass(cls)?.id ??
      models[0]?.id ??
      "";
    const selected = models.find((m) => m.id === selectedId) ?? null;
    const options = models.map((m) => {
      const langs = m.languages.length ? m.languages.join("/") : "";
      const parts = [m.name, langs, this.sizeLabel(m.approxSizeBytes)].filter(
        (p) => p.length > 0,
      );
      let label = parts.join(" · ");
      if (!m.downloaded) label += " · ⬇ download";
      return { id: m.id, label };
    });
    return {
      cls,
      selectedId,
      selected,
      options,
      downloading: !!selectedId && this.store.brainDownloadingId() === selectedId,
      ready: !!selected?.downloaded,
      needsDownload: !!selected && !selected.downloaded,
      // Warn when it won't fit, or it's a large heavy model (RAM-hungry alongside
      // recording) — data-driven (minRamGb), no hardcoded ids.
      ramWarn:
        !!selected &&
        (!selected.fitsRam || (cls === "heavy" && selected.minRamGb >= 10)),
      ramGb: selected?.minRamGb ?? 0,
      name: selected?.name ?? "",
    };
  }

  /** Friendly "~1.1 GB" / "~620 MB" size label from a byte count. */
  private sizeLabel(bytes: number): string {
    const gb = 1024 * 1024 * 1024;
    return bytes >= gb
      ? (bytes / gb).toFixed(1) + " GB"
      : Math.round(bytes / (1024 * 1024)) + " MB";
  }
}
