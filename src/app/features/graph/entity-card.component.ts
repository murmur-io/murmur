import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
  output,
} from "@angular/core";
import type { GraphNode } from "../../core/models";

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
  template: `
    <button
      type="button"
      class="ec"
      [class.is-project]="isProject()"
      [class.is-selected]="selected()"
      [attr.aria-pressed]="selected()"
      [attr.aria-label]="
        entity().name + ' — ' + kindLabel() + ', ' + countLabel()
      "
      (click)="select.emit(entity().id)"
    >
      <span class="ec-dot" aria-hidden="true"></span>
      <span class="ec-name">{{ entity().name }}</span>
      <span class="ec-kind" aria-hidden="true">{{ kindLabel() }}</span>
      <span class="count ec-count" [attr.title]="countLabel()">
        {{ entity().mentionCount }}
      </span>
    </button>
  `,
  styles: [
    `
      :host {
        display: block;
      }
      .ec {
        display: grid;
        grid-template-columns: auto 1fr auto auto;
        align-items: center;
        gap: var(--space-3);
        width: 100%;
        padding: var(--space-3) var(--space-3) var(--space-3) var(--space-4);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-md);
        background: var(--surface-raised);
        color: var(--text-primary);
        font-family: inherit;
        font-size: 0.9375rem;
        text-align: left;
        cursor: pointer;
        transition:
          background var(--transition),
          border-color var(--transition),
          transform var(--transition-fast),
          box-shadow var(--transition);
      }
      .ec:hover {
        background: var(--surface-hover);
        border-color: var(--border-strong);
        transform: translateY(-1px);
      }
      .ec:active {
        transform: translateY(0);
      }
      .ec:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .ec.is-selected {
        border-color: transparent;
        background: var(--accent-soft);
        box-shadow:
          inset 0 0 0 1px var(--accent-ring),
          var(--glass-highlight);
      }
      .ec-dot {
        flex: none;
        width: 9px;
        height: 9px;
        border-radius: var(--radius-pill);
        background: var(--accent);
        box-shadow: 0 0 0 3px var(--accent-soft);
      }
      .ec.is-project .ec-dot {
        background: #9d7bff;
        box-shadow: 0 0 0 3px rgba(157, 123, 255, 0.18);
      }
      .ec-name {
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        font-weight: 550;
        letter-spacing: -0.01em;
      }
      .ec-kind {
        flex: none;
        color: var(--text-muted);
        font-size: 0.6875rem;
        font-weight: 600;
        letter-spacing: 0.06em;
        text-transform: uppercase;
      }
      .ec-count {
        flex: none;
      }
    `,
  ],
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
