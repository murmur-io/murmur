import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  computed,
  effect,
  forwardRef,
  input,
  model,
  signal,
  untracked,
  viewChild,
} from "@angular/core";
import { NG_VALUE_ACCESSOR, type ControlValueAccessor } from "@angular/forms";

/** One notch of the ladder. `id` is the committed value; `name` is what a screen reader says. */
export interface PowerRung {
  /** The value committed to the form / `[(value)]` when this rung is chosen. */
  id: string;
  /** The HUMAN rung name — never a raw id. This is what `aria-valuetext` announces. */
  name: string;
}

/**
 * Design System — `<mur-power-slider>`: a DISCRETE ladder as a range control.
 *
 * It owns its own `<input type="range">` and shares mur-slider's look through the
 * `.mur-range` primitive (`design-system/primitives.css`). It deliberately does NOT
 * wrap `<mur-slider>`: that would need six passthrough bindings (min/max/step/
 * disabled/aria/value) and would make mur-slider's appearance depend on what a
 * caller passes — a design-system component's whole job is the opposite.
 *
 * PREVIEW vs COMMIT. Dragging fires `input` continuously; only `change` (pointer
 * released / keyboard settled) commits. So a drag moves the thumb and the visible
 * rung, but `value` — and `onChange`, i.e. the form and any persistence the host
 * hangs off it — updates ONCE, at the end. `<mur-select>`'s `onChangeEvent` is the
 * in-repo precedent for the same split. Without it, dragging past Maximum would
 * persist (and, in the model picker, could start downloading) every rung on the way.
 *
 * TWO ways to drive it, exactly like `<mur-select>`, and they must not fight:
 *   - `formControlName` / `[formControl]` — the CVA path (`writeValue` /
 *     `setDisabledState`);
 *   - `[(value)]` + `[disabled]` — the signal path, for hosts with no reactive form
 *     (the onboarding wizard is signals-only).
 * `value` is a `model()` so both writers hit the SAME signal; `disabled` cannot be a
 * plain input for the CVA (an input signal is read-only), so `setDisabledState`'s
 * answer lives in a private signal and the two are OR-ed in {@link isDisabled}.
 *
 * ACCESSIBILITY is not decoration here:
 *   - `aria-valuetext` reads the rung NAME ("Balanced"), because "2 of 4" tells a
 *     screen-reader user nothing about what they just chose;
 *   - PageUp / PageDown jump exactly ONE rung. Browsers move a range by ~10% of its
 *     span on those keys, which silently stops being one rung the moment the ladder
 *     grows past ten — so the jump is implemented rather than inherited;
 *   - Home / End / arrows are the native range behaviour, untouched;
 *   - the tick labels are `aria-hidden` decoration: the input already announces the
 *     rung, and a second set of controls for the same value is a worse experience,
 *     not a better one.
 */
@Component({
  selector: "mur-power-slider",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./power-slider.component.html",
  styleUrl: "./power-slider.component.scss",
  providers: [
    {
      provide: NG_VALUE_ACCESSOR,
      useExisting: forwardRef(() => MurPowerSliderComponent),
      multi: true,
    },
  ],
})
export class MurPowerSliderComponent implements ControlValueAccessor {
  /** The ladder, in display order (cheapest first). */
  readonly rungs = input<readonly PowerRung[]>([]);

  /** Accessible name for the control — required in practice. */
  readonly ariaLabel = input<string | null>(null);

  /** The COMMITTED rung id. Two-way (`[(value)]`) AND the CVA's model. */
  readonly value = model("");

  /** Caller-driven disable. Independent of — and OR-ed with — `setDisabledState`. */
  readonly disabled = input(false);

  /** `setDisabledState` from a disabled FormControl. Private: the input is read-only. */
  private readonly cvaDisabled = signal(false);

  /** The effective disabled state the `<input>` binds. */
  readonly isDisabled = computed(() => this.disabled() || this.cvaDisabled());

  private readonly range =
    viewChild<ElementRef<HTMLInputElement>>("rangeInput");

  /** In-flight drag position, or `null` when nothing is being previewed. */
  private readonly _preview = signal<number | null>(null);

  /** Highest valid index (never negative, so an empty ladder still renders a sane track). */
  readonly maxIndex = computed(() => Math.max(0, this.rungs().length - 1));

  /**
   * Index of the COMMITTED value. An unknown id (a long-tail size chosen from "show
   * every size", or a custom one) has no rung, and answers `-1` — the callers below
   * clamp it to 0 for the thumb while {@link isOffLadder} keeps the UI honest about it.
   */
  readonly committedIndex = computed(() =>
    this.rungs().findIndex((r) => r.id === this.value()),
  );

  /** The committed value is not one of the rungs (so no rung should read as active). */
  readonly isOffLadder = computed(() => this.committedIndex() < 0);

  /** What the control SHOWS right now: the drag preview if there is one, else the commit. */
  readonly activeIndex = computed(() => {
    const preview = this._preview();
    if (preview !== null) return preview;
    return Math.max(0, this.committedIndex());
  });

  /** The rung the control currently shows, or `null` for an empty ladder. */
  readonly activeRung = computed<PowerRung | null>(
    () => this.rungs()[this.activeIndex()] ?? null,
  );

  /** `aria-valuetext`: the human rung name, NEVER the index and never the raw id. */
  readonly valueText = computed(() => this.activeRung()?.name ?? "");

  /** Track fill, 0–100. A single-rung ladder is fully filled rather than 0/0 = NaN. */
  readonly fillPct = computed(() => {
    const max = this.maxIndex();
    if (max <= 0) return 100;
    return (this.activeIndex() / max) * 100;
  });

  private onChange: (v: string) => void = () => undefined;
  private onTouched: () => void = () => undefined;

  /**
   * THE DISABLED-MID-DRAG TRAP (the signal-CVA revert-coalescing trap). When the
   * host disables the control while a drag is in flight, the preview is abandoned
   * and the thumb must snap back to the committed rung — but abandoning the preview
   * is a NET-ZERO change to `activeIndex` (it was N because of the preview, and it
   * is N again from the commit). A `[value]` binding compares against the last value
   * IT wrote, sees no change, and never touches the element again, leaving the DOM
   * showing the abandoned position. So the element's `value` PROPERTY is written
   * directly here. Tracking `isDisabled()` is what makes this effect run at all: that
   * IS a real signal change, unlike the preview it cleans up after.
   */
  private readonly _revertOnDisable = effect(() => {
    if (!this.isDisabled()) return;
    const el = this.range()?.nativeElement;
    if (!el) return;
    untracked(() => {
      this._preview.set(null);
      el.value = String(Math.max(0, this.committedIndex()));
    });
  });

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
    this.cvaDisabled.set(d);
  }

  /** Dragging: PREVIEW only — no commit, no `onChange`, nothing persisted. */
  onInput(e: Event): void {
    this._preview.set(this.clamp(Number((e.target as HTMLInputElement).value)));
  }

  /** Released / settled: COMMIT. This is the only place `onChange` is called. */
  onChangeEvent(e: Event): void {
    this.commit(this.clamp(Number((e.target as HTMLInputElement).value)));
  }

  /**
   * PageUp / PageDown move exactly one rung. `preventDefault` stops the browser's
   * own ~10%-of-span jump, which means the element's value is NOT updated by the
   * browser — {@link commit} writes the signal, and the `[value]` binding (a real
   * change) puts it on the element.
   */
  onKeydown(e: KeyboardEvent): void {
    if (this.isDisabled()) return;
    const step = e.key === "PageUp" ? 1 : e.key === "PageDown" ? -1 : 0;
    if (step === 0) return;
    e.preventDefault();
    this.commit(this.clamp(this.activeIndex() + step));
  }

  /** Write the rung at `index` through both writers (signal + CVA) and end the preview. */
  private commit(index: number): void {
    const rung = this.rungs()[index];
    this._preview.set(null);
    if (!rung) return;
    this.value.set(rung.id);
    this.onChange(rung.id);
    this.onTouched();
  }

  /** Clamp into `0..maxIndex`; a NaN (an empty / non-numeric element value) reads as 0. */
  private clamp(index: number): number {
    if (!Number.isFinite(index)) return 0;
    return Math.min(Math.max(Math.round(index), 0), this.maxIndex());
  }
}
