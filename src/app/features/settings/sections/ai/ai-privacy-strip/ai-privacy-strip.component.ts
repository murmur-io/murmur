import {
  ChangeDetectionStrategy,
  Component,
  inject,
  signal,
} from "@angular/core";
import { SettingsStore } from "../../../settings.store";

/**
 * AI & Models → Block C: WHERE YOUR TEXT GOES. A two-line privacy strip
 * (what always stays on-device; where the default connection sends redacted
 * text — hidden entirely when the default is a local Ollama), plus the
 * cloud-processing consent state with the existing Allow flow AND the new
 * Revoke (an inline two-step confirm, NOT a browser confirm() and NOT a
 * floating overlay). Reads the same store state as the Privacy section's
 * canonical consent card, so the two surfaces can't diverge.
 */
@Component({
  selector: "app-ai-privacy-strip",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./ai-privacy-strip.component.html",
  styleUrl: "./ai-privacy-strip.component.scss",
})
export class AiPrivacyStripComponent {
  private readonly store = inject(SettingsStore);

  readonly cloudConsented = this.store.cloudConsented;
  readonly consenting = this.store.consenting;
  readonly consentError = this.store.consentError;
  readonly revoking = this.store.revoking;
  readonly revokeError = this.store.revokeError;
  readonly defaultEgressDestination = this.store.defaultEgressDestination;

  /** True while the inline "Really revoke?" confirm step is showing. */
  readonly confirmingRevoke = signal(false);

  allowCloudProcessing(): void {
    void this.store.allowCloudProcessing();
  }

  startRevoke(): void {
    this.confirmingRevoke.set(true);
  }

  cancelRevoke(): void {
    this.confirmingRevoke.set(false);
  }

  async confirmRevoke(): Promise<void> {
    await this.store.revokeCloudProcessing();
    this.confirmingRevoke.set(false);
  }
}
