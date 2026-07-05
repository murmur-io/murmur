import { ChangeDetectionStrategy, Component, input } from "@angular/core";

export type PillKind = "neutral" | "success" | "warning" | "danger" | "accent";

/** Design System — <mur-pill kind="success">…</mur-pill>: the status pill. */
@Component({
  selector: "mur-pill",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: {
    class: "pill",
    "[class.is-success]": 'kind() === "success"',
    "[class.is-warning]": 'kind() === "warning"',
    "[class.is-danger]": 'kind() === "danger"',
    "[class.is-accent]": 'kind() === "accent"',
  },
  templateUrl: "./pill.component.html",
  styleUrl: "./pill.component.scss",
})
export class MurPillComponent {
  readonly kind = input<PillKind>("neutral");
  /** Show the little status dot before the content. */
  readonly dot = input(true);
}
