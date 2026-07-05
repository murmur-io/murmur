import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../../settings.store";

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
  templateUrl: "./settings-audio-section.component.html",
  styleUrl: "./settings-audio-section.component.scss",
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
