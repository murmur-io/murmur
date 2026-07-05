import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { SettingsStore } from "../../settings.store";

/**
 * Settings → obsidian section (Stage-1 split): the `@case ("obsidian")` block of the
 * former settings.component.ts monolith, moved VERBATIM. State/actions live in
 * the shell-provided SettingsStore so section switches never drop them.
 */
@Component({
  selector: "app-settings-obsidian-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./settings-obsidian-section.component.html",
  styleUrl: "./settings-obsidian-section.component.scss",
})
export class SettingsObsidianSectionComponent {
  private readonly store = inject(SettingsStore);

  readonly obsidianUrl = this.store.obsidianUrl;
  readonly urlCopied = this.store.urlCopied;

  copyObsidianUrl(): void {
    void this.store.copyObsidianUrl();
  }
}
