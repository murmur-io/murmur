import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { GlassService } from "../../../../services/glass.service";
import { ThemeService, type ThemeMode } from "../../../../services/theme.service";

/**
 * Settings → appearance section (Stage-1 split): the `@case ("appearance")` block of the
 * former settings.component.ts monolith, moved VERBATIM. State/actions live in
 * the shell-provided SettingsStore so section switches never drop them.
 */
@Component({
  selector: "app-settings-appearance-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./settings-appearance-section.component.html",
  styleUrl: "./settings-appearance-section.component.scss",
})
export class SettingsAppearanceSectionComponent {
  private readonly theme = inject(ThemeService);
  private readonly glass = inject(GlassService);

  /** Current theme choice (Light / Dark / System) — drives the Appearance control. */
  readonly themeMode = this.theme.mode;

  /** Liquid Glass transparency 0–100 — drives the slider position + label. */
  readonly glassLevel = this.glass.level;

  /** Apply a theme immediately (persisted in the service; no save() needed). */
  setTheme(mode: ThemeMode): void {
    this.theme.setMode(mode);
  }

  /** Apply + persist the glass level live as the slider moves (auto-saved). */
  setGlass(value: string): void {
    this.glass.setLevel(Number(value));
  }
}
