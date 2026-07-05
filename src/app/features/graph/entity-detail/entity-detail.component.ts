import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import type { EntityDetail, EntityNeighbor } from "../../../core/models";
import { SourcesComponent } from "../../../shared/sources/sources.component";
import { EntityNeighborhoodComponent } from "../entity-neighborhood/entity-neighborhood.component";

/**
 * The right-hand detail panel for one selected entity. Loads its
 * {@link EntityDetail} via IPC (re-loading whenever the `entityId` input
 * changes), then renders: a header (name + kind chip), the bounded
 * neighborhood SVG, the backlinked meetings as reusable `app-sources` chips
 * (→ /meeting/:id), and a neighbors list where each row re-selects that entity.
 *
 * Loading/error/empty are all handled honestly. The IPC call is a one-shot
 * awaited promise (not a data stream), so it's loaded imperatively inside an
 * effect that tracks the input signal — no `subscribe`, no markForCheck.
 */
@Component({
  selector: "app-entity-detail",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [SourcesComponent, EntityNeighborhoodComponent],
  templateUrl: "./entity-detail.component.html",
  styleUrl: "./entity-detail.component.scss",
})
export class EntityDetailComponent {
  private readonly ipc = inject(IpcService);

  /** The entity to show detail for; changing it re-loads the panel. */
  readonly entityId = input.required<string>();
  /** Emits when the user picks a neighbor — the container re-selects it. */
  readonly select = output<string>();
  /** Emits when the user dismisses the panel. */
  readonly close = output<void>();

  readonly detail = signal<EntityDetail | null>(null);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

  protected readonly meetingCountLabel = computed(() => {
    const n = this.detail()?.meetings.length ?? 0;
    return n === 1 ? "1 meeting" : `${n} meetings`;
  });

  /**
   * Re-load the detail whenever `entityId` changes. The IPC call is a one-shot
   * promise, so we await it inside an effect that tracks the input signal — a
   * stale-result guard drops responses that resolve after the id moved on
   * (fast neighbor-to-neighbor pivots), so the panel never shows mismatched data.
   */
  private readonly _load = effect(
    () => {
      const id = this.entityId();
      this.loading.set(true);
      this.error.set(null);
      void this.fetch(id);
    },
    // Sets loading/error synchronously inside the tracked effect, so writes
    // must be permitted here.
    { allowSignalWrites: true },
  );

  private async fetch(id: string): Promise<void> {
    try {
      const result = await this.ipc.getEntityDetail(id);
      // Guard against an out-of-order resolution (the selection moved on).
      if (this.entityId() !== id) {
        return;
      }
      this.detail.set(result);
    } catch (e) {
      if (this.entityId() !== id) {
        return;
      }
      this.detail.set(null);
      this.error.set(String(e));
    } finally {
      if (this.entityId() === id) {
        this.loading.set(false);
      }
    }
  }

  protected sharedLabel(nb: EntityNeighbor): string {
    return nb.sharedMeetings === 1
      ? "1 shared meeting"
      : `${nb.sharedMeetings} shared meetings`;
  }
}
