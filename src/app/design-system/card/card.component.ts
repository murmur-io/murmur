import { ChangeDetectionStrategy, Component } from "@angular/core";

/**
 * Design System — <mur-card>: the frosted glass surface for IN-FLOW panels.
 * NEVER use for floating overlays (rule T3 — those need var(--surface-overlay)).
 */
@Component({
  selector: "mur-card",
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { class: "card" },
  templateUrl: "./card.component.html",
  styleUrl: "./card.component.scss",
})
export class MurCardComponent {}
