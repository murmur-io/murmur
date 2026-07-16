import {
  ChangeDetectionStrategy,
  Component,
  type ElementRef,
  forwardRef,
  input,
  signal,
  viewChild,
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

  private readonly box =
    viewChild<ElementRef<HTMLInputElement>>("box");

  private onChange: (v: boolean) => void = () => undefined;
  private onTouched: () => void = () => undefined;

  writeValue(v: unknown): void {
    const val = !!v;
    this.checked.set(val);
    // A confirm-then-REVERT (user flips → backend rejects → setValue back)
    // coalesces the signal's false→true→false into a net no-change inside one
    // CD cycle, so the [checked] binding never rewrites the NATIVE property
    // the click already flipped. Sync it directly — exactly what Angular's
    // CheckboxControlValueAccessor does.
    const el = this.box()?.nativeElement;
    if (el && el.checked !== val) {
      el.checked = val;
    }
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
