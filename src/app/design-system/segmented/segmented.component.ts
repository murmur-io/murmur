import {
  ChangeDetectionStrategy,
  Component,
  input,
  model,
} from "@angular/core";
import {
  MurIconComponent,
  type ShellIcon,
} from "../icon/icon.component";

/** One choice in a <mur-segmented> control. */
export interface SegmentOption {
  readonly value: string;
  readonly label: string;
  readonly icon?: ShellIcon;
}

/**
 * Design System — <mur-segmented>: the pill segmented control (Light/Dark/
 * System pattern). Two-way bind the selection with [(value)].
 */
@Component({
  selector: "mur-segmented",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurIconComponent],
  templateUrl: "./segmented.component.html",
  styleUrl: "./segmented.component.scss",
})
export class MurSegmentedComponent {
  readonly options = input.required<readonly SegmentOption[]>();
  readonly ariaLabel = input<string | null>(null);

  readonly value = model("");

  select(v: string): void {
    this.value.set(v);
  }
}
