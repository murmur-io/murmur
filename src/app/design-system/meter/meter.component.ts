import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
} from "@angular/core";

/**
 * Design System — `<mur-meter>`: a tiny segmented indicator for a COARSE, ordinal
 * quantity (accuracy, speed) where a percentage would be a lie.
 *
 * Deliberately not `<mur-progress>`: progress is a measured fraction of a known
 * whole, and it announces `aria-valuenow`. This is a relative rating with no unit,
 * so it announces a sentence ("Accuracy: 4 of 4") and nothing numeric that invites
 * being read as a percentage.
 *
 * The pips are `aria-hidden` decoration; the HOST carries `role="img"` plus the
 * label, so assistive tech hears one phrase instead of counting boxes.
 *
 * @example
 *   <mur-meter label="Accuracy" [value]="4" [max]="4" detail="Same as Sharp" />
 */
@Component({
  selector: "mur-meter",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./meter.component.html",
  styleUrl: "./meter.component.scss",
  host: {
    role: "img",
    "[attr.aria-label]": "ariaText()",
  },
})
export class MurMeterComponent {
  /** The quantity's name, shown and announced (e.g. `Accuracy`). */
  readonly label = input("");

  /** Filled pips, clamped into `0..max`. */
  readonly value = input(0);

  /** Total pips. */
  readonly max = input(4);

  /** Optional clarifying phrase, shown after the pips and announced with the label. */
  readonly detail = input<string | null>(null);

  /** Pip count, guarded so a bad `max` can never render a runaway (or empty) row. */
  readonly pips = computed(() => {
    const max = this.max();
    const n = Number.isFinite(max) ? Math.round(max) : 0;
    return Array.from({ length: Math.min(Math.max(n, 0), 10) }, (_, i) => i);
  });

  /** {@link value} clamped into the pip range — an out-of-range input never overfills. */
  readonly filled = computed(() => {
    const v = this.value();
    if (!Number.isFinite(v)) return 0;
    return Math.min(Math.max(Math.round(v), 0), this.pips().length);
  });

  /** The single phrase assistive tech hears. */
  readonly ariaText = computed(() => {
    const base = `${this.label()}: ${this.filled()} of ${this.pips().length}`;
    const detail = this.detail();
    return detail ? `${base}. ${detail}` : base;
  });
}
