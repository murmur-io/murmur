import { ChangeDetectionStrategy, Component, input } from "@angular/core";
import { MurSpinnerComponent } from "../spinner/spinner.component";

/** Visual variants — mirror the `.btn` primitive family (primitives.css). */
export type MurButtonVariant = "default" | "primary" | "ghost" | "danger";

/**
 * Design System — `<mur-button>`: the button. Renders a NATIVE `<button>`
 * inside, so platform semantics (type, disabled, focus order, Enter/Space
 * activation) stay real; the component owns the typed visual variant, the
 * `sm` size, and the in-flight `busy` state (ring spinner + muted
 * interaction). Callers bind `(click)` on the host as usual — the inner
 * button's click bubbles up, and a disabled/busy host swallows pointer events
 * entirely (`pointer-events: none`), so no phantom clicks from the padding.
 *
 * The visuals RIDE the `.btn` class primitives rather than re-declaring them,
 * so a migrated call site is pixel-identical to a legacy `class="btn …"` one —
 * the wave migration can proceed screen by screen with zero visual drift.
 *
 * Not yet covered (still legacy `class="btn …"`, migrate in a later wave):
 * link-shaped buttons (`<a routerLink>`) and icon-only squares with bespoke
 * geometry — they need `href`/`iconOnly` support here first.
 */
@Component({
  selector: "mur-button",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurSpinnerComponent],
  templateUrl: "./button.component.html",
  styleUrl: "./button.component.scss",
  host: {
    // Disabled/busy must also kill clicks on the HOST box (the caller's
    // (click) listener lives there), not just on the inner native button.
    "[class.is-blocked]": "disabled() || busy()",
  },
})
export class MurButtonComponent {
  /** Visual variant; "default" is the frosted neutral `.btn`. */
  readonly variant = input<MurButtonVariant>("default");
  /** "sm" = compact 30px control (inline row actions, card footers). */
  readonly size = input<"md" | "sm">("md");
  /**
   * In-flight state: prepends a ring spinner and mutes interaction.
   * Semantically distinct from `disabled` — callers own that separately.
   */
  readonly busy = input(false);
  /** Mirrors the native `disabled` onto the inner `<button>`. */
  readonly disabled = input(false);
  /** Native button type; defaults to "button" so forms never submit by accident. */
  readonly type = input<"button" | "submit">("button");
  /** Accessible name for icon-only content (→ inner `aria-label`). */
  readonly ariaLabel = input<string | null>(null);
  /** Disclosure state for toggle buttons (→ inner `aria-expanded`). */
  readonly ariaExpanded = input<boolean | null>(null);
}
