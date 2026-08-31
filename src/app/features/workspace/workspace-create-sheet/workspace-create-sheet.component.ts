import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  inject,
  input,
  linkedSignal,
  output,
  signal,
  viewChild,
} from "@angular/core";

import { MurIconComponent } from "../../../design-system/icon/icon.component";
import type { WorkspaceDestination } from "../workspace-destination";

export type WorkspaceCreateKind = "space" | "note" | "folder" | "dashboard";

/** A destination the user wants but that does not exist yet. */
export interface WorkspaceCreateNewContainer {
  readonly kind: "space" | "folder";
  readonly name: string;
}

export interface WorkspaceCreateRequest {
  readonly kind: WorkspaceCreateKind;
  readonly name: string;
  readonly target: WorkspaceDestination | null;
  /**
   * When set, the caller creates THIS container first and puts the item inside
   * it. `target` then means the PARENT the new folder goes under, and is null
   * for a new top-level Workspace. Only ever set for an item kind (note /
   * dashboard) — see `canCreateDestination`.
   */
  readonly newContainer: WorkspaceCreateNewContainer | null;
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
  /** What the opener came here to make — the Workspaces header says Workspace, "New note" says note. */
  readonly initialKind = input<WorkspaceCreateKind>("space");
  readonly create = output<WorkspaceCreateRequest>();
  readonly cancelled = output<void>();

  private readonly nameInput = viewChild<ElementRef<HTMLInputElement>>("nameInput");
  /**
   * `linkedSignal`, not `signal(this.initialKind())`: a signal input is not bound
   * yet when a field initializer runs, so the plain form would freeze the DEFAULT
   * instead of what the opener asked for. The source is constant for one open —
   * the sheet is created fresh each time — so the reset semantics never fire
   * under the user's hands mid-edit.
   */
  readonly kind = linkedSignal(() => this.initialKind());
  readonly name = signal("");
  readonly query = signal("");
  readonly selectedTargetId = signal<string | null>(null);
  /** Which brand-new container the item should land in, if the user armed one. */
  readonly newContainerKind = signal<"space" | "folder" | null>(null);
  readonly newContainerName = signal("");

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

  /**
   * Only an ITEM lands inside a container, so only an item can bring a brand-new
   * container with it. A Workspace has no parent to invent, and offering "a new
   * folder to hold this new folder" is rope nobody asked for.
   */
  readonly canCreateDestination = computed(
    () => this.kind() === "note" || this.kind() === "dashboard",
  );

  readonly newContainerDefaultName = computed(() =>
    this.newContainerKind() === "space" ? "New Workspace" : "New folder",
  );

  /** A new Workspace needs no parent; a new folder needs one; otherwise pick a destination. */
  readonly createBlocked = computed(() => {
    if (this.busy()) {
      return true;
    }
    switch (this.newContainerKind()) {
      case "space":
        return false;
      case "folder":
        return !this.selectedTarget();
      default:
        return this.kind() !== "space" && !this.selectedTarget();
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
    this.disarmNewContainer();
  }

  /** Toggle "put this in a container that does not exist yet". */
  armNewContainer(kind: "space" | "folder"): void {
    if (this.busy()) {
      return;
    }
    if (this.newContainerKind() === kind) {
      this.disarmNewContainer();
      return;
    }
    this.newContainerKind.set(kind);
    this.newContainerName.set("");
  }

  disarmNewContainer(): void {
    this.newContainerKind.set(null);
    this.newContainerName.set("");
  }

  onCreate(): void {
    if (this.createBlocked()) {
      return;
    }
    const pending = this.newContainerKind();
    this.create.emit({
      kind: this.kind(),
      name: this.name().trim() || this.defaultName(),
      // A new top-level Workspace has no parent; every other shape carries the
      // row the user selected.
      target: pending === "space" ? null : this.selectedTarget(),
      newContainer: pending
        ? {
            kind: pending,
            name: this.newContainerName().trim() || this.newContainerDefaultName(),
          }
        : null,
    });
  }

  onScrimClick(event: MouseEvent): void {
    if (event.target === event.currentTarget && !this.busy()) {
      this.cancelled.emit();
    }
  }
}
