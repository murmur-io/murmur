import { ChangeDetectionStrategy, Component, input } from "@angular/core";

/** Every glyph the shell chrome can render (nav + quick actions + chrome). */
export type ShellIcon =
  | "record"
  | "meetings"
  | "notes"
  | "reminders"
  | "dashboards"
  | "analytics"
  | "graph"
  | "people"
  | "brain"
  | "ask"
  | "settings"
  | "search"
  | "plus"
  | "sidebar"
  | "topbar"
  | "sun"
  | "moon"
  | "display";

/**
 * Design System — one inline-SVG glyph, shared by the floating sidebar, the
 * pill bar and any future chrome so the icon set lives in exactly one place.
 */
@Component({
  selector: "mur-icon",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./icon.component.html",
  styleUrl: "./icon.component.scss",
})
export class MurIconComponent {
  readonly icon = input.required<ShellIcon>();
}
