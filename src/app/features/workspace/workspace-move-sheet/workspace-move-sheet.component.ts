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

import type { ItemRow } from "../../../core/models";
import { MurIconComponent } from "../../../design-system/icon/icon.component";
import type { WorkspaceDestination } from "../workspace-destination";

/** Viewport-safe destination picker used by every keyboard/pointer Move affordance in Workspaces. */
@Component({
  selector: "app-workspace-move-sheet",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurIconComponent],
  templateUrl: "./workspace-move-sheet.component.html",
  styleUrl: "./workspace-move-sheet.component.scss",
})
export class WorkspaceMoveSheetComponent {
  private readonly injector = inject(Injector);

  readonly item = input.required<{
    readonly kind: ItemRow["kind"] | "unclassified";
    readonly title: string | null;
  }>();
  readonly fromLabel = input.required<string>();
  readonly actionVerb = input<"Move" | "Add a copy">("Move");
  readonly targets = input.required<readonly WorkspaceDestination[]>();
  readonly busy = input(false);
  readonly error = input<string | null>(null);
  readonly move = output<WorkspaceDestination>();
  readonly cancelled = output<void>();

  private readonly searchInput = viewChild<ElementRef<HTMLInputElement>>("searchInput");
  readonly query = signal("");
  readonly filteredTargets = computed(() => {
    const query = this.query().trim().toLocaleLowerCase();
    return query
      ? this.targets().filter((target) =>
          target.label.toLocaleLowerCase().includes(query),
        )
      : this.targets();
  });

  readonly itemKind = computed(() => {
    switch (this.item().kind) {
      case "meeting":
        return "recording";
      case "dashboard":
        return "dashboard";
      case "task":
        return "task";
      case "unclassified":
        return "shared item";
      default:
        return "note";
    }
  });

  readonly title = computed(() => this.item().title?.trim() || "Untitled");

  constructor() {
    afterNextRender(() => this.searchInput()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

  choose(target: WorkspaceDestination): void {
    if (!this.busy()) {
      this.move.emit(target);
    }
  }

  onScrimClick(event: MouseEvent): void {
    if (event.target === event.currentTarget && !this.busy()) {
      this.cancelled.emit();
    }
  }
}
