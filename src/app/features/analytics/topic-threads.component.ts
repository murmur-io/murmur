import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  inject,
  signal,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import { IpcService } from "../../core/ipc.service";
import type { TopicThread } from "../../core/models";

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
  standalone: true,
  imports: [RouterLink],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="card panel">
      <div class="panel-head">
        <h3>Topic threads</h3>
        @if (!loading() && threads().length) {
          <span class="panel-note">{{ threads().length }} tracked</span>
        }
      </div>

      @if (loading()) {
        <p class="empty">Loading…</p>
      } @else if (error()) {
        <div class="threads-error" role="alert">{{ error() }}</div>
      } @else if (threads().length === 0) {
        <div class="threads-hint">
          <span class="threads-hint-mark" aria-hidden="true"></span>
          <p class="empty">Open a few meetings to build topic threads</p>
        </div>
      } @else {
        <ul class="thread-list">
          @for (t of threads(); track t.label; let i = $index) {
            <li class="thread" [style.--i]="i">
              <button
                type="button"
                class="thread-head"
                [class.is-open]="isOpen(t.label)"
                [attr.aria-expanded]="isOpen(t.label)"
                (click)="toggle(t.label)"
              >
                <span class="thread-caret" aria-hidden="true"></span>
                <span class="thread-label">{{ t.label }}</span>
                <span
                  class="count thread-count"
                  [attr.aria-label]="
                    t.count + (t.count === 1 ? ' meeting' : ' meetings')
                  "
                >
                  {{ t.count }}
                </span>
              </button>

              @if (isOpen(t.label)) {
                <ul class="mention-list">
                  @for (m of t.mentions; track m.meetingId + "@" + m.startS) {
                    <li class="mention">
                      <a
                        class="mention-link"
                        [routerLink]="['/meeting', m.meetingId]"
                      >
                        <span class="mention-title">{{
                          m.title || "Untitled meeting"
                        }}</span>
                        <span class="mention-meta">
                          <span class="mention-date">{{
                            formatDate(m.startedAt)
                          }}</span>
                          <span class="mention-at">{{
                            formatStamp(m.startS)
                          }}</span>
                        </span>
                      </a>
                    </li>
                  }
                </ul>
              }
            </li>
          }
        </ul>
      }
    </div>
  `,
  styles: [
    `
      :host {
        display: block;
      }

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
        font-variant-numeric: tabular-nums;
      }

      /* --- Quiet hint / error --- */
      .threads-hint {
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: var(--space-2);
        padding: var(--space-5) var(--space-4);
        text-align: center;
      }
      .threads-hint-mark {
        width: 40px;
        height: 40px;
        border-radius: var(--radius-pill);
        background: var(--accent-soft);
        border: 1px solid var(--glass-border);
        box-shadow: var(--glass-highlight);
      }
      .threads-error {
        padding: var(--space-3);
        border: 1px solid rgba(255, 107, 107, 0.3);
        border-radius: var(--radius-md);
        background: var(--danger-soft);
        color: var(--text-primary);
        font-size: 0.875rem;
      }

      /* --- Thread list --- */
      .thread-list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .thread {
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        overflow: hidden;
        animation: rise 360ms var(--transition) both;
        animation-delay: calc(var(--i, 0) * 40ms);
      }

      .thread-head {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        width: 100%;
        padding: var(--space-3) var(--space-4);
        border: none;
        background: transparent;
        color: var(--text-primary);
        font-family: inherit;
        font-size: 0.9375rem;
        text-align: left;
        cursor: pointer;
        transition: background var(--transition);
      }
      .thread-head:hover {
        background: var(--surface-hover);
      }
      .thread-head:focus-visible {
        outline: none;
        box-shadow: inset 0 0 0 2px var(--accent-ring);
      }
      .thread-caret {
        flex: none;
        width: 8px;
        height: 8px;
        border-right: 1.6px solid var(--text-muted);
        border-bottom: 1.6px solid var(--text-muted);
        transform: rotate(-45deg);
        transition: transform var(--transition);
      }
      .thread-head.is-open .thread-caret {
        transform: rotate(45deg);
      }
      .thread-label {
        flex: 1 1 auto;
        min-width: 0;
        font-weight: 550;
        letter-spacing: -0.01em;
        overflow-wrap: anywhere;
      }
      .thread-count {
        flex: none;
      }

      /* --- Mentions (expanded) --- */
      .mention-list {
        list-style: none;
        margin: 0;
        padding: 0 var(--space-2) var(--space-2);
        display: flex;
        flex-direction: column;
        gap: 2px;
        animation: rise 240ms var(--transition) both;
      }
      .mention-link {
        display: flex;
        align-items: baseline;
        justify-content: space-between;
        gap: var(--space-3);
        padding: var(--space-2) var(--space-3);
        border-radius: var(--radius-sm);
        color: var(--text-secondary);
        transition:
          background var(--transition),
          color var(--transition);
      }
      .mention-link:hover {
        background: var(--surface-hover);
        color: var(--text-primary);
      }
      .mention-link:focus-visible {
        outline: none;
        box-shadow: 0 0 0 2px var(--accent-ring);
      }
      .mention-title {
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        font-size: 0.875rem;
        font-weight: 500;
      }
      .mention-meta {
        flex: none;
        display: inline-flex;
        align-items: baseline;
        gap: var(--space-2);
        color: var(--text-muted);
        font-size: 0.75rem;
        white-space: nowrap;
      }
      .mention-at {
        font-family: var(--font-mono);
        font-variant-numeric: tabular-nums;
      }

      @media (prefers-reduced-motion: reduce) {
        .panel,
        .thread,
        .mention-list {
          animation: none;
        }
      }
    `,
  ],
})
export class TopicThreadsComponent implements OnInit {
  private readonly ipc = inject(IpcService);

  /** All loaded threads, sorted multi-mention first (most-discussed topics up top). */
  readonly threads = signal<TopicThread[]>([]);
  /** True while {@link IpcService.topicThreads} is in flight. */
  readonly loading = signal(true);
  /** Inline error message; null when clear. */
  readonly error = signal<string | null>(null);

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
      this.error.set("Couldn’t load topic threads: " + String(e));
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
  formatDate(startedAt: string): string {
    const d = new Date(startedAt);
    if (Number.isNaN(d.getTime())) {
      return startedAt;
    }
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  }

  /** Presentational only: seconds offset → "m:ss" mention stamp. */
  formatStamp(startS: number): string {
    const total = Math.max(0, Math.round(startS || 0));
    const m = Math.floor(total / 60);
    const s = total % 60;
    return `${m}:${String(s).padStart(2, "0")}`;
  }
}
