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

import { MurIconComponent } from "../../../design-system/icon/icon.component";
import type { WorkspaceDestination } from "../workspace-destination";

export type WorkspaceCreateKind = "space" | "note" | "folder" | "dashboard";

export interface WorkspaceCreateRequest {
  readonly kind: WorkspaceCreateKind;
  readonly name: string;
  readonly target: WorkspaceDestination | null;
}

/** Explicit create flow for the Workspaces header; opening it never writes anything. */
@Component({
  selector: "app-workspace-create-sheet",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurIconComponent],
  templateUrl: "./workspace-create-sheet.component.html",
  styleUrl: "./workspace-create-sheet.component.scss",
})
export class WorkspaceCreateSheetComponent {
  private readonly injector = inject(Injector);

  readonly targets = input.required<readonly WorkspaceDestination[]>();
  readonly busy = input(false);
  readonly error = input<string | null>(null);
  readonly create = output<WorkspaceCreateRequest>();
  readonly cancelled = output<void>();

  private readonly nameInput = viewChild<ElementRef<HTMLInputElement>>("nameInput");
  readonly kind = signal<WorkspaceCreateKind>("space");
  readonly name = signal("");
  readonly query = signal("");
  readonly selectedTargetId = signal<string | null>(null);

  readonly filteredTargets = computed(() => {
    if (this.kind() === "space") {
      return [];
    }
    const query = this.query().trim().toLocaleLowerCase();
    return this.targets().filter(
      (target) => !query || target.label.toLocaleLowerCase().includes(query),
    );
  });

  readonly selectedTarget = computed(
    () =>
      this.filteredTargets().find(
        (target) => target.container.id === this.selectedTargetId(),
      ) ?? null,
  );

  readonly defaultName = computed(() => {
    switch (this.kind()) {
      case "space":
        return "New Workspace";
      case "note":
        return "Untitled";
      case "dashboard":
        return "New dashboard";
      default:
        return "New folder";
    }
  });

  constructor() {
    afterNextRender(() => this.nameInput()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

  chooseKind(kind: WorkspaceCreateKind): void {
    if (this.busy()) {
      return;
    }
    this.kind.set(kind);
    this.selectedTargetId.set(null);
  }

  onCreate(): void {
    const target = this.selectedTarget();
    if ((this.kind() !== "space" && !target) || this.busy()) {
      return;
    }
    this.create.emit({
      kind: this.kind(),
      name: this.name().trim() || this.defaultName(),
      target,
    });
  }

  onScrimClick(event: MouseEvent): void {
    if (event.target === event.currentTarget && !this.busy()) {
      this.cancelled.emit();
    }
  }
}
