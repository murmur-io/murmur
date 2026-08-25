import { ChangeDetectionStrategy, Component, input } from "@angular/core";

/** Every glyph the shell chrome can render (nav + quick actions + chrome). */
export type ShellIcon =
  | "record"
  | "meetings"
  | "notes"
  | "reminders"
  | "tasks"
  | "dashboards"
  | "analytics"
  | "graph"
  | "people"
  | "brain"
  | "ask"
  | "settings"
  | "search"
  | "spaces"
  | "browse"
  | "history"
  | "plus"
  | "sidebar"
  | "topbar"
  | "sun"
  | "moon"
  | "display"
  // Dashboard tile marks. A board tile identifies its KIND by a glyph, and the
  // glyph set lives here with every other icon rather than inline in one feature.
  | "document"
  | "drift"
  | "numbers"
  | "pulse"
  | "promises"
  | "lock";

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
