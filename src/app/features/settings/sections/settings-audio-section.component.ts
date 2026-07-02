import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../settings.store";

/**
 * Settings → audio section (Stage-1 split): the `@case ("audio")` block of the
 * former settings.component.ts monolith, moved VERBATIM. State/actions live in
 * the shell-provided SettingsStore so section switches never drop them.
 */
@Component({
  selector: "app-settings-audio-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  template: `
    <div class="section-stack" [formGroup]="form">
              <!-- Microphone input device -->
              <div class="card">
                <label class="field">
                  <span class="field-label">Microphone</span>
                  <select formControlName="inputDevice">
                    <option value="">System default</option>
                    @for (dev of inputDevices(); track dev.name) {
                      <option [value]="dev.name">
                        {{ dev.name }}{{ dev.isDefault ? " (default)" : "" }}
                      </option>
                    }
                  </select>
                  <span class="field-help text-muted">
                    Which microphone to record. “System default” follows your macOS input
                    selection; a chosen device falls back to the default if it’s unplugged.
                  </span>
                </label>
              </div>

              <!-- Capture system audio — toggle row -->
              <div class="card">
                <label class="toggle-row">
                  <span class="toggle-copy">
                    <span class="toggle-title">Capture system audio</span>
                    <span class="text-secondary toggle-sub">
                      Records the other side of the call — needs the Screen Recording
                      permission on first use.
                    </span>
                  </span>
                  <input type="checkbox" formControlName="captureSystemAudio" />
                </label>
              </div>

              <!-- Smart transcription (VAD) — toggle row -->
              <div class="card">
                <label class="toggle-row">
                  <span class="toggle-copy">
                    <span class="toggle-title">Smart speech detection</span>
                    <span class="text-secondary toggle-sub">
                      Skips silence and resets context between pauses for cleaner, faster
                      transcripts (voice-activity detection). Recommended.
                    </span>
                  </span>
                  <input type="checkbox" formControlName="vadEnabled" />
                </label>
              </div>

              <!-- High-fidelity masters — toggle row -->
              <div class="card">
                <label class="toggle-row">
                  <span class="toggle-copy">
                    <span class="toggle-title">Keep high-fidelity masters</span>
                    <span class="text-secondary toggle-sub">
                      Archive faithful per-stream float32 recordings (mic + system)
                      alongside the standard mix. Best quality; roughly doubles audio disk
                      use per meeting.
                    </span>
                  </span>
                  <input type="checkbox" formControlName="keepHiresMasters" />
                </label>
              </div>

              <!-- Speaker diarization — toggle row -->
              <div class="card">
                <label class="toggle-row">
                  <span class="toggle-copy">
                    <span class="toggle-title">Identify remote speakers</span>
                    <span class="text-secondary toggle-sub">
                      Label individual people on the other side of the call (Speaker
                      1/2/3) instead of one “Others”. Needs system-audio capture;
                      downloads ~40 MB of models on first use.
                    </span>
                  </span>
                  <input type="checkbox" formControlName="diarizeOthers" />
                </label>
              </div>

              <!-- On-device echo removal (post-processing) — toggle row -->
              <div class="card">
                <label class="toggle-row">
                  <span class="toggle-copy">
                    <span class="toggle-title">Remove speaker echo from recordings</span>
                    <span class="text-secondary toggle-sub">
                      After each recording, cancel the other participants' voices out of your
                      microphone track using the captured system audio — fixes the doubled
                      voice when recording on speakers. Runs fully on-device.
                    </span>
                  </span>
                  <input type="checkbox" formControlName="postAecEnabled" />
                </label>
              </div>

              <!-- Echo cancellation (experimental) — toggle row -->
              <div class="card">
                <label class="toggle-row">
                  <span class="toggle-copy">
                    <span class="toggle-title">Cancel speaker echo (experimental)</span>
                    <span class="text-secondary toggle-sub">
                      Experimental Apple voice processing on the transcription mic. May not
                      remove echo on all setups (macOS gives it no reference signal) — echoed
                      lines are also removed automatically after each recording. Headphones
                      remain the most reliable fix.
                    </span>
                  </span>
                  <input type="checkbox" formControlName="aecEnabled" />
                </label>
              </div>

              <!-- Voice trigger — toggle row -->
              <div class="card">
                <label class="toggle-row">
                  <span class="toggle-copy">
                    <span class="toggle-title">Voice trigger</span>
                    <span class="text-secondary toggle-sub">
                      Start recording hands-free when you say “start recording”. Listens
                      with your Whisper model while idle.
                    </span>
                  </span>
                  <input type="checkbox" formControlName="voiceTrigger" />
                </label>
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
    `,
  ],
})
export class SettingsAudioSectionComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;
  readonly inputDevices = this.store.inputDevices;
}
