import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  computed,
  inject,
  signal,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import type { Analytics } from "../../../core/models";
import { EgressLedgerComponent } from "../egress-ledger/egress-ledger.component";
import { TopicThreadsComponent } from "../topic-threads/topic-threads.component";
import { WeeklyDigestComponent } from "../weekly-digest/weekly-digest.component";

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
  imports: [EgressLedgerComponent, TopicThreadsComponent, WeeklyDigestComponent],
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./analytics.component.html",
  styleUrl: "./analytics.component.scss",
})
export class AnalyticsComponent implements OnInit {
  private readonly ipc = inject(IpcService);

  readonly data = signal<Analytics | null>(null);
  readonly loading = signal(true);
  readonly error = signal<string | null>(null);

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
    await this.load();
  }

  /** (Re-)fetch analytics. Also the Retry button's handler on a load failure. */
  async load(): Promise<void> {
    this.loading.set(true);
    this.error.set(null);
    try {
      this.data.set(await this.ipc.getAnalytics());
    } catch (e) {
      this.data.set(null);
      this.error.set(String(e));
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
