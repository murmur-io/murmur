import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
  signal,
} from "@angular/core";
import { RouterLink } from "@angular/router";

import type { VaultSource } from "../core/models";

/**
 * Collapsible list of source meetings (for Ask / Brief). Shows the first `limit` as chips that
 * route to the meeting, with a "+N more" toggle for the rest.
 */
@Component({
  selector: "app-sources",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  template: `
    @if (sources().length) {
      <div class="src">
        <span class="src-label">Sources</span>
        @for (s of visible(); track s.meetingId) {
          <a class="src-chip" [routerLink]="['/meeting', s.meetingId]">
            <span class="src-dot"></span>
            <span class="src-title">{{ s.title }}</span>
            <span class="src-date">{{ fmt(s.startedAt) }}</span>
          </a>
        }
        @if (sources().length > limit()) {
          <button type="button" class="src-more" (click)="toggle()">
            {{
              expanded()
                ? "Show less"
                : "+" + (sources().length - limit()) + " more"
            }}
          </button>
        }
      </div>
    }
  `,
  styles: [
    `
      :host {
        display: block;
      }
      .src {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-2);
      }
      .src-label {
        font-size: 11px;
        font-weight: 600;
        letter-spacing: 0.08em;
        text-transform: uppercase;
        color: var(--text-muted);
        margin-right: var(--space-1);
      }
      .src-chip {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        max-width: 22rem;
        padding: 6px 12px;
        background: var(--surface-raised);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-pill);
        color: var(--text-secondary);
        text-decoration: none;
        font-size: 13px;
        transition:
          border-color var(--transition-fast),
          color var(--transition-fast),
          background var(--transition-fast);
      }
      .src-chip:hover {
        border-color: var(--accent-ring);
        color: var(--text-primary);
        background: var(--surface-hover);
      }
      .src-dot {
        flex: 0 0 auto;
        width: 6px;
        height: 6px;
        border-radius: var(--radius-pill);
        background: var(--accent);
      }
      .src-title {
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .src-date {
        flex: 0 0 auto;
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-size: 11px;
        font-variant-numeric: tabular-nums;
      }
      .src-more {
        padding: 6px 12px;
        background: transparent;
        border: 1px dashed var(--border-strong);
        border-radius: var(--radius-pill);
        color: var(--accent-hover);
        font-size: 12px;
        font-weight: 600;
        cursor: pointer;
        transition: background var(--transition-fast);
      }
      .src-more:hover {
        background: var(--accent-soft);
      }
    `,
  ],
})
export class SourcesComponent {
  readonly sources = input<VaultSource[]>([]);
  readonly limit = input<number>(4);
  readonly expanded = signal(false);

  readonly visible = computed(() =>
    this.expanded() ? this.sources() : this.sources().slice(0, this.limit()),
  );

  toggle(): void {
    this.expanded.update((v) => !v);
  }

  fmt(iso: string): string {
    const d = new Date(iso);
    return isNaN(d.getTime())
      ? ""
      : d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  }
}
