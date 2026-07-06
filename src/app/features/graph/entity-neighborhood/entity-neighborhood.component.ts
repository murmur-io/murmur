import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
  output,
} from "@angular/core";
import type { EntityKind, EntityNeighbor } from "../../../core/models";

/** A neighbor resolved to its fixed position on the neighborhood ring. */
interface Satellite {
  id: string;
  name: string;
  kind: EntityKind;
  sharedMeetings: number;
  /** Centre coordinates of the satellite node, in SVG user units. */
  cx: number;
  cy: number;
  /** Label anchor — flips side so text never overruns the viewBox edge. */
  labelX: number;
  textAnchor: "start" | "end" | "middle";
  /** Edge stroke width ∝ shared-meeting count (clamped). */
  strokeWidth: number;
}

/** Square SVG canvas (user units); the hub sits dead-centre. */
const SIZE = 320;
const CENTER = SIZE / 2;
/** Ring radius for the satellites — leaves room for labels at the rim. */
const RADIUS = 104;
/** Hard cap on rendered satellites (bounded decoration, never a full graph). */
const MAX_SATELLITES = 12;
const SAT_R = 5.5;

/**
 * A bounded, single-entity "neighborhood" — pure decoration over the directory.
 *
 * The selected entity sits at the centre (reusing the brand wave glyph), and up
 * to {@link MAX_SATELLITES} co-occurring neighbors are laid out ONCE on a circle
 * via cos/sin inside a `computed()`. There is NO force simulation, NO
 * requestAnimationFrame/setInterval — positions are a pure function of the
 * neighbor list, recomputed only when that input changes. Edge thickness scales
 * with shared-meeting count. Clicking a satellite emits its id so the container
 * can re-select that entity. Degrades gracefully to a lone hub at zero neighbors
 * and honours `prefers-reduced-motion` (entrance only; nothing ever loops).
 */
@Component({
  selector: "app-entity-neighborhood",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./entity-neighborhood.component.html",
  styleUrl: "./entity-neighborhood.component.scss",
})
export class EntityNeighborhoodComponent {
  /** The co-occurring neighbors to lay out (top-K is capped internally). */
  readonly neighbors = input<EntityNeighbor[]>([]);
  /** Display name of the centred entity (for the SVG aria-label). */
  readonly centerName = input<string>("");
  /** Emits a neighbor's id when its satellite is activated. */
  readonly selected = output<string>();

  protected readonly size = SIZE;
  protected readonly center = CENTER;
  protected readonly satR = SAT_R;

  /**
   * Lay out up to {@link MAX_SATELLITES} neighbors on a circle, ONCE, via
   * cos/sin. Pure derivation of the input — no simulation, no animation frame.
   * Neighbors arrive sorted by the backend; we take the strongest top-K and
   * scale edge width to the shared-meeting count within this set.
   */
  protected readonly satellites = computed<Satellite[]>(() => {
    const all = this.neighbors();
    const top = all.slice(0, MAX_SATELLITES);
    const n = top.length;
    if (n === 0) {
      return [];
    }

    const maxShared = Math.max(1, ...top.map((s) => s.sharedMeetings));
    // Start at the top (12 o'clock) and step evenly clockwise.
    const step = (2 * Math.PI) / n;
    const start = -Math.PI / 2;

    return top.map((s, i) => {
      const angle = start + i * step;
      const cx = CENTER + RADIUS * Math.cos(angle);
      const cy = CENTER + RADIUS * Math.sin(angle);
      const cos = Math.cos(angle);
      // Place the label outboard of the node, flipping side around the rim.
      const onRight = cos > 0.2;
      const onLeft = cos < -0.2;
      const labelX = onRight ? cx + SAT_R + 6 : onLeft ? cx - SAT_R - 6 : cx;
      const textAnchor: Satellite["textAnchor"] = onRight
        ? "start"
        : onLeft
          ? "end"
          : "middle";
      // Map shared count → 1.2…5 px stroke; +0 baseline keeps thin edges visible.
      const strokeWidth = 1.2 + (s.sharedMeetings / maxShared) * 3.8;

      return {
        id: s.id,
        name: s.name,
        kind: s.kind,
        sharedMeetings: s.sharedMeetings,
        cx,
        cy,
        labelX,
        textAnchor,
        strokeWidth: Math.round(strokeWidth * 100) / 100,
      };
    });
  });

  protected readonly ariaLabel = computed(() => {
    const name = this.centerName() || "This entity";
    const k = this.satellites().length;
    if (k === 0) {
      return `${name} has no connected entities in the visible graph.`;
    }
    return `${name} and its ${k} most-connected ${
      k === 1 ? "entity" : "entities"
    }.`;
  });

  protected satLabel(s: Satellite): string {
    const shared =
      s.sharedMeetings === 1
        ? "1 shared meeting"
        : `${s.sharedMeetings} shared meetings`;
    return `${s.name} — ${shared}. Open this entity.`;
  }

  /** Space activates the satellite without scrolling the page. */
  protected onSpace(event: Event, id: string): void {
    event.preventDefault();
    this.selected.emit(id);
  }
}
