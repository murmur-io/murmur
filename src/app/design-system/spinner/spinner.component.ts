import { ChangeDetectionStrategy, Component, input } from "@angular/core";

/** Design System — <mur-spinner>: the ring progress spinner. */
@Component({
  selector: "mur-spinner",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./spinner.component.html",
  styleUrl: "./spinner.component.scss",
})
export class MurSpinnerComponent {
  /** Diameter in px. */
  readonly size = input(16);
}
