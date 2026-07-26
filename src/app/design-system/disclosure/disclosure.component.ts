import { ChangeDetectionStrategy, Component, input, model } from "@angular/core";

/** Per-instance id source. A counter — not `Math.random()` — so ids stay deterministic. */
let nextDisclosureId = 0;

/**
 * Design System — `<mur-disclosure>`: the progressive-disclosure section.
 *
 * A summary/trigger button over a projected panel, following the WAI-ARIA
 * Disclosure pattern: the trigger is a real `<button>` (so Enter/Space are
 * native), carries `aria-expanded`, and points `aria-controls` at the panel's
 * id. The panel therefore STAYS IN THE DOM and toggles via `hidden` — an
 * `aria-controls` that dangles when collapsed is an accessibility bug, and
 * removing the panel would not have bought laziness anyway (Angular
 * instantiates projected content with the CALLER's view either way).
 *
 * NOT an overlay — the panel is IN-FLOW, so it uses `--surface-input`, not
 * `--surface-overlay` (rule T3 applies to floating content only).
 *
 * `:host { display: contents }` is DELIBERATE and load-bearing: the hosts that
 * use this (e.g. `settings-ai-section`) flex-stack their children, so the
 * component element must not become an extra box between the flex container
 * and `.disc-wrap`. Removing it breaks the settings layout.
 *
 * @example
 *   <mur-disclosure [(open)]="expanded" panelLabel="Advanced settings">
 *     <span murDisclosureSummary>⚙ Advanced</span>
 *     <app-thing />
 *   </mur-disclosure>
 */
@Component({
  selector: "mur-disclosure",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./disclosure.component.html",
  styleUrl: "./disclosure.component.scss",
})
export class MurDisclosureComponent {
  /** Open state. Two-way: `[(open)]="someWritableSignal"`. */
  readonly open = model(false);

  /** Accessible name for the panel region (the trigger names itself from its content). */
  readonly panelLabel = input<string | null>(null);

  /** Stable, unique ids so `aria-controls`/`aria-labelledby` always resolve. */
  private readonly uid = `mur-disclosure-${nextDisclosureId++}`;
  readonly panelId = `${this.uid}-panel`;
  readonly triggerId = `${this.uid}-trigger`;

  toggle(): void {
    this.open.update((v) => !v);
  }
}
