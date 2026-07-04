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
                      Capture the other side of a Zoom / Meet / Teams call, not just
                      your mic — needs the Screen Recording permission on first use.
                    </span>
                  </span>
                  <input class="switch" type="checkbox" formControlName="captureSystemAudio" />
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
                  <input class="switch" type="checkbox" formControlName="vadEnabled" />
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
                  <input class="switch" type="checkbox" formControlName="keepHiresMasters" />
                </label>
              </div>

              <!-- Speaker diarization — toggle row -->
              <div class="card">
                <label class="toggle-row">
                  <span class="toggle-copy">
                    <span class="toggle-title">Split “Others” into individual speakers</span>
                    <span class="text-secondary toggle-sub">
                      Label individual people on the other side of the call (Speaker
                      1/2/3) instead of one “Others”. Experimental — verify the
                      quality on your calls. Needs system-audio capture; downloads
                      ~40 MB of models on first use.
                    </span>
                  </span>
                  <input class="switch" type="checkbox" formControlName="diarizeOthers" />
                </label>
              </div>

              <!-- Speaker voiceprints — cross-meeting re-identification (opt-in). -->
              <div class="card">
                <label class="toggle-row">
                  <span class="toggle-copy">
                    <span class="toggle-title">Recognise speakers across meetings</span>
                    <span class="text-secondary toggle-sub">
                      Recognise the same speaker across meetings, fully on-device.
                      Experimental — accuracy unverified; captures a voice fingerprint
                      of remote participants (opt-in). Needs “Split Others into
                      individual speakers” above.
                    </span>
                  </span>
                  <input class="switch" type="checkbox" formControlName="voiceprintEnabled" />
                </label>

                <!-- Management list — the captured voiceprints, each forgettable. -->
                @if (voiceprints().length) {
                  <div class="vp-list">
                    <div class="vp-list-head">
                      <span class="vp-list-title text-secondary">
                        {{ voiceprints().length }} stored
                        {{ voiceprints().length === 1 ? "voiceprint" : "voiceprints" }}
                      </span>
                      <button
                        type="button"
                        class="btn btn-ghost vp-clear"
                        [disabled]="voiceprintBusy()"
                        (click)="clearVoiceprints()"
                      >
                        Forget all
                      </button>
                    </div>
                    @for (vp of voiceprints(); track vp.id) {
                      <div class="vp-row">
                        <span class="vp-name">
                          {{ vp.label || "Speaker " + vp.clusterIndex }}
                        </span>
                        <button
                          type="button"
                          class="vp-forget"
                          [disabled]="voiceprintBusy()"
                          [attr.aria-label]="
                            'Forget voiceprint ' + (vp.label || vp.clusterIndex)
                          "
                          (click)="forgetVoiceprint(vp.id)"
                        >
                          Forget
                        </button>
                      </div>
                    }
                  </div>
                }
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
                  <input class="switch" type="checkbox" formControlName="postAecEnabled" />
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
                  <input class="switch" type="checkbox" formControlName="voiceTrigger" />
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

      /* --- Speaker-voiceprint management list (under the toggle) --- */
      .vp-list {
        margin-top: var(--space-4);
        padding-top: var(--space-3);
        border-top: 1px solid var(--border);
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .vp-list-head {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
      }
      .vp-list-title {
        font-size: 0.8125rem;
      }
      .vp-clear {
        padding: var(--space-1) var(--space-2);
        font-size: 0.8125rem;
      }
      .vp-row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
      }
      .vp-name {
        color: var(--text-primary);
        font-size: 0.9rem;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .vp-forget {
        flex-shrink: 0;
        background: none;
        border: none;
        color: var(--text-secondary);
        font-size: 0.8125rem;
        cursor: pointer;
        padding: var(--space-1) var(--space-2);
        border-radius: var(--radius-sm);
        transition: color var(--transition-fast), background var(--transition-fast);
      }
      .vp-forget:hover:not(:disabled) {
        color: var(--live);
        background: var(--surface-raised);
      }
      .vp-forget:disabled {
        opacity: 0.5;
        cursor: default;
      }
    `,
  ],
})
export class SettingsAudioSectionComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;
  readonly inputDevices = this.store.inputDevices;
  readonly voiceprints = this.store.voiceprints;
  readonly voiceprintBusy = this.store.voiceprintBusy;

  forgetVoiceprint(id: string): void {
    void this.store.forgetVoiceprint(id);
  }

  clearVoiceprints(): void {
    void this.store.clearVoiceprints();
  }
}
