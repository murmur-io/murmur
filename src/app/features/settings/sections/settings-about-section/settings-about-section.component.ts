import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { IpcService } from "../../../../core/ipc.service";
import { UpdateService } from "../../../../services/update.service";
import { SettingsStore } from "../../settings.store";

/**
 * Settings → about section (Stage-1 split): the `@case ("about")` block of the
 * former settings.component.ts monolith, moved VERBATIM. State/actions live in
 * the shell-provided SettingsStore so section switches never drop them.
 */
@Component({
  selector: "app-settings-about-section",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./settings-about-section.component.html",
  styleUrl: "./settings-about-section.component.scss",
})
export class SettingsAboutSectionComponent {
  private readonly store = inject(SettingsStore);
  private readonly ipc = inject(IpcService);
  private readonly updates = inject(UpdateService);

  /** Static product identity (name/version/description), loaded by the store. */
  readonly appInfo = this.store.appInfo;

  /** Update-check lifecycle from the shared service (drives the button + result line). */
  readonly updateStatus = this.updates.status;

  /** The most-recent update-check result (the Download button reads its releaseUrl). */
  readonly latestUpdate = this.updates.latest;

  /** Run the shared manual update check (surfaces both outcomes as toasts). */
  checkForUpdates(): void {
    void this.updates.checkManually();
  }

  /** Open the GitHub release page for an available update. */
  downloadUpdate(url: string): void {
    void this.ipc.openReleasePage(url);
  }
}
