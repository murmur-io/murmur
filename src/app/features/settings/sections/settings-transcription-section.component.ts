import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../settings.store";

/**
 * Settings → transcription section (Stage-1 split): the `@case ("transcription")` block of the
 * former settings.component.ts monolith, moved VERBATIM. State/actions live in
 * the shell-provided SettingsStore so section switches never drop them.
 */
@Component({
  selector: "app-settings-transcription-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  template: `
    <div class="section-stack" [formGroup]="form">
              <div class="card model-card">
                <div class="model-copy">
                  <h3>Transcription model</h3>
                  <p class="text-secondary model-sub">
                    Runs entirely on-device. Pick your language and quality — the
                    matching Whisper model is fetched once and reused for every
                    recording.
                  </p>
                </div>

                <div class="model-grid">
                  <label class="field">
                    <span class="field-label">Language</span>
                    <select formControlName="language" (change)="onModelChoiceChange()">
                      <option value="">Auto-detect</option>
                      <option value="pl">Polski</option>
                      <option value="en">English</option>
                      <option value="de">Deutsch</option>
                      <option value="es">Español</option>
                      <option value="fr">Français</option>
                      <option value="it">Italiano</option>
                      <option value="pt">Português</option>
                      <option value="uk">Українська</option>
                      <option value="nl">Nederlands</option>
                    </select>
                    <span class="field-help text-muted">
                      Force the transcription language. Polish recommended if you record
                      mostly in Polish (auto-detect can misfire on short clips).
                    </span>
                  </label>

                  <label class="field">
                    <span class="field-label">Quality</span>
                    <select
                      formControlName="modelSize"
                      (change)="onModelChoiceChange()"
                    >
                      <option value="tiny">Tiny — fastest (~75 MB)</option>
                      <option value="base">Base (~150 MB)</option>
                      <option value="small">Small (~470 MB)</option>
                      <option value="medium">Medium (~1.5 GB)</option>
                      <option value="large-v3-turbo">
                        Large v3 Turbo — fast &amp; accurate (~1.6 GB)
                      </option>
                      <option value="large-v3">
                        Large v3 — best accuracy, recommended (~3 GB)
                      </option>
                    </select>
                    <span class="field-help text-muted">
                      Large v3 is the most accurate and the default — it’s a one-time ~3
                      GB download. Turbo is nearly as good and much smaller.
                    </span>
                  </label>
                </div>

                <div class="model-status-row">
                  @if (modelPresent() === true) {
                    <span class="pill is-success">
                      <span class="pill-dot"></span>
                      Downloaded ✓
                    </span>
                    <span class="text-muted model-note">
                      Stored on this Mac — used for every recording.
                    </span>
                  } @else if (modelPresent() === false) {
                    @if (downloadingModel()) {
                      <div class="brain-progress" role="status">
                        <div class="brain-progress-track" aria-hidden="true">
                          <div
                            class="brain-progress-fill"
                            [style.width.%]="modelDownloadFrac() * 100"
                          ></div>
                        </div>
                        <span class="brain-progress-label text-muted">
                          @if (modelDownloadFrac() > 0) {
                            Downloading… {{ modelPct() }}
                          } @else {
                            Downloading…
                          }
                        </span>
                      </div>
                      <span class="text-muted model-note">
                        Fetching the model — large models can take a few minutes.
                      </span>
                    } @else {
                      <button
                        type="button"
                        class="btn btn-primary"
                        (click)="downloadModel()"
                      >
                        Download ({{ downloadHint() }})
                      </button>
                      <span class="text-muted model-note">
                        {{ downloadHint() }}, one time, on-device.
                      </span>
                    }
                  } @else {
                    <span class="pill">
                      <span class="pill-dot"></span>
                      Checking…
                    </span>
                  }
                </div>
                @if (modelDownloadError(); as derr) {
                  <p class="model-error text-danger">{{ derr }}</p>
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

      /* --- Whisper model status card --- */
      .model-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .model-copy h3 {
        margin: 0;
      }
      .model-sub {
        margin: 0;
        font-size: 0.875rem;
      }
      .model-error {
        margin: var(--space-3) 0 0;
        font-size: 0.85rem;
      }
      .model-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .model-grid {
        display: grid;
        grid-template-columns: 1fr 1fr;
        gap: var(--space-4);
      }
      @media (max-width: 520px) {
        .model-grid {
          grid-template-columns: 1fr;
        }
      }
      .model-status-row {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
        min-height: 40px;
      }
      .model-note {
        font-size: 0.85rem;
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
    `,
  ],
})
export class SettingsTranscriptionSectionComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;
  readonly modelPresent = this.store.modelPresent;
  readonly downloadingModel = this.store.downloadingModel;
  readonly modelDownloadFrac = this.store.modelDownloadFrac;
  readonly modelPct = this.store.modelPct;
  readonly modelDownloadError = this.store.modelDownloadError;
  readonly downloadHint = this.store.downloadHint;

  onModelChoiceChange(): void {
    void this.store.onModelChoiceChange();
  }

  downloadModel(): void {
    void this.store.downloadModel();
  }
}
