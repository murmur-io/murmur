import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  computed,
  inject,
  signal,
} from "@angular/core";
import { IpcService } from "../../core/ipc.service";
import type { Analytics } from "../../core/models";
import { TopicThreadsComponent } from "./topic-threads.component";
import { WeeklyDigestComponent } from "./weekly-digest.component";

/** One rendered column of the 30-day activity chart (zero-filled for gaps). */
interface ChartBar {
  date: string; // YYYY-MM-DD
  count: number;
  durationS: number;
  /** Bar height as a 0–100 percentage of the busiest day. */
  pct: number;
  /** Tick label (e.g. "Jun 3"), present only on a sparse subset of bars. */
  tick: string | null;
}

/** A status row for the breakdown, pre-resolved to its pill modifier + share. */
interface StatusRow {
  status: string;
  label: string;
  count: number;
  pillClass: string;
  /** Bar width as a 0–100 percentage of the most common status. */
  pct: number;
}

const DAY_MS = 86_400_000;
const CHART_DAYS = 30;
const STATUS_ORDER = [
  "DRAFT",
  "RECORDING",
  "TRANSCRIBED",
  "SUMMARIZED",
  "EXPORTED",
  "ERROR",
];

@Component({
  selector: "app-analytics",
  standalone: true,
  imports: [TopicThreadsComponent, WeeklyDigestComponent],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <section class="analytics">
      <header class="head">
        <h2>Analytics</h2>
        @if (!loading() && !isEmpty()) {
          <span class="count">{{ data()!.totalMeetings }}</span>
        }
      </header>

      @if (loading()) {
        <div class="card state-card">
          <p class="empty">Loading…</p>
        </div>
      } @else if (isEmpty()) {
        <div class="card empty-state">
          <span class="empty-mark" aria-hidden="true"></span>
          <p class="empty-title">Nothing to measure yet</p>
          <p class="empty">
            Record your first meeting and your stats will appear here.
          </p>
        </div>
      } @else {
        <!-- Hero stat cards -->
        <div class="hero">
          @for (s of heroStats(); track s.label; let i = $index) {
            <div class="card stat" [style.animation-delay.ms]="i * 60">
              <span class="stat-label">{{ s.label }}</span>
              <span class="stat-value">{{ s.value }}</span>
              @if (s.sub) {
                <span class="stat-sub">{{ s.sub }}</span>
              }
            </div>
          }
        </div>

        <!-- 30-day activity chart -->
        <div class="card panel chart-panel" [style.animation-delay.ms]="260">
          <div class="panel-head">
            <h3>Activity</h3>
            <span class="panel-note">Last 30 days</span>
          </div>
          <div
            class="chart"
            role="img"
            aria-label="Meetings per day, last 30 days"
          >
            @for (b of chartBars(); track b.date) {
              <div class="chart-col">
                <div class="chart-track">
                  <div
                    class="chart-bar"
                    [class.is-zero]="b.count === 0"
                    [style.height.%]="b.count === 0 ? 0 : b.pct"
                  >
                    <span class="chart-tip">
                      {{ chartTip(b) }}
                    </span>
                  </div>
                </div>
              </div>
            }
          </div>
          <div class="chart-axis">
            @for (b of chartBars(); track b.date) {
              <span class="chart-axis-cell">
                @if (b.tick) {
                  <span class="chart-tick">{{ b.tick }}</span>
                }
              </span>
            }
          </div>
        </div>

        <div class="grid">
          <!-- Status breakdown -->
          <div class="card panel" [style.animation-delay.ms]="320">
            <div class="panel-head">
              <h3>By status</h3>
            </div>
            <ul class="status-list">
              @for (row of statusRows(); track row.status) {
                <li class="status-row">
                  <span class="pill" [class]="row.pillClass">
                    <span class="pill-dot"></span>
                    {{ row.label }}
                  </span>
                  <span class="status-track">
                    <span
                      class="status-fill"
                      [class]="row.pillClass"
                      [style.width.%]="row.pct"
                    ></span>
                  </span>
                  <span class="status-count">{{ row.count }}</span>
                </li>
              }
            </ul>
          </div>

          <!-- Small tiles -->
          <div class="tiles">
            <div class="card tile" [style.animation-delay.ms]="360">
              <span class="tile-label">Longest session</span>
              <span class="tile-value">
                {{ formatDuration(data()!.longestDurationS) }}
              </span>
            </div>
            <div class="card tile" [style.animation-delay.ms]="400">
              <span class="tile-label">Notes created</span>
              <span class="tile-value">{{ data()!.notesCount }}</span>
            </div>
            <div class="card tile" [style.animation-delay.ms]="440">
              <span class="tile-label">Recording since</span>
              <span class="tile-value tile-value--text">
                {{ recordingSince() }}
              </span>
            </div>
          </div>
        </div>

        <!-- Topic threads (cross-meeting clusters from cached timelines) -->
        <app-topic-threads />

        <!-- Weekly digest (on-demand synthesis of recent meetings) -->
        <app-weekly-digest />
      }
    </section>
  `,
  styles: [
    `
      .analytics {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
      }

      .head {
        display: flex;
        align-items: center;
        gap: var(--space-3);
      }
      .head h2 {
        margin: 0;
      }

      /* --- Hero stat cards --- */
      .hero {
        display: grid;
        grid-template-columns: repeat(4, 1fr);
        gap: var(--space-4);
      }
      .stat {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        padding: var(--space-5);
        animation: rise 420ms var(--transition) both;
        transition:
          transform var(--transition),
          border-color var(--transition),
          box-shadow var(--transition);
      }
      .stat:hover {
        transform: translateY(-2px);
        border-color: var(--border-strong);
        box-shadow: var(--shadow-lg), var(--glass-highlight);
      }
      .stat-label {
        color: var(--text-muted);
        font-size: 0.8125rem;
        font-weight: 550;
        letter-spacing: 0.01em;
        text-transform: uppercase;
      }
      .stat-value {
        color: var(--text-primary);
        font-family: var(--font-mono);
        font-size: 1.85rem;
        font-weight: 600;
        line-height: 1.05;
        letter-spacing: -0.03em;
        font-variant-numeric: tabular-nums;
      }
      .stat-sub {
        color: var(--text-secondary);
        font-size: 0.8125rem;
        font-variant-numeric: tabular-nums;
      }

      /* --- Panels --- */
      .panel {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
        animation: rise 420ms var(--transition) both;
      }
      .panel-head {
        display: flex;
        align-items: baseline;
        justify-content: space-between;
        gap: var(--space-3);
      }
      .panel-head h3 {
        margin: 0;
      }
      .panel-note {
        color: var(--text-muted);
        font-size: 0.8125rem;
        font-weight: 550;
        letter-spacing: 0.01em;
      }

      /* --- 30-day activity chart --- */
      .chart {
        display: flex;
        align-items: flex-end;
        gap: 3px;
        height: 132px;
      }
      .chart-col {
        flex: 1 1 0;
        height: 100%;
        min-width: 0;
      }
      .chart-track {
        position: relative;
        display: flex;
        align-items: flex-end;
        height: 100%;
      }
      .chart-bar {
        position: relative;
        width: 100%;
        min-height: 2px;
        border-radius: var(--radius-sm) var(--radius-sm) 3px 3px;
        background: var(--accent-gradient);
        box-shadow: 0 0 0 1px rgba(110, 118, 255, 0.18);
        transform-origin: bottom;
        animation: grow-bar 560ms var(--ease-spring) both;
        transition:
          filter var(--transition),
          box-shadow var(--transition);
      }
      .chart-bar.is-zero {
        min-height: 3px;
        height: 3px !important;
        background: var(--surface-hover);
        box-shadow: none;
        border-radius: var(--radius-pill);
        align-self: flex-end;
      }
      .chart-col:hover .chart-bar:not(.is-zero) {
        filter: brightness(1.15);
        box-shadow: var(--shadow-accent);
      }
      .chart-col:hover .chart-tip {
        opacity: 1;
        transform: translate(-50%, -6px);
      }
      @keyframes grow-bar {
        from {
          transform: scaleY(0);
        }
        to {
          transform: scaleY(1);
        }
      }

      .chart-tip {
        position: absolute;
        bottom: 100%;
        left: 50%;
        transform: translate(-50%, 0);
        padding: var(--space-1) var(--space-2);
        border-radius: var(--radius-sm);
        background: var(--surface-overlay);
        border: 1px solid var(--glass-border);
        box-shadow: var(--shadow-md);
        color: var(--text-primary);
        font-family: var(--font-mono);
        font-size: 0.75rem;
        font-variant-numeric: tabular-nums;
        line-height: 1.2;
        white-space: nowrap;
        opacity: 0;
        pointer-events: none;
        transition:
          opacity var(--transition),
          transform var(--transition);
        z-index: 2;
      }

      .chart-axis {
        display: flex;
        gap: 3px;
        margin-top: calc(-1 * var(--space-2));
      }
      .chart-axis-cell {
        flex: 1 1 0;
        min-width: 0;
        display: flex;
        justify-content: center;
      }
      .chart-tick {
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-size: 0.6875rem;
        letter-spacing: 0.01em;
        white-space: nowrap;
      }

      /* --- Two-column lower grid --- */
      .grid {
        display: grid;
        grid-template-columns: 1.4fr 1fr;
        gap: var(--space-4);
        align-items: start;
      }

      /* --- Status breakdown --- */
      .status-list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .status-row {
        display: grid;
        grid-template-columns: 116px 1fr auto;
        align-items: center;
        gap: var(--space-3);
      }
      .status-row .pill {
        justify-content: flex-start;
      }
      .status-track {
        position: relative;
        height: 8px;
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        overflow: hidden;
      }
      .status-fill {
        position: absolute;
        inset: 0 auto 0 0;
        height: 100%;
        min-width: 8px;
        border-radius: var(--radius-pill);
        background: var(--accent-gradient);
        animation: grow-fill 560ms var(--ease-spring) both;
        transform-origin: left;
      }
      /* Reuse the pill colour vocabulary for the fill, but as solid bars.
         (Accent reuses the base gradient; only the status colours override.) */
      .status-fill.is-success {
        background: var(--success);
      }
      .status-fill.is-danger {
        background: var(--danger);
      }
      @keyframes grow-fill {
        from {
          transform: scaleX(0);
        }
        to {
          transform: scaleX(1);
        }
      }
      .status-count {
        color: var(--text-primary);
        font-family: var(--font-mono);
        font-size: 0.9375rem;
        font-weight: 600;
        font-variant-numeric: tabular-nums;
        text-align: right;
      }

      /* --- Small tiles --- */
      .tiles {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .tile {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        padding: var(--space-4) var(--space-5);
        animation: rise 420ms var(--transition) both;
        transition:
          transform var(--transition),
          border-color var(--transition);
      }
      .tile:hover {
        transform: translateY(-2px);
        border-color: var(--border-strong);
      }
      .tile-label {
        color: var(--text-muted);
        font-size: 0.8125rem;
        font-weight: 550;
        letter-spacing: 0.01em;
        text-transform: uppercase;
      }
      .tile-value {
        color: var(--text-primary);
        font-family: var(--font-mono);
        font-size: 1.4rem;
        font-weight: 600;
        letter-spacing: -0.02em;
        font-variant-numeric: tabular-nums;
      }
      .tile-value--text {
        font-family: var(--font-sans);
        font-size: 1.0625rem;
        letter-spacing: -0.01em;
      }

      /* --- Empty / loading states (.count/.state-card/.empty* are global) --- */

      /* --- Responsive --- */
      @media (max-width: 720px) {
        .hero {
          grid-template-columns: repeat(2, 1fr);
        }
        .grid {
          grid-template-columns: 1fr;
        }
      }
    `,
  ],
})
export class AnalyticsComponent implements OnInit {
  private readonly ipc = inject(IpcService);

  readonly data = signal<Analytics | null>(null);
  readonly loading = signal(true);

  readonly isEmpty = computed(() => {
    const a = this.data();
    return !a || a.totalMeetings === 0;
  });

  /** The four hero figures, pre-formatted for the template. */
  readonly heroStats = computed(() => {
    const a = this.data();
    if (!a) {
      return [];
    }
    return [
      {
        label: "Total meetings",
        value: String(a.totalMeetings),
        sub: null as string | null,
      },
      {
        label: "Total time",
        value: this.formatDuration(a.totalDurationS),
        sub: null,
      },
      {
        label: "This week",
        value: String(a.meetings7d),
        sub:
          a.duration7dS > 0
            ? `${this.formatDuration(a.duration7dS)} recorded`
            : null,
      },
      {
        label: "Avg length",
        value: this.formatDuration(a.avgDurationS),
        sub: null,
      },
    ];
  });

  /** Last 30 calendar days, zero-filled, keyed by count for the bar heights. */
  readonly chartBars = computed<ChartBar[]>(() => {
    const a = this.data();
    if (!a) {
      return [];
    }

    const byDate = new Map<string, { count: number; durationS: number }>();
    for (const d of a.perDay) {
      byDate.set(d.date, { count: d.count, durationS: d.durationS });
    }

    // Build the contiguous window ending today (local time).
    const today = new Date();
    today.setHours(0, 0, 0, 0);
    const start = today.getTime() - (CHART_DAYS - 1) * DAY_MS;

    const days: { date: string; count: number; durationS: number }[] = [];
    for (let i = 0; i < CHART_DAYS; i++) {
      const date = this.isoDate(new Date(start + i * DAY_MS));
      const hit = byDate.get(date);
      days.push({
        date,
        count: hit?.count ?? 0,
        durationS: hit?.durationS ?? 0,
      });
    }

    const max = Math.max(1, ...days.map((d) => d.count));
    // Show a tick on the first day, the last day, and roughly weekly between.
    const tickEvery = 7;

    return days.map((d, i) => ({
      date: d.date,
      count: d.count,
      durationS: d.durationS,
      pct: Math.round((d.count / max) * 100),
      tick:
        i === 0 || i === days.length - 1 || i % tickEvery === 0
          ? this.tickLabel(d.date)
          : null,
    }));
  });

  /** Status rows in canonical order, only for statuses that actually occur. */
  readonly statusRows = computed<StatusRow[]>(() => {
    const a = this.data();
    if (!a) {
      return [];
    }

    const counts = new Map<string, number>();
    for (const s of a.byStatus) {
      counts.set(s.status, (counts.get(s.status) ?? 0) + s.count);
    }

    const max = Math.max(1, ...Array.from(counts.values()));

    // Honour the canonical order first, then append any unexpected statuses.
    const ordered = [
      ...STATUS_ORDER.filter((s) => counts.has(s)),
      ...Array.from(counts.keys()).filter((s) => !STATUS_ORDER.includes(s)),
    ];

    return ordered.map((status) => {
      const count = counts.get(status) ?? 0;
      return {
        status,
        label: this.statusLabel(status),
        count,
        pillClass: this.statusPillClass(status),
        pct: Math.round((count / max) * 100),
      };
    });
  });

  readonly recordingSince = computed(() => {
    const first = this.data()?.firstMeetingAt;
    if (!first) {
      return "—";
    }
    const d = new Date(first);
    if (Number.isNaN(d.getTime())) {
      return first;
    }
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  });

  async ngOnInit(): Promise<void> {
    try {
      this.data.set(await this.ipc.getAnalytics());
    } finally {
      this.loading.set(false);
    }
  }

  /** Hover tooltip text for a chart column. */
  chartTip(b: ChartBar): string {
    const label = b.count === 1 ? "1 meeting" : `${b.count} meetings`;
    const when = this.tickLabel(b.date);
    return `${when} · ${label}`;
  }

  statusLabel(s: string): string {
    return s.charAt(0) + s.slice(1).toLowerCase();
  }

  /** Maps a status to a pill state modifier (matches Library exactly). */
  statusPillClass(s: string): string {
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

  /** seconds → compact "1h 5m" / "12m" / "45s". Treats empty/zero gracefully. */
  formatDuration(durationS: number): string {
    const total = Math.max(0, Math.round(durationS || 0));
    if (total === 0) {
      return "0s";
    }
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    if (h > 0) {
      return m > 0 ? `${h}h ${m}m` : `${h}h`;
    }
    if (m > 0) {
      return `${m}m`;
    }
    return `${s}s`;
  }

  /** Local-time YYYY-MM-DD (matches the perDay key format). */
  private isoDate(d: Date): string {
    const y = d.getFullYear();
    const m = String(d.getMonth() + 1).padStart(2, "0");
    const day = String(d.getDate()).padStart(2, "0");
    return `${y}-${m}-${day}`;
  }

  /** "YYYY-MM-DD" → "Jun 3" axis/tooltip label. */
  private tickLabel(iso: string): string {
    const [y, m, d] = iso.split("-").map(Number);
    const date = new Date(y, (m ?? 1) - 1, d ?? 1);
    if (Number.isNaN(date.getTime())) {
      return iso;
    }
    return date.toLocaleDateString(undefined, {
      month: "short",
      day: "numeric",
    });
  }
}
