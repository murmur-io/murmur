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

  // brain2 connectors (Phase 2) — Jira.
  readonly jiraTokenControl = this.store.jiraTokenControl;
  readonly hasJiraToken = this.store.hasJiraToken;
  readonly savingJiraToken = this.store.savingJiraToken;
  readonly jiraTokenError = this.store.jiraTokenError;
  readonly jiraConsented = this.store.jiraConsented;
  readonly jiraConsenting = this.store.jiraConsenting;
  readonly jiraConsentError = this.store.jiraConsentError;

  // brain2 connectors (Phase 3) — Slack.
  readonly slackTokenControl = this.store.slackTokenControl;
  readonly hasSlackToken = this.store.hasSlackToken;
  readonly savingSlackToken = this.store.savingSlackToken;
  readonly slackTokenError = this.store.slackTokenError;
  readonly slackConsented = this.store.slackConsented;
  readonly slackConsenting = this.store.slackConsenting;
  readonly slackConsentError = this.store.slackConsentError;

  saveWebKey(): void {
    void this.store.saveWebKey();
  }

  allowWebSearch(): void {
    void this.store.allowWebSearch();
  }

  saveJiraToken(): void {
    void this.store.saveJiraToken();
  }

  allowJira(): void {
    void this.store.allowJira();
  }

  saveSlackToken(): void {
    void this.store.saveSlackToken();
  }

  allowSlack(): void {
    void this.store.allowSlack();
  }
}
