import {
  ChangeDetectionStrategy,
  Component,
  forwardRef,
  input,
  signal,
} from "@angular/core";
import { NG_VALUE_ACCESSOR, type ControlValueAccessor } from "@angular/forms";

/**
 * Design System — <mur-toggle>: the macOS-style ON/OFF switch as a form
 * control (ControlValueAccessor), so `formControlName` binds to it directly.
 * The visual rides the global `.switch` primitive (design-system/primitives.css).
 */
@Component({
  selector: "mur-toggle",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./toggle.component.html",
  styleUrl: "./toggle.component.scss",
  providers: [
    {
      provide: NG_VALUE_ACCESSOR,
      useExisting: forwardRef(() => MurToggleComponent),
      multi: true,
    },
  ],
})
export class MurToggleComponent implements ControlValueAccessor {
  /** Accessible name for the bare switch (the visible copy usually sits in a sibling label). */
  readonly ariaLabel = input<string | null>(null);

  readonly checked = signal(false);
  readonly disabled = signal(false);

  private onChange: (v: boolean) => void = () => undefined;
  private onTouched: () => void = () => undefined;

  writeValue(v: unknown): void {
    this.checked.set(!!v);
  }
  registerOnChange(fn: (v: boolean) => void): void {
    this.onChange = fn;
  }
  registerOnTouched(fn: () => void): void {
    this.onTouched = fn;
  }
  setDisabledState(d: boolean): void {
    this.disabled.set(d);
  }

  onInput(e: Event): void {
    const v = (e.target as HTMLInputElement).checked;
    this.checked.set(v);
    this.onChange(v);
    this.onTouched();
  }
}
