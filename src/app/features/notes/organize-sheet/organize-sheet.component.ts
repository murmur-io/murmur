import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
  viewChild,
} from "@angular/core";
import type {
  OrganizeFailure,
  OrganizeMove,
  OrganizePlan,
} from "../../../core/models";

export interface OrganizeAttemptReceipt {
  readonly moves: readonly OrganizeMove[];
  readonly appliedIds: readonly string[];
  readonly failures: readonly OrganizeFailure[];
}

export interface OrganizeViewPlan extends OrganizePlan {
  /** Stable coverage of the model plan, even after applied rows are removed. */
  readonly plannedProposedCount?: number;
  readonly receipt?: OrganizeAttemptReceipt | null;
  readonly applyError?: string | null;
}

/** Editable, confirm-before-apply review for the bounded note organizer. */
@Component({
  selector: "app-organize-sheet",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./organize-sheet.component.html",
  styleUrl: "./organize-sheet.component.scss",
})
export class OrganizeSheetComponent {
  private readonly injector = inject(Injector);

  readonly plan = input<OrganizeViewPlan | null>(null);
  readonly busy = input(false);
  readonly planning = input(false);
  readonly applying = input(false);
  readonly guidanceEnabled = input(false);
  readonly apply = output<OrganizeMove[]>();
  readonly replan = output<string>();
  readonly cancelled = output<void>();

  private readonly panel = viewChild<ElementRef<HTMLDivElement>>("panel");
  private readonly excluded = signal<ReadonlySet<string>>(new Set());
  private readonly targetOverrides = signal<ReadonlyMap<string, string>>(
    new Map(),
  );
  readonly guidance = signal("");
  private previousPlan: OrganizeViewPlan | null = null;

  private readonly _resetReviewForFreshPlan = effect(() => {
    const plan = this.plan();
    if (plan === this.previousPlan) {
      return;
    }
    this.previousPlan = plan;
    // Receipt/error updates describe the SAME review attempt. A clean plan
    // object is a new model review (including an identical replan response),
    // so local exclusions and destination edits must not leak into it.
    if (plan && !plan.receipt && !plan.applyError) {
      this.excluded.set(new Set());
      this.targetOverrides.set(new Map());
    }
  });

  readonly moves = computed(() => this.plan()?.moves ?? []);
  readonly receipt = computed(() => this.plan()?.receipt ?? null);
  readonly appliedIds = computed(
    () => new Set(this.receipt()?.appliedIds ?? []),
  );
  readonly failures = computed(
    () => new Map((this.receipt()?.failures ?? []).map((item) => [item.noteId, item])),
  );
  readonly pendingMoves = computed(() =>
    this.moves().filter((move) => !this.appliedIds().has(move.noteId)),
  );
  readonly selectedMoves = computed(() => {
    const excluded = this.excluded();
    const targets = new Map(
      (this.plan()?.targets ?? []).map((target) => [target.id, target]),
    );
    const overrides = this.targetOverrides();
    return this.pendingMoves()
      .filter(
        (move) =>
          !excluded.has(move.noteId) &&
          this.failures().get(move.noteId)?.retryable !== false,
      )
      .map((move) => {
        const targetId = overrides.get(move.noteId);
        const target = targetId ? targets.get(targetId) : null;
        return target
          ? { ...move, toFolderId: target.id, toFolder: target.label }
          : move;
      });
  });
  readonly selectedCount = computed(() => this.selectedMoves().length);
  readonly allSelected = computed(
    () =>
      this.pendingMoves().length > 0 &&
      this.selectedCount() === this.pendingMoves().length,
  );

  constructor() {
    afterNextRender(() => this.panel()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

  isIncluded(noteId: string): boolean {
    return !this.excluded().has(noteId);
  }

  toggleMove(noteId: string): void {
    this.excluded.update((current) => {
      const next = new Set(current);
      if (!next.delete(noteId)) {
        next.add(noteId);
      }
      return next;
    });
  }

  clearAll(): void {
    this.excluded.set(new Set(this.pendingMoves().map((move) => move.noteId)));
  }

  selectAll(): void {
    this.excluded.set(new Set());
  }

  chooseTarget(noteId: string, event: Event): void {
    const value = (event.target as HTMLSelectElement).value;
    this.targetOverrides.update((current) => {
      const next = new Map(current);
      if (value) {
        next.set(noteId, value);
      } else {
        next.delete(noteId);
      }
      return next;
    });
  }

  selectedTarget(move: OrganizeMove): string {
    return this.targetOverrides().get(move.noteId) ?? move.toFolderId ?? "";
  }

  failureFor(noteId: string): OrganizeFailure | null {
    return this.failures().get(noteId) ?? null;
  }

  onReplan(): void {
    if (!this.busy()) {
      this.replan.emit(this.guidance().trim());
    }
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
}
