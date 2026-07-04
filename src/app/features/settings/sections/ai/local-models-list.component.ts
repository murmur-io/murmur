import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { SettingsStore } from "../../settings.store";

/**
 * AI & Models → the shared "Local models" GGUF registry panel.
 *
 * Extracted from AiRoleRowsComponent (Task 3): rendered once inside `@if (anyLocal())`
 * in the role-rows block, shared by every feature role set to "Local model". Task 4
 * will re-mount this inside the forthcoming `<app-ai-advanced-block>`.
 *
 * Consumes brainModels / brainDownloadingId / brainDownloadFrac / brainPct /
 * brainModelsLoading / brainError / customGgufValue from SettingsStore, and
 * delegates downloads / selections / custom-GGUF writes back to it.
 */
@Component({
  selector: "app-local-models-list",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [],
  template: `
    <div class="brain-models">
      <div class="brain-models-head">
        <span class="brain-models-label text-muted">Local models</span>
        <button
          type="button"
          class="btn btn-sm"
          (click)="refreshBrainModels()"
          [disabled]="brainModelsLoading()"
        >
          {{ brainModelsLoading() ? "Loading…" : "Refresh" }}
        </button>
      </div>

      <p class="brain-note text-muted">
        Shared by every feature set to "Local model" — the selected
        model runs fully on this Mac.
      </p>

      @if (brainModels(); as models) {
        @if (models.length === 0 && !brainModelsLoading()) {
          <p class="brain-empty text-muted">
            No local models available.
          </p>
        } @else {
          <ul class="brain-model-list">
            @for (m of models; track m.id) {
              <li
                class="brain-model-row"
                [class.is-unfit]="!m.fitsRam"
                [class.is-selected]="m.selected"
              >
                <div class="brain-model-info">
                  <span class="brain-model-name">
                    {{ m.name }}
                    @if (m.selected) {
                      <span class="pill is-success brain-inline-pill">
                        <span class="pill-dot"></span>
                        In use
                      </span>
                    }
                  </span>
                  <span class="brain-model-meta text-muted">
                    {{ sizeLabel(m.approxSizeBytes) }} · {{ m.class }} ·
                    needs ≥{{ m.minRamGb }} GB RAM
                    @if (m.languages.length > 0) {
                      · {{ m.languages.join("/") }}
                    }
                  </span>
                  @if (!m.fitsRam) {
                    <span class="pill is-warning brain-fit-pill">
                      <span class="pill-dot"></span>
                      May not fit this Mac's RAM
                    </span>
                  }
                </div>

                <div class="brain-model-actions">
                  @if (brainDownloadingId() === m.id) {
                    <div class="brain-progress" role="status">
                      <div
                        class="brain-progress-track"
                        aria-hidden="true"
                      >
                        <div
                          class="brain-progress-fill"
                          [style.width.%]="brainDownloadFrac() * 100"
                        ></div>
                      </div>
                      <span class="brain-progress-label text-muted">
                        Downloading… {{ brainPct() }}
                      </span>
                    </div>
                  } @else if (m.downloaded) {
                    <button
                      type="button"
                      class="btn btn-sm"
                      (click)="useBrainModel(m.id)"
                      [disabled]="m.selected"
                    >
                      {{ m.selected ? "Selected" : "Use" }}
                    </button>
                  } @else {
                    <button
                      type="button"
                      class="btn btn-primary btn-sm"
                      (click)="downloadBrainModel(m.id)"
                      [disabled]="brainDownloadingId() !== null"
                    >
                      Download
                    </button>
                  }
                </div>
              </li>
            }
          </ul>
        }
      }

      <!--
        One shared input driving TWO mutually-exclusive controls
        (brainModelPath for a file path, brainModelId for a registry id),
        so it can't be a formControlName — it's a store-backed
        [value]/(input) pair. A registry pick above clears the path.
      -->
      <label class="field brain-custom">
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

      @if (brainError(); as berr) {
        <p class="text-danger brain-error">{{ berr }}</p>
      }
    </div>
  `,
  styles: [
    `
      :host {
        display: contents;
      }

      .brain-models {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        padding: var(--space-4);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
      }
      .brain-models-head {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
      }
      .brain-models-label {
        font-size: 0.8125rem;
        font-weight: 550;
        letter-spacing: 0.01em;
        text-transform: uppercase;
      }
      .brain-note {
        margin: 0;
        font-size: 0.8125rem;
        line-height: 1.5;
      }
      .brain-empty {
        margin: 0;
        font-size: 0.875rem;
      }
      .brain-model-list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .brain-model-row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
        padding: var(--space-3);
        border-radius: var(--radius-md);
        background: var(--surface-raised);
        border: 1px solid var(--glass-border);
      }
      .brain-model-row.is-selected {
        border-color: var(--accent-hover);
      }
      .brain-model-row.is-unfit {
        opacity: 0.78;
      }
      .brain-model-info {
        display: flex;
        flex-direction: column;
        gap: 3px;
        min-width: 0;
      }
      .brain-model-name {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        color: var(--text-primary);
        font-weight: 550;
        font-size: 0.9rem;
        flex-wrap: wrap;
      }
      .brain-model-meta {
        font-size: 0.8125rem;
      }
      .brain-inline-pill,
      .brain-fit-pill {
        align-self: flex-start;
      }
      .brain-fit-pill {
        margin-top: 2px;
      }
      .brain-model-actions {
        flex: none;
        display: flex;
        align-items: center;
        gap: var(--space-2);
      }
      .brain-progress {
        display: flex;
        flex-direction: column;
        gap: 3px;
        min-width: 120px;
      }
      .brain-progress-track {
        height: 6px;
        border-radius: 3px;
        background: var(--surface-input);
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
      .brain-custom {
        margin-top: var(--space-1);
      }
      .brain-error {
        margin: 0;
        font-size: 0.85rem;
      }
      .btn-sm {
        height: 32px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
      }
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
    `,
  ],
})
export class LocalModelsListComponent {
  private readonly store = inject(SettingsStore);

  readonly brainModels = this.store.brainModels;
  readonly brainModelsLoading = this.store.brainModelsLoading;
  readonly brainError = this.store.brainError;
  readonly brainDownloadingId = this.store.brainDownloadingId;
  readonly brainDownloadFrac = this.store.brainDownloadFrac;
  readonly brainPct = this.store.brainPct;
  readonly customGgufValue = this.store.customGgufValue;

  refreshBrainModels(): void {
    void this.store.refreshBrainModels();
  }

  useBrainModel(id: string): void {
    void this.store.useBrainModel(id);
  }

  downloadBrainModel(id: string): void {
    void this.store.downloadBrainModel(id);
  }

  setCustomGguf(v: string): void {
    this.store.setCustomGguf(v);
  }

  /** Human GB/MB size label from a byte count (binary), mirroring the Storage section. */
  sizeLabel(bytes: number): string {
    const gb = 1024 * 1024 * 1024;
    if (bytes >= gb) return (bytes / gb).toFixed(1) + " GB";
    return Math.round(bytes / (1024 * 1024)) + " MB";
  }
}
