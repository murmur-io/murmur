import {
  ChangeDetectionStrategy,
  Component,
  forwardRef,
  input,
  signal,
} from "@angular/core";
import { NG_VALUE_ACCESSOR, type ControlValueAccessor } from "@angular/forms";

/**
 * Design System — <mur-input>: a text field as a form control (CVA), visual
 * language from the global field primitives. Use for plain text/password/url
 * fields; number fields keep the native input (see the storage-limit lesson:
 * NumberValueAccessor commits numbers).
 */
@Component({
  selector: "mur-input",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./input.component.html",
  styleUrl: "./input.component.scss",
  providers: [
    {
      provide: NG_VALUE_ACCESSOR,
      useExisting: forwardRef(() => MurInputComponent),
      multi: true,
    },
  ],
})
export class MurInputComponent implements ControlValueAccessor {
  readonly type = input<"text" | "password" | "search" | "url" | "email">("text");
  readonly placeholder = input("");
  readonly ariaLabel = input<string | null>(null);
  readonly autocomplete = input("off");
  readonly spellcheck = input(false);

  readonly value = signal("");
  readonly disabled = signal(false);

  private onChange: (v: string) => void = () => undefined;
  private onTouched: () => void = () => undefined;

  writeValue(v: unknown): void {
    this.value.set(v == null ? "" : String(v));
  }
  registerOnChange(fn: (v: string) => void): void {
    this.onChange = fn;
  }
  registerOnTouched(fn: () => void): void {
    this.onTouched = fn;
  }
  setDisabledState(d: boolean): void {
    this.disabled.set(d);
  }

  onInput(e: Event): void {
    const v = (e.target as HTMLInputElement).value;
    this.value.set(v);
    this.onChange(v);
  }
  onBlur(): void {
    this.onTouched();
  }
}
