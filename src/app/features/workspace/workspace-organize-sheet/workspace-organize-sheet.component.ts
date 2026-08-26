import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  inject,
  input,
  output,
  signal,
  viewChild,
} from "@angular/core";

import type {
  WorkspaceOrganizeMove,
  WorkspaceOrganizeFailure,
  WorkspaceOrganizePlan,
  WorkspaceOrganizeSkip,
} from "../../../core/models";
import { MurIconComponent } from "../../../design-system/icon/icon.component";

interface SkipGroup {
  readonly code: WorkspaceOrganizeSkip["code"];
  readonly label: string;
  readonly detail: string;
  readonly items: readonly WorkspaceOrganizeSkip[];
}

const REVIEW_PREVIEW_COUNT = 8;
const MAX_FAILURE_REASON_LENGTH = 240;

export interface WorkspaceOrganizeAttemptReceipt {
  readonly moves: readonly WorkspaceOrganizeMove[];
  readonly appliedIds: readonly string[];
  readonly failures: readonly WorkspaceOrganizeFailure[];
}

export interface WorkspaceOrganizeViewPlan extends WorkspaceOrganizePlan {
  readonly receipt?: WorkspaceOrganizeAttemptReceipt | null;
  readonly applyError?: string | null;
}

interface WorkspaceOrganizeResultRow {
  readonly itemId: string;
  readonly title: string;
  readonly toContainer: string | null;
  readonly reason: string | null;
  readonly retryMove: WorkspaceOrganizeMove | null;
}

/** Review-before-apply sheet for Brain's unfiled-recording organization plan. */
@Component({
  selector: "app-workspace-organize-sheet",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurIconComponent],
  templateUrl: "./workspace-organize-sheet.component.html",
  styleUrl: "./workspace-organize-sheet.component.scss",
})
export class WorkspaceOrganizeSheetComponent {
  private readonly injector = inject(Injector);

  readonly plan = input.required<WorkspaceOrganizeViewPlan>();
  readonly busy = input(false);
  readonly apply = output<WorkspaceOrganizeMove[]>();
  readonly cancelled = output<void>();

  private readonly panel = viewChild<ElementRef<HTMLDivElement>>("panel");
  private readonly excluded = signal<ReadonlySet<string>>(new Set());
  private readonly manualTargets = signal<ReadonlyMap<string, string>>(
    new Map(),
  );
  readonly showAllReview = signal(false);
  readonly expandedSkipCodes = signal<ReadonlySet<string>>(new Set());

  readonly automaticMoves = computed(() => this.plan().moves ?? []);
  readonly reviewItems = computed(() => this.plan().review ?? []);
  readonly targets = computed(() => this.plan().targets ?? []);
  readonly skippedItems = computed(() => this.plan().skipped ?? []);
  readonly receipt = computed(() => this.plan().receipt ?? null);
  readonly applyError = computed(() => this.plan().applyError ?? null);
  readonly appliedIds = computed(
    () => new Set(this.receipt()?.appliedIds ?? []),
  );
  readonly failedIds = computed(
    () => new Set((this.receipt()?.failures ?? []).map((item) => item.itemId)),
  );
  readonly pendingAutomaticMoves = computed(() => {
    const applied = this.appliedIds();
    const failed = this.failedIds();
    return this.automaticMoves().filter(
      (move) => !applied.has(move.itemId) && !failed.has(move.itemId),
    );
  });
  readonly pendingReviewItems = computed(() => {
    const applied = this.appliedIds();
    const failed = this.failedIds();
    return this.reviewItems().filter(
      (item) => !applied.has(item.itemId) && !failed.has(item.itemId),
    );
  });
  readonly appliedRows = computed<WorkspaceOrganizeResultRow[]>(() => {
    const receipt = this.receipt();
    if (!receipt) {
      return [];
    }
    const appliedIds = new Set(receipt.appliedIds);
    return receipt.moves
      .filter((move) => appliedIds.has(move.itemId))
      .map((move) => ({
        itemId: move.itemId,
        title: move.title,
        toContainer: move.toContainer,
        reason: null,
        retryMove: null,
      }));
  });
  readonly failedRows = computed<WorkspaceOrganizeResultRow[]>(() => {
    const receipt = this.receipt();
    if (!receipt) {
      return [];
    }
    const moves = new Map(receipt.moves.map((move) => [move.itemId, move]));
    const plannedTitles = new Map([
      ...this.automaticMoves().map(
        (move) => [move.itemId, move.title] as const,
      ),
      ...this.reviewItems().map((item) => [item.itemId, item.title] as const),
    ]);
    return receipt.failures.map((failure) => {
      const move = moves.get(failure.itemId);
      return {
        itemId: failure.itemId,
        title:
          move?.title ?? plannedTitles.get(failure.itemId) ?? failure.itemId,
        toContainer: move?.toContainer ?? null,
        reason: this.boundedFailureReason(failure.reason),
        retryMove: move ?? null,
      };
    });
  });
  readonly hasAttemptResult = computed(
    () => this.appliedRows().length > 0 || this.failedRows().length > 0,
  );

  readonly includedAutomaticMoves = computed(() => {
    const excluded = this.excluded();
    return this.pendingAutomaticMoves().filter(
      (move) => !excluded.has(move.itemId),
    );
  });

  readonly manualMoves = computed<WorkspaceOrganizeMove[]>(() => {
    const choices = this.manualTargets();
    const targets = new Map(
      this.targets().map((target) => [target.id, target]),
    );
    return this.pendingReviewItems().flatMap((item) => {
      const targetId = choices.get(item.itemId);
      const target = targetId ? targets.get(targetId) : undefined;
      if (!target) {
        return [];
      }
      return [
        {
          itemId: item.itemId,
          title: item.title,
          fromContainerId: null,
          fromContainer: "Unfiled",
          toContainerId: target.id,
          toContainer: target.label,
          reason: `Destination chosen during review. ${item.reason}`,
        },
      ];
    });
  });

  readonly retryableFailedMoves = computed(() =>
    this.failedRows().flatMap((row) =>
      row.retryMove && this.isIncluded(row.itemId) ? [row.retryMove] : [],
    ),
  );
  readonly selectedMoves = computed(() =>
    this.hasAttemptResult()
      ? this.retryableFailedMoves()
      : [...this.includedAutomaticMoves(), ...this.manualMoves()],
  );
  readonly selectedCount = computed(() => this.selectedMoves().length);
  readonly needsChoiceCount = computed(
    () =>
      this.hasAttemptResult()
        ? 0
        : this.pendingReviewItems().length - this.manualMoves().length,
  );
  readonly unchangedAfterAttemptCount = computed(() =>
    this.hasAttemptResult()
      ? this.pendingAutomaticMoves().length + this.pendingReviewItems().length
      : 0,
  );
  readonly visibleReviewItems = computed(() =>
    this.showAllReview()
      ? this.pendingReviewItems()
      : this.pendingReviewItems().slice(0, REVIEW_PREVIEW_COUNT),
  );
  readonly hiddenReviewCount = computed(() =>
    Math.max(
      0,
      this.pendingReviewItems().length - this.visibleReviewItems().length,
    ),
  );
  readonly allSelected = computed(
    () =>
      this.includedAutomaticMoves().length > 0 &&
      this.includedAutomaticMoves().length ===
        this.pendingAutomaticMoves().length,
  );
  readonly skipGroups = computed<SkipGroup[]>(() => {
    const grouped = new Map<
      WorkspaceOrganizeSkip["code"],
      WorkspaceOrganizeSkip[]
    >();
    for (const item of this.skippedItems()) {
      const code = item.code ?? "deferred";
      const existing = grouped.get(code) ?? [];
      existing.push(item);
      grouped.set(code, existing);
    }
    return [...grouped.entries()].map(([code, items]) => ({
      code,
      items,
      ...this.skipGroupCopy(code),
    }));
  });

  constructor() {
    afterNextRender(() => this.panel()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

  isIncluded(itemId: string): boolean {
    return !this.excluded().has(itemId);
  }

  toggleMove(itemId: string): void {
    this.excluded.update((current) => {
      const next = new Set(current);
      if (!next.delete(itemId)) {
        next.add(itemId);
      }
      return next;
    });
  }

  toggleSelectAll(): void {
    if (this.allSelected()) {
      this.excluded.set(
        new Set(this.pendingAutomaticMoves().map((move) => move.itemId)),
      );
      return;
    }
    this.excluded.set(new Set());
  }

  chooseManualTarget(itemId: string, event: Event): void {
    const targetId = (event.target as HTMLSelectElement).value;
    this.manualTargets.update((current) => {
      const next = new Map(current);
      if (targetId) {
        next.set(itemId, targetId);
      } else {
        next.delete(itemId);
      }
      return next;
    });
  }

  selectedManualTarget(itemId: string): string {
    return this.manualTargets().get(itemId) ?? "";
  }

  toggleSkipGroup(code: string): void {
    this.expandedSkipCodes.update((current) => {
      const next = new Set(current);
      if (!next.delete(code)) {
        next.add(code);
      }
      return next;
    });
  }

  isSkipGroupExpanded(code: string): boolean {
    return this.expandedSkipCodes().has(code);
  }

  onApply(): void {
    if (!this.busy() && this.selectedCount() > 0) {
      this.apply.emit(this.selectedMoves());
    }
  }

  onScrimClick(event: MouseEvent): void {
    if (event.target === event.currentTarget && !this.busy()) {
      this.cancelled.emit();
    }
  }

  private boundedFailureReason(reason: string): string {
    const normalized = reason.replace(/\s+/g, " ").trim();
    if (!normalized) {
      return "Review the destination and try again.";
    }
    const hadInvalidArgumentPrefix = /^invalid argument\s*:/i.test(normalized);
    const detail = normalized.replace(/^invalid argument\s*:\s*/i, "").trim();
    let actionable = detail || "Review the destination and try again.";
    if (hadInvalidArgumentPrefix && /lock|seal/i.test(detail)) {
      actionable = "Unlock or choose an open destination, then retry.";
    } else if (hadInvalidArgumentPrefix) {
      actionable = `${this.sentenceCase(detail)} Review the destination and try again.`;
    }
    if (actionable.length <= MAX_FAILURE_REASON_LENGTH) {
      return actionable;
    }
    return `${actionable.slice(0, MAX_FAILURE_REASON_LENGTH - 1)}…`;
  }

  private sentenceCase(value: string): string {
    const withoutTrailingPunctuation = value.replace(/[.!?]+$/, "");
    if (!withoutTrailingPunctuation) {
      return "";
    }
    return `${withoutTrailingPunctuation[0].toLocaleUpperCase()}${withoutTrailingPunctuation.slice(1)}.`;
  }

  private skipGroupCopy(code: WorkspaceOrganizeSkip["code"]): {
    label: string;
    detail: string;
  } {
    switch (code) {
      case "notReady":
        return {
          label: "Still processing",
          detail: "These recordings can be filed after processing finishes.",
        };
      case "emptyNote":
        return {
          label: "No usable content",
          detail: "Brain needs a transcript or note before it can choose a destination.",
        };
      case "noDestination":
        return {
          label: "No open destination",
          detail: "Create or unlock a Space or folder, then plan again.",
        };
      default:
        return {
          label: "Deferred",
          detail: "These recordings remain unfiled and unchanged.",
        };
    }
  }
}
