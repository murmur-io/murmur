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

  readonly plan = input.required<WorkspaceOrganizePlan>();
  readonly busy = input(false);
  readonly apply = output<WorkspaceOrganizeMove[]>();
  readonly cancelled = output<void>();

  private readonly panel = viewChild<ElementRef<HTMLDivElement>>("panel");
  private readonly excluded = signal<ReadonlySet<string>>(new Set());
  private readonly manualTargets = signal<ReadonlyMap<string, string>>(new Map());
  readonly showAllReview = signal(false);
  readonly expandedSkipCodes = signal<ReadonlySet<string>>(new Set());

  readonly automaticMoves = computed(() => this.plan().moves ?? []);
  readonly reviewItems = computed(() => this.plan().review ?? []);
  readonly targets = computed(() => this.plan().targets ?? []);
  readonly skippedItems = computed(() => this.plan().skipped ?? []);

  readonly includedAutomaticMoves = computed(() => {
    const excluded = this.excluded();
    return this.automaticMoves().filter((move) => !excluded.has(move.itemId));
  });

  readonly manualMoves = computed<WorkspaceOrganizeMove[]>(() => {
    const choices = this.manualTargets();
    const targets = new Map(this.targets().map((target) => [target.id, target]));
    return this.reviewItems().flatMap((item) => {
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

  readonly selectedMoves = computed(() => [
    ...this.includedAutomaticMoves(),
    ...this.manualMoves(),
  ]);
  readonly selectedCount = computed(() => this.selectedMoves().length);
  readonly needsChoiceCount = computed(
    () => this.reviewItems().length - this.manualMoves().length,
  );
  readonly visibleReviewItems = computed(() =>
    this.showAllReview()
      ? this.reviewItems()
      : this.reviewItems().slice(0, REVIEW_PREVIEW_COUNT),
  );
  readonly hiddenReviewCount = computed(() =>
    Math.max(0, this.reviewItems().length - this.visibleReviewItems().length),
  );
  readonly allSelected = computed(
    () => this.automaticMoves().length > 0 && this.excluded().size === 0,
  );
  readonly skipGroups = computed<SkipGroup[]>(() => {
    const grouped = new Map<WorkspaceOrganizeSkip["code"], WorkspaceOrganizeSkip[]>();
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
        new Set(this.automaticMoves().map((move) => move.itemId)),
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

  private skipGroupCopy(code: WorkspaceOrganizeSkip["code"]): {
    label: string;
    detail: string;
  } {
    switch (code) {
      case "notReady":
        return {
          label: "Still processing",
          detail: "These recordings can be filed after their note is ready.",
        };
      case "emptyNote":
        return {
          label: "No usable note",
          detail: "Brain needs note content before it can choose a destination.",
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
