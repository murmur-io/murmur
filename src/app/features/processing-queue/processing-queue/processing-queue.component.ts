import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  computed,
  inject,
  signal,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import type {
  ProcessingQueueItem,
  ProcessingQueueState,
} from "../../../core/models";
import { MurIconComponent } from "../../../design-system/icon/icon.component";
import { MurSpinnerComponent } from "../../../design-system/spinner/spinner.component";
import { ProcessingQueueStore } from "../../../services/processing-queue.store";
import { RecorderStore } from "../../../core/recorder.store";

type QueueFilter = "all" | "queued" | "processing" | "failed" | "held";

interface QueueViewRow {
  readonly id: string;
  readonly title: string;
  readonly state: ProcessingQueueState | "held";
  readonly stateLabel: string;
  readonly detailLabel: string;
  readonly selected: boolean;
  readonly runnable: boolean;
  readonly retryable: boolean;
  readonly removable: boolean;
  readonly canMoveUp: boolean;
  readonly canMoveDown: boolean;
}

function failureCopy(code: string | null): string {
  switch (code) {
    case "cloud-consent":
      return "Cloud processing needs permission";
    case "provider-unavailable":
      return "Processing provider unavailable";
    case "model-missing":
      return "Required model is not available";
    default:
      return "Processing needs attention";
  }
}

@Component({
  selector: "app-processing-queue",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink, MurIconComponent, MurSpinnerComponent],
  templateUrl: "./processing-queue.component.html",
  styleUrl: "./processing-queue.component.scss",
})
export class ProcessingQueueComponent implements OnInit {
  readonly store = inject(ProcessingQueueStore);
  private readonly recorder = inject(RecorderStore);
  readonly filter = signal<QueueFilter>("all");
  private readonly selectedIds = signal<ReadonlySet<string>>(new Set());

  readonly counts = computed(() => {
    const items = this.store.items();
    return {
      all: items.length,
      queued: items.filter((item) => item.state === "queued" && item.locked === false)
        .length,
      processing: items.filter(
        (item) => item.state === "processing" && item.locked === false,
      ).length,
      failed: items.filter((item) => item.state === "failed" && item.locked === false)
        .length,
      held: items.filter((item) => item.locked !== false).length,
    };
  });

  readonly rows = computed<QueueViewRow[]>(() => {
    const all = this.store.items();
    const reorderableIds = all
      .filter((item) => item.locked === false && item.state === "queued")
      .map((item) => item.meetingId);
    const reorderIndex = new Map(
      reorderableIds.map((id, index) => [id, index] as const),
    );
    const activeFilter = this.filter();
    const selected = this.selectedIds();
    return all
      .filter((item) => {
        if (activeFilter === "all") return true;
        if (activeFilter === "held") return item.locked !== false;
        return item.locked === false && item.state === activeFilter;
      })
      .map((item) =>
        this.toViewRow(
          item,
          reorderIndex.get(item.meetingId) ?? -1,
          reorderableIds.length,
          selected,
        ),
      );
  });

  readonly selectedCount = computed(() => this.selectedIds().size);
  readonly actionsDisabled = computed(
    () =>
      this.store.mutating() ||
      this.recorder.isRecording() ||
      this.store.running(),
  );
  readonly selectedRunnableIds = computed(() =>
    this.store
      .items()
      .filter(
        (item) =>
          this.selectedIds().has(item.meetingId) &&
          item.locked === false &&
          item.state === "queued",
      )
      .map((item) => item.meetingId),
  );
  readonly selectedRetryIds = computed(() =>
    this.store
      .items()
      .filter(
        (item) =>
          this.selectedIds().has(item.meetingId) &&
          item.locked === false &&
          item.state === "failed",
      )
      .map((item) => item.meetingId),
  );
  readonly selectedRemovableIds = computed(() =>
    this.store
      .items()
      .filter(
        (item) =>
          this.selectedIds().has(item.meetingId) &&
          item.locked === false &&
          item.state !== "processing",
      )
      .map((item) => item.meetingId),
  );

  ngOnInit(): void {
    void this.store.init();
  }

  setFilter(filter: QueueFilter): void {
    this.filter.set(filter);
  }

  toggleSelected(id: string, checked: boolean): void {
    this.selectedIds.update((current) => {
      const next = new Set(current);
      if (checked) next.add(id);
      else next.delete(id);
      return next;
    });
  }

  clearSelection(): void {
    this.selectedIds.set(new Set());
  }

  async run(ids: string[]): Promise<void> {
    if (await this.store.processNow(ids)) this.clearSelection();
  }

  async retry(ids: string[]): Promise<void> {
    if (await this.store.retry(ids)) this.clearSelection();
  }

  async remove(ids: string[]): Promise<void> {
    if (await this.store.remove(ids)) this.clearSelection();
  }

  move(id: string, direction: -1 | 1): void {
    const items = this.store.items();
    const ordered = items.map((item) => item.meetingId);
    const movable = items
      .filter((item) => item.locked === false && item.state === "queued")
      .map((item) => item.meetingId);
    const movableIndex = movable.indexOf(id);
    const targetMovableIndex = movableIndex + direction;
    if (
      movableIndex < 0 ||
      targetMovableIndex < 0 ||
      targetMovableIndex >= movable.length
    ) return;
    const index = ordered.indexOf(id);
    const target = ordered.indexOf(movable[targetMovableIndex]);
    [ordered[index], ordered[target]] = [ordered[target], ordered[index]];
    void this.store.reorder(ordered);
  }

  private toViewRow(
    item: ProcessingQueueItem,
    index: number,
    reorderableCount: number,
    selected: ReadonlySet<string>,
  ): QueueViewRow {
    const unlocked = item.locked === false;
    const state = unlocked ? item.state : "held";
    const labels: Record<QueueViewRow["state"], string> = {
      queued: "Waiting",
      processing: "Processing",
      failed: "Failed",
      held: "Held — locked",
    };
    let detailLabel = `Attempt ${item.attempts + 1}`;
    if (item.state === "processing") detailLabel = item.stage || "Working";
    if (item.state === "failed") detailLabel = failureCopy(item.lastErrorCode);
    if (!unlocked) detailLabel = "Unlock its folder to process this recording";
    return {
      id: item.meetingId,
      title: unlocked ? item.title || "Untitled meeting" : "🔒 Locked recording",
      state,
      stateLabel: labels[state],
      detailLabel,
      selected: selected.has(item.meetingId),
      runnable: unlocked && item.state === "queued",
      retryable: unlocked && item.state === "failed",
      removable: unlocked && item.state !== "processing",
      canMoveUp: unlocked && item.state === "queued" && index > 0,
      canMoveDown:
        unlocked && item.state === "queued" && index < reorderableCount - 1,
    };
  }
}
