import {
  ChangeDetectionStrategy,
  Component,
  input,
  model,
} from "@angular/core";

/**
 * Design System — <mur-slider>: the Liquid Glass range slider (pill track,
 * round specular thumb). Two-way bind with [(value)] or [value]/(valueChange).
 */
@Component({
  selector: "mur-slider",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./slider.component.html",
  styleUrl: "./slider.component.scss",
})
export class MurSliderComponent {
  readonly min = input(0);
  readonly max = input(100);
  readonly step = input(1);
  readonly ariaLabel = input<string | null>(null);
  readonly disabled = input(false);

  readonly value = model(0);

  onInput(e: Event): void {
    this.value.set(Number((e.target as HTMLInputElement).value));
  }
}
