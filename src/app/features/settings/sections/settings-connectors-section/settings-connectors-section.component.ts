import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { MurToggleComponent } from "../../../../design-system/toggle/toggle.component";
import { SettingsStore } from "../../settings.store";

/**
 * Settings → connectors section (Stage-1 split): the `@case ("connectors")` block of the
 * former settings.component.ts monolith, moved VERBATIM. State/actions live in
 * the shell-provided SettingsStore so section switches never drop them.
 */
@Component({
  selector: "app-settings-connectors-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    MurToggleComponent,ReactiveFormsModule],
  templateUrl: "./settings-connectors-section.component.html",
  styleUrl: "./settings-connectors-section.component.scss",
})
export class SettingsConnectorsSectionComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;
  readonly webKeyControl = this.store.webKeyControl;
  readonly hasWebKey = this.store.hasWebKey;
  readonly savingWebKey = this.store.savingWebKey;
  readonly webKeyError = this.store.webKeyError;
  readonly webConsented = this.store.webConsented;
  readonly webConsenting = this.store.webConsenting;
  readonly webConsentError = this.store.webConsentError;

  saveWebKey(): void {
    void this.store.saveWebKey();
  }

  allowWebSearch(): void {
    void this.store.allowWebSearch();
  }
}
