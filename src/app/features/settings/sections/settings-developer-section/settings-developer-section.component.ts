import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { RouterLink } from "@angular/router";
import { DeveloperModeService } from "../../../../services/developer-mode.service";

/**
 * Settings → Developer section: the one toggle that reveals the diagnostics
 * tools, plus a shortcut into them once it is on.
 *
 * State lives in the ROOT {@link DeveloperModeService} — not in a local signal
 * and not in the settings form — because the shell sidebar (always mounted)
 * reads the same signal, so flipping the switch here lights the sidebar group
 * up immediately and survives leaving /settings.
 */
@Component({
  selector: "app-settings-developer-section",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  templateUrl: "./settings-developer-section.component.html",
  styleUrl: "./settings-developer-section.component.scss",
})
export class SettingsDeveloperSectionComponent {
  private readonly developer = inject(DeveloperModeService);

  /** Whether developer mode is on. Default off. */
  readonly enabled = this.developer.enabled;

  /** Flip the switch (auto-saved — there is no Save button in Settings). */
  setEnabled(event: Event): void {
    this.developer.setEnabled((event.target as HTMLInputElement).checked);
  }
}
