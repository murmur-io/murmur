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
} from "../../../core/models";

/** Review-before-apply sheet for Brain's unfiled-recording organization plan. */
@Component({
  selector: "app-workspace-organize-sheet",
  changeDetection: ChangeDetectionStrategy.OnPush,
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

  readonly selectedMoves = computed(() => {
    const excluded = this.excluded();
    return this.plan().moves.filter((move) => !excluded.has(move.itemId));
  });
  readonly selectedCount = computed(() => this.selectedMoves().length);
  readonly allSelected = computed(
    () => this.plan().moves.length > 0 && this.excluded().size === 0,
  );

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
      this.excluded.set(new Set(this.plan().moves.map((move) => move.itemId)));
      return;
    }
    this.excluded.set(new Set());
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
