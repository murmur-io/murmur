import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
  output,
} from "@angular/core";
import type { EntityKind, EntityNeighbor } from "../../core/models";

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
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <figure class="nb">
      <svg
        class="nb-svg"
        [attr.viewBox]="'0 0 ' + size + ' ' + size"
        role="img"
        [attr.aria-label]="ariaLabel()"
      >
        <defs>
          <linearGradient id="nbWave" x1="0" y1="0" x2="1" y2="1">
            <stop offset="0" stop-color="#6e76ff" />
            <stop offset="1" stop-color="#9d7bff" />
          </linearGradient>
          <radialGradient id="nbHubGlow" cx="50%" cy="50%" r="50%">
            <stop offset="0" stop-color="rgba(110,118,255,0.35)" />
            <stop offset="1" stop-color="rgba(110,118,255,0)" />
          </radialGradient>
        </defs>

        <!-- Edges first, under the nodes. -->
        <g class="nb-edges" aria-hidden="true">
          @for (s of satellites(); track s.id) {
            <line
              class="nb-edge"
              [attr.x1]="center"
              [attr.y1]="center"
              [attr.x2]="s.cx"
              [attr.y2]="s.cy"
              [attr.stroke-width]="s.strokeWidth"
              [style.--i]="$index"
            />
          }
        </g>

        <!-- Soft hub glow + the centred brand wave. -->
        <circle
          [attr.cx]="center"
          [attr.cy]="center"
          r="46"
          fill="url(#nbHubGlow)"
          aria-hidden="true"
        />
        <g
          class="nb-hub"
          [attr.transform]="
            'translate(' + (center - 14) + ',' + (center - 14) + ')'
          "
          aria-hidden="true"
        >
          <rect
            class="nb-bar b1"
            x="4.4"
            y="10"
            width="2.4"
            height="8"
            rx="1.2"
          />
          <rect
            class="nb-bar b2"
            x="8.4"
            y="7"
            width="2.4"
            height="14"
            rx="1.2"
          />
          <rect
            class="nb-bar b3"
            x="12.4"
            y="4"
            width="2.4"
            height="20"
            rx="1.2"
          />
          <rect
            class="nb-bar b4"
            x="16.4"
            y="7"
            width="2.4"
            height="14"
            rx="1.2"
          />
          <rect
            class="nb-bar b5"
            x="20.4"
            y="10"
            width="2.4"
            height="8"
            rx="1.2"
          />
        </g>

        <!-- Satellites: each a clickable group (node + label). -->
        <g class="nb-sats">
          @for (s of satellites(); track s.id) {
            <g
              class="nb-sat"
              [class.is-project]="s.kind === 'project'"
              role="button"
              tabindex="0"
              [attr.aria-label]="satLabel(s)"
              [style.--i]="$index"
              (click)="select.emit(s.id)"
              (keydown.enter)="select.emit(s.id)"
              (keydown.space)="onSpace($event, s.id)"
            >
              <circle
                class="nb-sat-hit"
                [attr.cx]="s.cx"
                [attr.cy]="s.cy"
                r="16"
              />
              <circle
                class="nb-sat-dot"
                [attr.cx]="s.cx"
                [attr.cy]="s.cy"
                [attr.r]="satR"
              />
              <text
                class="nb-sat-label"
                [attr.x]="s.labelX"
                [attr.y]="s.cy + 4"
                [attr.text-anchor]="s.textAnchor"
              >
                {{ s.name }}
              </text>
            </g>
          }
        </g>
      </svg>

      @if (satellites().length === 0) {
        <figcaption class="nb-empty">
          No connections yet — this entity stands alone in your visible graph.
        </figcaption>
      } @else {
        <figcaption class="nb-cap">
          Edge thickness reflects how many meetings they share.
        </figcaption>
      }
    </figure>
  `,
  styles: [
    `
      :host {
        display: block;
      }
      .nb {
        margin: 0;
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: var(--space-2);
      }
      .nb-svg {
        display: block;
        width: 100%;
        max-width: 340px;
        height: auto;
        overflow: visible;
      }

      /* Hub brand wave. */
      .nb-bar {
        fill: url(#nbWave);
      }

      /* Edges grow in from the hub on entrance only — never loop. */
      .nb-edge {
        stroke: var(--border-strong);
        opacity: 0;
        stroke-linecap: round;
        animation: nb-edge-in 360ms var(--transition) both;
        animation-delay: calc(var(--i, 0) * 40ms + 80ms);
        transition: stroke var(--transition);
      }

      .nb-sat {
        cursor: pointer;
        animation: nb-sat-in 360ms var(--ease-spring) both;
        animation-delay: calc(var(--i, 0) * 40ms + 120ms);
      }
      .nb-sat-hit {
        fill: transparent;
      }
      .nb-sat-dot {
        fill: var(--accent);
        stroke: var(--surface-base);
        stroke-width: 2;
        transition:
          r var(--transition-fast),
          fill var(--transition);
      }
      .nb-sat.is-project .nb-sat-dot {
        fill: #9d7bff;
      }
      .nb-sat-label {
        fill: var(--text-secondary);
        font-family: var(--font-sans);
        font-size: 11px;
        font-weight: 550;
        pointer-events: none;
        transition: fill var(--transition);
      }
      .nb-sat:hover .nb-sat-dot,
      .nb-sat:focus-visible .nb-sat-dot {
        r: 7.5;
      }
      .nb-sat:hover .nb-sat-label,
      .nb-sat:focus-visible .nb-sat-label {
        fill: var(--text-primary);
      }
      .nb-sat:focus-visible {
        outline: none;
      }
      .nb-sat:focus-visible .nb-sat-dot {
        stroke: var(--accent-hover);
        stroke-width: 2.5;
      }

      .nb-cap,
      .nb-empty {
        margin: 0;
        max-width: 32ch;
        text-align: center;
        color: var(--text-muted);
        font-size: 0.75rem;
        line-height: 1.45;
      }

      @keyframes nb-edge-in {
        from {
          opacity: 0;
        }
        to {
          opacity: 0.7;
        }
      }
      @keyframes nb-sat-in {
        from {
          opacity: 0;
          transform: scale(0.6);
          transform-origin: center;
        }
        to {
          opacity: 1;
          transform: scale(1);
        }
      }
      @media (prefers-reduced-motion: reduce) {
        .nb-edge,
        .nb-sat {
          animation: none;
          opacity: 1;
        }
        .nb-edge {
          opacity: 0.7;
        }
      }
    `,
  ],
})
export class EntityNeighborhoodComponent {
  /** The co-occurring neighbors to lay out (top-K is capped internally). */
  readonly neighbors = input<EntityNeighbor[]>([]);
  /** Display name of the centred entity (for the SVG aria-label). */
  readonly centerName = input<string>("");
  /** Emits a neighbor's id when its satellite is activated. */
  readonly select = output<string>();

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
    this.select.emit(id);
  }
}
