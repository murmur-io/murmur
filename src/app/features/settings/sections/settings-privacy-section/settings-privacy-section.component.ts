import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { MurToggleComponent } from "../../../../design-system/toggle/toggle.component";
import { MurProgressComponent } from "../../../../design-system/progress/progress.component";
import { RouterLink } from "@angular/router";
import { SettingsStore } from "../../settings.store";

/**
 * Settings → privacy section (Stage-1 split): the `@case ("privacy")` block of the
 * former settings.component.ts monolith, moved VERBATIM. State/actions live in
 * the shell-provided SettingsStore so section switches never drop them.
 */
@Component({
  selector: "app-settings-privacy-section",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    MurToggleComponent,
    MurProgressComponent,
    ReactiveFormsModule,
    RouterLink,
  ],
  templateUrl: "./settings-privacy-section.component.html",
  styleUrl: "./settings-privacy-section.component.scss",
})
export class SettingsPrivacySectionComponent {
  private readonly store = inject(SettingsStore);

  /** The shared settings form — the `userMemoryEnabled` toggle round-trips here. */
  readonly form = this.store.form;

  readonly cloudConsented = this.store.cloudConsented;
  readonly consenting = this.store.consenting;
  readonly consentError = this.store.consentError;
  readonly configCopied = this.store.configCopied;
  readonly mcpUrl = this.store.mcpUrl;
  readonly mcpConfig = this.store.mcpConfig;

  // Phase D — on-device name-redaction (NER) model affordance.
  readonly nerModelPresent = this.store.nerModelPresent;
  readonly downloadingNerModel = this.store.downloadingNerModel;
  readonly nerDownloadFrac = this.store.nerDownloadFrac;
  readonly nerPct = this.store.nerPct;
  readonly nerDownloadError = this.store.nerDownloadError;

  allowCloudProcessing(): void {
    void this.store.allowCloudProcessing();
  }

  downloadNerModel(): void {
    void this.store.downloadNerModel();
  }

  copyMcpConfig(): void {
    void this.store.copyMcpConfig();
  }
}
