import { ChangeDetectionStrategy, Component, input } from "@angular/core";

/**
 * Compact provider mark for AI engine rows. The paths are app-native,
 * single-color brand cues (not remote assets), so they inherit the current
 * theme and never add an image request or package dependency.
 */
@Component({
  selector: "mur-provider-icon",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./provider-icon.component.html",
  styleUrl: "./provider-icon.component.scss",
  host: {
    class: "provider-mark",
    "aria-hidden": "true",
    "[attr.data-provider-icon]": "provider()",
  },
})
export class MurProviderIconComponent {
  readonly provider = input.required<string>();
}
