import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { MurToggleComponent } from "../../../../../design-system/toggle/toggle.component";
import { SettingsStore } from "../../../settings.store";

/**
 * AI & Models → "Live during meetings" block (Task 5).
 *
 * Extracted verbatim from AiDefaultsBlockComponent as a standalone card.
 * Owns the two live-meeting toggles (in-meeting voice assistant +
 * proactive brain hints) and the cloud-egress consent warning that appears
 * when the in-meeting assistant is on, the live role resolves to a cloud
 * provider, and the user has not yet consented.
 *
 * Not an overlay — the consent banner is IN-FLOW (frosted `.banner`,
 * correct per angular-zoneless.md §T3).
 */
@Component({
  selector: "app-during-meetings-block",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    MurToggleComponent,ReactiveFormsModule],
  templateUrl: "./during-meetings-block.component.html",
  styleUrl: "./during-meetings-block.component.scss",
})
export class DuringMeetingsBlockComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;
  readonly cloudConsented = this.store.cloudConsented;
  readonly consenting = this.store.consenting;
  readonly consentError = this.store.consentError;
  readonly liveTargetIsCloud = this.store.liveTargetIsCloud;

  allowCloudProcessing(): void {
    void this.store.allowCloudProcessing();
  }
}
