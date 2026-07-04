import { ChangeDetectionStrategy, Component, input } from "@angular/core";

/** Every glyph the shell chrome can render (nav + quick actions + chrome). */
export type ShellIcon =
  | "record"
  | "meetings"
  | "analytics"
  | "graph"
  | "people"
  | "brain"
  | "ask"
  | "settings"
  | "search"
  | "plus"
  | "sidebar";

/**
 * Design System — one inline-SVG glyph, shared by the floating sidebar, the
 * pill bar and any future chrome so the icon set lives in exactly one place.
 */
@Component({
  selector: "app-nav-icon",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./nav-icon.component.html",
  styleUrl: "./nav-icon.component.scss",
})
export class NavIconComponent {
  readonly icon = input.required<ShellIcon>();
}
