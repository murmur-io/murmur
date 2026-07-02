import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../settings.store";

/**
 * Settings → general section (Stage-1 split): the `@case ("general")` block of the
 * former settings.component.ts monolith, moved VERBATIM. State/actions live in
 * the shell-provided SettingsStore so section switches never drop them.
 */
@Component({
  selector: "app-settings-general-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  template: `
    <div class="section-stack" [formGroup]="form">
              <div class="card">
                <fieldset>
                  <legend>General</legend>

                  <label class="field">
                    <span class="field-label">Provider</span>
                    <select formControlName="providerId">
                      <option value="claude_code">Claude Code (default)</option>
                      <option value="anthropic">Anthropic API</option>
                      <option value="ollama">Ollama</option>
                      <option value="gateway">AI Gateway (OpenAI-compatible)</option>
                    </select>
                  </label>

                  <label class="field">
                    <span class="field-label">Vault folder</span>
                    <span class="row">
                      <input formControlName="vaultPath" placeholder="/path/to/vault" />
                      <button type="button" class="btn" (click)="pickVault()">
                        Browse…
                      </button>
                    </span>
                  </label>

                  <label class="field">
                    <span class="field-label">Vault subfolder</span>
                    <input formControlName="vaultSubfolder" placeholder="Meetings" />
                  </label>

                  <label class="field">
                    <span class="field-label"
                      >Whisper model path (optional override)</span
                    >
                    <span class="row">
                      <input
                        formControlName="whisperModelPath"
                        placeholder="leave blank — auto-managed by the model chosen in Settings → Transcription"
                      />
                      <button type="button" class="btn" (click)="pickModel()">
                        Browse…
                      </button>
                    </span>
                  </label>
                </fieldset>
              </div>

              <div class="card setup-card">
                <div class="setup-copy">
                  <span class="setup-title">First-run setup</span>
                  <span class="text-secondary setup-sub">
                    Re-open the guided wizard. Your existing settings are preserved
                    and prefilled.
                  </span>
                </div>
                <button
                  type="button"
                  class="btn btn-ghost"
                  (click)="rerunOnboarding()"
                >
                  Run setup again
                </button>
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

      /* --- General: re-run setup call-out --- */
      .setup-card {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-4);
        flex-wrap: wrap;
      }
      .setup-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        min-width: 0;
      }
      .setup-title {
        color: var(--text-primary);
        font-size: 0.95rem;
        font-weight: 550;
      }
      .setup-sub {
        font-size: 0.85rem;
        line-height: 1.5;
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
    `,
  ],
})
export class SettingsGeneralSectionComponent {
  private readonly store = inject(SettingsStore);

  /** The shared settings form — single source of truth, owned by the store. */
  readonly form = this.store.form;

  pickVault(): void {
    void this.store.pickVault();
  }

  pickModel(): void {
    void this.store.pickModel();
  }

  rerunOnboarding(): void {
    this.store.rerunOnboarding();
  }
}
