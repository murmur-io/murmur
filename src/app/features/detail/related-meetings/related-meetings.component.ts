import {
  ChangeDetectionStrategy,
  Component,
  effect,
  inject,
  input,
  output,
  signal,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import type { SearchHit } from "../../../core/models";

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
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./related-meetings.component.html",
  styleUrl: "./related-meetings.component.scss",
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
