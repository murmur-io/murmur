import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
} from "@angular/core";

/** Bar height presets. `sm` is the settings/inline default; `md` is the wizard bar. */
export type MurProgressSize = "sm" | "md";

/**
 * Design System — `<mur-progress>`: the ONE linear progress bar.
 *
 * Replaces seven hand-rolled `*-progress-track` / `*-progress-fill` pairs
 * (onboarding, settings-transcription, settings-privacy, and the four AI
 * blocks), which had drifted into two sizes and two track colours.
 *
 * Two states:
 *   - DETERMINATE — `[value]` is a number in `0..max()`; the fill is a `width:%`
 *     and the host reports `aria-valuenow`.
 *   - INDETERMINATE — `[value]` is `null` (the default); a sliding sliver
 *     animates and `aria-valuenow` is OMITTED, which is exactly how a
 *     `progressbar` signals "busy, amount unknown" to assistive tech.
 *
 * The host IS the track (`role="progressbar"`), so a caller never needs a
 * wrapper element. Content is in-flow, never an overlay (T3 does not apply).
 *
 * @example
 *   <mur-progress [value]="downloadFrac() * 100" ariaLabel="Model download" />
 *   <mur-progress ariaLabel="Re-indexing" />   <!-- indeterminate -->
 */
@Component({
  selector: "mur-progress",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./progress.component.html",
  styleUrl: "./progress.component.scss",
  host: {
    role: "progressbar",
    "[class.is-md]": 'size() === "md"',
    "[class.is-indeterminate]": "indeterminate()",
    "[attr.aria-label]": "ariaLabel()",
    "[attr.aria-valuemin]": "0",
    "[attr.aria-valuemax]": "max()",
    // Indeterminate MUST NOT carry aria-valuenow — its absence is the signal.
    "[attr.aria-valuenow]": "indeterminate() ? null : valueNow()",
  },
})
export class MurProgressComponent {
  /** Progress in `0..max()`. `null` (the default) renders the indeterminate bar. */
  readonly value = input<number | null>(null);

  /** Upper bound of {@link value}. Callers pass percentages, so 100 by default. */
  readonly max = input(100);

  /** Accessible name — REQUIRED in practice: a bar with no label announces nothing useful. */
  readonly ariaLabel = input<string | null>(null);

  /** `sm` = 6px (inline/settings rows), `md` = 8px (the onboarding + privacy wizards). */
  readonly size = input<MurProgressSize>("sm");

  /** No value ⇒ "busy, amount unknown". */
  readonly indeterminate = computed(() => this.value() === null);

  /** The value clamped into `0..max()` (a NaN or out-of-range input can never widen the fill past the track). */
  readonly valueNow = computed(() => {
    const raw = this.value();
    if (raw === null || !Number.isFinite(raw)) return 0;
    return Math.min(Math.max(raw, 0), Math.max(this.max(), 0));
  });

  /** The fill width as a percentage of the track. */
  readonly fillPct = computed(() => {
    const max = this.max();
    if (!Number.isFinite(max) || max <= 0) return 0;
    return (this.valueNow() / max) * 100;
  });
}
