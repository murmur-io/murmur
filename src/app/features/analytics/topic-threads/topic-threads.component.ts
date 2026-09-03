import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  inject,
  signal,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import { IpcService } from "../../../core/ipc.service";
import { TopicThreadsStore } from "../../../services/topic-threads-store.service";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";
import { DateFormatService } from "../../../core/date-format.service";

/**
 * "Topic threads" — cross-meeting topic clusters surfaced on the Analytics
 * screen. Each thread is a label that recurs across the user's cached meeting
 * timelines, with the meetings (mentions) where it came up.
 *
 * It is a presentational sibling of the analytics dashboard cards: the parent
 * owns the page; this component owns only the threads it loads via
 * {@link IpcService.topicThreads}. Threads come from cached timelines — they
 * only appear once a few meetings have been opened (their timelines generated).
 *
 * Lives in its own file so its inline styles get their own per-component
 * `anyComponentStyle` budget (the analytics component's styles are near the cap).
 *
 * Labels are model-derived text rendered as PLAIN TEXT (no markdown lib, no
 * innerHTML/DomSanitizer) — Angular interpolation escapes them safely.
 */
@Component({
  selector: "app-topic-threads",
  imports: [RouterLink],
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./topic-threads.component.html",
  styleUrl: "./topic-threads.component.scss",
})
export class TopicThreadsComponent implements OnInit {
  private readonly dates = inject(DateFormatService);

  private readonly ipc = inject(IpcService);
  private readonly store = inject(TopicThreadsStore);
  private readonly errorCopy = inject(ErrorCopyService);

  /**
   * All loaded threads, sorted multi-mention first (most-discussed topics up
   * top). Root-persisted so a navigate-away-and-back shows the LAST-KNOWN
   * threads instantly instead of blanking to "Loading…" — see
   * `TopicThreadsStore`.
   */
  readonly threads = this.store.threads;
  /** True while {@link IpcService.topicThreads} is in flight. */
  readonly loading = this.store.loading;
  /** Inline error message; null when clear. */
  readonly error = this.store.error;

  /** Labels of the currently-expanded threads. */
  private readonly openLabels = signal<ReadonlySet<string>>(new Set());

  async ngOnInit(): Promise<void> {
    try {
      const raw = await this.ipc.topicThreads();
      // Sort: most-mentioned threads first, then alphabetically for stability.
      const sorted = [...raw].sort(
        (a, b) => b.count - a.count || a.label.localeCompare(b.label),
      );
      this.threads.set(sorted);
    } catch (e) {
      this.error.set(this.errorCopy.because("Couldn’t load topic threads", e));
    } finally {
      this.loading.set(false);
    }
  }

  /** Whether a given thread is expanded. */
  isOpen(label: string): boolean {
    return this.openLabels().has(label);
  }

  /** Expand/collapse a thread's mention list. */
  toggle(label: string): void {
    this.openLabels.update((cur) => {
      const next = new Set(cur);
      if (next.has(label)) {
        next.delete(label);
      } else {
        next.add(label);
      }
      return next;
    });
  }

  /** Presentational only: render a stored timestamp as a friendly local date. */
  /** Formatted through {@link DateFormatService} — the one place a date becomes user-visible text. */
  formatDate(startedAt: string): string {
    return this.dates.day(startedAt);
  }

  /** Presentational only: seconds offset → "m:ss" mention stamp. */
  formatStamp(startS: number): string {
    const total = Math.max(0, Math.round(startS || 0));
    const m = Math.floor(total / 60);
    const s = total % 60;
    return `${m}:${String(s).padStart(2, "0")}`;
  }
}
