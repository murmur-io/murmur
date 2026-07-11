import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import {
  MurSegmentedComponent,
  type SegmentOption,
} from "../../../../design-system/segmented/segmented.component";
import { MurSliderComponent } from "../../../../design-system/slider/slider.component";
import {
  ChromeService,
  type AccentId,
  type SidebarCollapseStyle,
} from "../../../../services/chrome.service";
import { GlassService } from "../../../../services/glass.service";
import { ThemeService, type ThemeMode } from "../../../../services/theme.service";

/**
 * Settings → appearance section: theme choice (mur-segmented) + the Liquid
 * Glass transparency slider (mur-slider). State/actions live in the
 * root-provided Theme/Glass services so section switches never drop them.
 */
@Component({
  selector: "app-settings-appearance-section",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurSegmentedComponent, MurSliderComponent],
  templateUrl: "./settings-appearance-section.component.html",
  styleUrl: "./settings-appearance-section.component.scss",
})
export class SettingsAppearanceSectionComponent {
  private readonly theme = inject(ThemeService);
  private readonly glass = inject(GlassService);
  private readonly chrome = inject(ChromeService);

  /** The three theme choices, rendered by <mur-segmented>. */
  readonly themeOptions: readonly SegmentOption[] = [
    { value: "light", label: "Light", icon: "sun" },
    { value: "dark", label: "Dark", icon: "moon" },
    { value: "system", label: "System", icon: "display" },
  ];

  /** Current theme choice (Light / Dark / System) — drives the Appearance control. */
  readonly themeMode = this.theme.mode;

  /** Liquid Glass transparency 0–100 — drives the slider position + label. */
  readonly glassLevel = this.glass.level;

  /** Apply a theme immediately (persisted in the service; no save() needed). */
  setTheme(mode: string): void {
    this.theme.setMode(mode as ThemeMode);
  }

  /** Apply + persist the glass level live as the slider moves (auto-saved). */
  setGlass(value: number): void {
    this.glass.setLevel(value);
  }

  /** The two sidebar-collapse behaviors, rendered by <mur-segmented>. */
  readonly collapseOptions: readonly SegmentOption[] = [
    { value: "bar", label: "Top bar", icon: "topbar" },
    { value: "rail", label: "Icon rail", icon: "sidebar" },
  ];

  /** Current collapse behavior — drives the Sidebar control. */
  readonly collapseStyle = this.chrome.collapseStyle;

  /** Apply a collapse behavior immediately (persisted in the service). */
  setCollapseStyle(style: string): void {
    this.chrome.setCollapseStyle(style as SidebarCollapseStyle);
  }

  /** The accent swatches; each maps to an `--accent-option-*` token class. */
  readonly accentOptions: readonly { id: AccentId; label: string }[] = [
    { id: "purple", label: "Purple" },
    { id: "blue", label: "Blue" },
    { id: "teal", label: "Teal" },
    { id: "green", label: "Green" },
    { id: "orange", label: "Orange" },
    { id: "pink", label: "Pink" },
  ];

  /** Current accent palette — drives the swatch selection ring. */
  readonly accent = this.chrome.accent;

  /** Apply an accent immediately (persisted in the service). */
  setAccent(accent: AccentId): void {
    this.chrome.setAccent(accent);
  }
}
