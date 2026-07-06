import { ChangeDetectionStrategy, Component } from "@angular/core";

/** Design System — <mur-empty-state>: the shared empty/loading well. */
@Component({
  selector: "mur-empty-state",
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { class: "empty-state" },
  templateUrl: "./empty-state.component.html",
  styleUrl: "./empty-state.component.scss",
})
export class MurEmptyStateComponent {}
