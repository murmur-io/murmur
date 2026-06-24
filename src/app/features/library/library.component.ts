import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  inject,
  signal,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import { IpcService } from "../../core/ipc.service";
import type { Meeting, MeetingStatus } from "../../core/models";

@Component({
  selector: "app-library",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  template: `
    <section class="library">
      <header class="library-head">
        <h2>Meetings</h2>
        @if (!loading() && meetings().length > 0) {
          <span class="count">{{ meetings().length }}</span>
        }
      </header>

      @if (loading()) {
        <div class="card state-card">
          <p class="empty">Loading…</p>
        </div>
      } @else if (meetings().length === 0) {
        <div class="card empty-state">
          <span class="empty-mark" aria-hidden="true"></span>
          <p class="empty-title">No meetings yet</p>
          <p class="empty">Record one from the Record tab to see it here.</p>
        </div>
      } @else {
        <ul class="list card">
          @for (m of meetings(); track m.id; let i = $index) {
            <li>
              <a
                class="row"
                [routerLink]="['/meeting', m.id]"
                [style.animation-delay.ms]="i * 45"
              >
                <span class="row-main">
                  <span class="title">{{ m.title || "(untitled)" }}</span>
                  <span class="meta">
                    <span class="date">{{ formatDate(m.startedAt) }}</span>
                    @if (m.durationS > 0) {
                      <span class="dot" aria-hidden="true">·</span>
                      <span class="duration">{{
                        formatDuration(m.durationS)
                      }}</span>
                    }
                  </span>
                </span>
                <span class="row-aside">
                  <span class="pill" [class]="statusPillClass(m.status)">
                    <span class="pill-dot"></span>
                    {{ statusLabel(m.status) }}
                  </span>
                  <span class="chevron" aria-hidden="true">›</span>
                </span>
              </a>
            </li>
          }
        </ul>
      }
    </section>
  `,
  styles: [
    `
      .library {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
      }

      .library-head {
        display: flex;
        align-items: center;
        gap: var(--space-3);
      }
      .library-head h2 {
        margin: 0;
      }
      .count {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        min-width: 24px;
        height: 24px;
        padding: 0 var(--space-2);
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        border: 1px solid var(--border);
        color: var(--text-secondary);
        font-size: 0.8125rem;
        font-weight: 600;
        line-height: 1;
      }

      /* --- Meeting list --- */
      .list {
        list-style: none;
        padding: var(--space-2);
        margin: 0;
      }
      .row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-4);
        padding: var(--space-3) var(--space-4);
        border-radius: var(--radius-md);
        text-decoration: none;
        color: inherit;
        animation: rise 360ms var(--transition) both;
        transition:
          background var(--transition),
          transform var(--transition-fast);
      }
      .list li + li {
        border-top: 1px solid var(--border-subtle);
      }
      .row:hover {
        background: var(--surface-hover);
      }
      .row:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .row:active {
        transform: translateY(1px);
      }

      .row-main {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        min-width: 0;
      }
      .title {
        color: var(--text-primary);
        font-weight: 600;
        letter-spacing: -0.01em;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .meta {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        color: var(--text-muted);
        font-size: 0.8125rem;
      }
      .meta .dot {
        color: var(--text-muted);
      }
      .duration {
        font-variant-numeric: tabular-nums;
      }

      .row-aside {
        display: inline-flex;
        align-items: center;
        gap: var(--space-3);
        flex: none;
      }
      .chevron {
        color: var(--text-muted);
        font-size: 1.25rem;
        line-height: 1;
        transition:
          color var(--transition),
          transform var(--transition);
      }
      .row:hover .chevron {
        color: var(--text-secondary);
        transform: translateX(2px);
      }

      /* --- Empty / loading states --- */
      .state-card {
        padding: var(--space-6);
      }
      .empty-state {
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: var(--space-2);
        padding: var(--space-7) var(--space-5);
        text-align: center;
      }
      .empty-mark {
        width: 44px;
        height: 44px;
        margin-bottom: var(--space-2);
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        border: 1px solid var(--border);
      }
      .empty-title {
        margin: 0;
        color: var(--text-primary);
        font-weight: 600;
      }
      .empty {
        margin: 0;
        color: var(--text-muted);
      }
    `,
  ],
})
export class LibraryComponent implements OnInit {
  private readonly ipc = inject(IpcService);

  readonly meetings = signal<Meeting[]>([]);
  readonly loading = signal(true);

  async ngOnInit(): Promise<void> {
    try {
      this.meetings.set(await this.ipc.listMeetings());
    } finally {
      this.loading.set(false);
    }
  }

  statusLabel(s: string): string {
    return s.charAt(0) + s.slice(1).toLowerCase();
  }

  /** Maps a meeting status to a status-pill state modifier (matches Record). */
  statusPillClass(s: MeetingStatus): string {
    switch (s) {
      case "RECORDING":
      case "ERROR":
        return "is-danger";
      case "TRANSCRIBED":
      case "SUMMARIZED":
        return "is-accent";
      case "EXPORTED":
        return "is-success";
      default:
        return "";
    }
  }

  /** Presentational only: render the stored timestamp as a friendly local date. */
  formatDate(startedAt: string): string {
    const d = new Date(startedAt);
    if (Number.isNaN(d.getTime())) {
      return startedAt;
    }
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  }

  /** Presentational only: seconds → compact "Hh Mm" / "Mm Ss" / "Ss" duration. */
  formatDuration(durationS: number): string {
    const total = Math.max(0, Math.round(durationS));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    if (h > 0) {
      return `${h}h ${m}m`;
    }
    if (m > 0) {
      return `${m}m ${s}s`;
    }
    return `${s}s`;
  }
}
