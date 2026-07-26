import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { toSignal } from "@angular/core/rxjs-interop";
import { startWith } from "rxjs";
import { SettingsStore } from "../../settings.store";
import { MurProgressComponent } from "../../../../design-system/progress/progress.component";
import { MurSelectComponent } from "../../../../design-system/select/select.component";
import { ModelPowerComponent } from "./model-power/model-power.component";

/**
 * Settings → transcription section. The quality `<select>` (nine hand-written
 * `<option>` labels carrying their own size figures) is now `<app-model-power>` —
 * the SAME picker the onboarding wizard hosts, driven here from the reactive form.
 */
@Component({
  selector: "app-settings-transcription-section",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    ReactiveFormsModule,
    MurProgressComponent,
    MurSelectComponent,
    ModelPowerComponent,
  ],
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

  /**
   * The picker is FULLY CONTROLLED, so it needs the form's current value as a
   * signal. `valueChanges` is the store's own idiom for this (`_gatewayModelValue`);
   * a plain field would never re-render under zoneless change detection.
   */
  readonly modelSize = toSignal(
    this.form.controls.modelSize.valueChanges.pipe(
      startWith(this.form.controls.modelSize.value),
    ),
    { initialValue: "" },
  );

  // OPTIONAL parakeet live-ASR engine (off-GPU live captions).
  readonly parakeetPresent = this.store.parakeetPresent;
  readonly downloadingParakeet = this.store.downloadingParakeet;
  readonly parakeetDownloadFrac = this.store.parakeetDownloadFrac;
  readonly parakeetPct = this.store.parakeetPct;
  readonly parakeetDownloadError = this.store.parakeetDownloadError;

  /**
   * Write the picked size into the form. The debounced auto-save persists it and
   * then refreshes the catalog — no direct save() here (a direct call produced a
   * double save; adversarial-verify finding).
   */
  onSizeChange(size: string): void {
    // A pick HERE is deliberate, so the next save records `modelSizeSource: "user"`
    // — the counterpart to the onboarding wizard's `"auto"` preselect.
    this.store.markModelSizeUserPick();
    this.form.controls.modelSize.setValue(size);
  }

  downloadModel(): void {
    void this.store.downloadModel();
  }

  cancelDownload(): void {
    void this.store.cancelModelDownload();
  }

  downloadParakeet(): void {
    void this.store.downloadParakeet();
  }
}
