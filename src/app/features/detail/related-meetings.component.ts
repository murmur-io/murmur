import {
  ChangeDetectionStrategy,
  Component,
  effect,
  inject,
  input,
  output,
  signal,
} from "@angular/core";
import { IpcService } from "../../core/ipc.service";
import type { SearchHit } from "../../core/models";

/**
 * "Powiązane wg znaczenia" — the semantic-related-meetings section shown at the
 * bottom of a meeting detail. Loads the meeting's semantic neighbors via the
 * gated `related_meetings` IPC command and renders each as a clickable row
 * (title + date + snippet). Navigation is owned by the parent: clicking a row
 * emits the neighbor's meeting id through {@link open}.
 *
 * The section is SILENT when there are no neighbors — when `related()` is empty
 * (feature flag off, no model, or no semantic neighbors) nothing renders. The
 * backend gates content, so a sealed/not-unlocked neighbor never reaches here.
 *
 * The IPC call is a one-shot awaited promise, loaded inside an effect that
 * tracks the `meetingId` input (mirrors `entity-detail.component.ts`), with a
 * stale-result guard that drops responses arriving after the id moved on.
 */
@Component({
  selector: "app-related-meetings",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (related().length) {
      <section class="rel" aria-label="Powiązane wg znaczenia">
        <h3 class="rel-title">Powiązane wg znaczenia</h3>
        <ul class="rel-list">
          @for (r of related(); track r.meeting.id) {
            <li>
              <button type="button" class="rel-row" (click)="open.emit(r.meeting.id)">
                <span class="rel-head">
                  <span class="rel-name">{{
                    r.meeting.title || "(untitled)"
                  }}</span>
                  <span class="rel-date">{{ fmtDate(r.meeting.startedAt) }}</span>
                </span>
                @if (r.snippet) {
                  <span class="rel-snippet">{{ r.snippet }}</span>
                }
              </button>
            </li>
          }
        </ul>
      </section>
    }
  `,
  styles: [
    `
      :host {
        display: block;
      }
      .rel {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .rel-title {
        margin: 0;
        font-size: 1rem;
      }
      .rel-list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .rel-row {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        width: 100%;
        padding: var(--space-3) var(--space-4);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        color: var(--text-primary);
        font-family: inherit;
        text-align: left;
        cursor: pointer;
        transition:
          background var(--transition),
          border-color var(--transition);
      }
      .rel-row:hover {
        background: var(--surface-hover);
        border-color: var(--border-strong);
      }
      .rel-row:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .rel-head {
        display: flex;
        align-items: baseline;
        gap: var(--space-3);
      }
      .rel-name {
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        font-weight: 600;
        font-size: 0.9375rem;
      }
      .rel-date {
        flex: none;
        margin-left: auto;
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-size: 0.75rem;
        font-variant-numeric: tabular-nums;
      }
      .rel-snippet {
        color: var(--text-secondary);
        font-size: 0.8125rem;
        line-height: 1.5;
        display: -webkit-box;
        -webkit-line-clamp: 2;
        -webkit-box-orient: vertical;
        overflow: hidden;
      }
    `,
  ],
})
export class RelatedMeetingsComponent {
  private readonly ipc = inject(IpcService);

  /** The meeting whose neighbors to show; changing it re-loads the list. */
  readonly meetingId = input<string | null>(null);
  /** Emits the picked neighbor's meeting id — the parent owns navigation. */
  readonly open = output<string>();

  readonly related = signal<SearchHit[]>([]);
  readonly loading = signal(false);

  /**
   * Re-load the neighbors whenever `meetingId` changes. Sets `loading`
   * synchronously before the await, so signal writes must be permitted (T1 /
   * NG0600). The stale-result guard drops a response that resolves after the
   * id moved on (fast meeting-to-meeting pivots).
   */
  private readonly _load = effect(
    () => {
      const id = this.meetingId();
      this.loading.set(true);
      void this.fetch(id);
    },
    { allowSignalWrites: true },
  );

  private async fetch(id: string | null): Promise<void> {
    if (!id) {
      this.related.set([]);
      this.loading.set(false);
      return;
    }
    try {
      const result = await this.ipc.relatedMeetings(id);
      if (this.meetingId() !== id) {
        return;
      }
      this.related.set(result);
    } catch {
      // Silent feature: a failure (e.g. command not registered) just hides it.
      if (this.meetingId() !== id) {
        return;
      }
      this.related.set([]);
    } finally {
      if (this.meetingId() === id) {
        this.loading.set(false);
      }
    }
  }

  protected fmtDate(iso: string): string {
    const d = new Date(iso);
    return isNaN(d.getTime())
      ? ""
      : d.toLocaleDateString(undefined, {
          year: "numeric",
          month: "short",
          day: "numeric",
        });
  }
}
