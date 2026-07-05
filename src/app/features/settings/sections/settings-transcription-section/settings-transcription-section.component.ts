import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../../settings.store";

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
  templateUrl: "./settings-transcription-section.component.html",
  styleUrl: "./settings-transcription-section.component.scss",
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
    this.store.onModelChoiceChange();
  }

  downloadModel(): void {
    void this.store.downloadModel();
  }
}
