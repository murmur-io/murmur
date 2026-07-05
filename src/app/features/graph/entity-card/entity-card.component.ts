import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
  output,
} from "@angular/core";
import type { GraphNode } from "../../../core/models";

/**
 * A single directory row in the graph's People/Projects list: a kind dot (cool
 * accent for people, violet for projects), the entity name, and a tabular-nums
 * mention-count chip. Selecting it emits the entity id so the container can open
 * the detail panel. Purely presentational — no IPC, no state beyond inputs.
 */
@Component({
  selector: "app-entity-card",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./entity-card.component.html",
  styleUrl: "./entity-card.component.scss",
})
export class EntityCardComponent {
  /** The directory row to render (entity + its visible mention count). */
  readonly entity = input.required<GraphNode>();
  /** Whether this row is the currently-open entity in the detail panel. */
  readonly selected = input(false);
  /** Emits the entity id when the row is chosen. */
  readonly select = output<string>();

  protected readonly isProject = computed(
    () => this.entity().kind === "project",
  );
  protected readonly kindLabel = computed(() =>
    this.isProject() ? "Project" : "Person",
  );
  protected readonly countLabel = computed(() => {
    const n = this.entity().mentionCount;
    return n === 1 ? "1 mention" : `${n} mentions`;
  });
}
