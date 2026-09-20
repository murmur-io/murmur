import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  effect,
  inject,
  signal,
  viewChild,
} from "@angular/core";

import { IpcService } from "../../core/ipc.service";
import type { DestinationPickerAnchorKind } from "../../core/models";
import { TeleportToBodyDirective } from "../../design-system/teleport-to-body.directive";
import { MurIconComponent } from "../../design-system/icon/icon.component";
import { WorkspaceService } from "../../features/workspace/workspace.service";
import { FoldersService } from "../../services/folders.service";
import { ToastService } from "../../services/toast.service";
import {
  RelatedHierarchyPickerComponent,
  type DestinationPickerTarget,
} from "../related-hierarchy-picker/related-hierarchy-picker.component";
import { DestinationMoveService } from "./destination-move.service";

@Component({
  selector: "app-destination-move",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurIconComponent, RelatedHierarchyPickerComponent, TeleportToBodyDirective],
  templateUrl: "./destination-move.component.html",
  styleUrl: "./destination-move.component.scss",
})
export class DestinationMoveComponent {
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);
  private readonly workspace = inject(WorkspaceService);
  private readonly toast = inject(ToastService);
  private readonly injector = inject(Injector);
  protected readonly moves = inject(DestinationMoveService);

  private operation = 0;
  readonly busy = signal(false);
  readonly error = signal<string | null>(null);
  readonly pendingLockedTarget = signal<DestinationPickerTarget | null>(null);
  private readonly confirmCancel =
    viewChild<ElementRef<HTMLButtonElement>>("confirmCancel");
  private readonly confirmCommit =
    viewChild<ElementRef<HTMLButtonElement>>("confirmCommit");
  private readonly picker = viewChild(RelatedHierarchyPickerComponent);
  readonly request = computed(() => this.moves.active()?.request ?? null);
  readonly actionLabel = computed(() => this.request()?.actionLabel ?? "Move");
  readonly pickerMode = computed<"destination" | "scope">(() =>
    this.request()?.kind === "scope" ? "scope" : "destination",
  );
  readonly anchorKind = computed<DestinationPickerAnchorKind>(() => {
    const request = this.request();
    if (!request) {
      return "meeting";
    }
    return (
      request.anchorKind ?? (request.kind === "shared" ? "org" : request.kind === "scope" ? "container" : request.kind)
    );
  });

  private readonly _focusConfirmation = effect(() => {
    if (this.pendingLockedTarget()) {
      afterNextRender(() => this.confirmCancel()?.nativeElement.focus(), {
        injector: this.injector,
      });
    }
  });

  invalidate(): void {
    this.operation += 1;
    this.pendingLockedTarget.set(null);
    this.error.set(null);
    if (this.busy()) this.moves.detach();
    else this.moves.finish({ moved: false });
    this.busy.set(false);
  }

  close(): void {
    if (this.request() && !this.busy()) {
      this.pendingLockedTarget.set(null);
      this.error.set(null);
      this.moves.finish({ moved: false });
    }
  }

  choose(target: DestinationPickerTarget): void {
    if (this.request()?.kind === "scope") {
      this.moves.finish({
        moved: false,
        selected: true,
        containerId: target.containerId,
        level: target.level,
        label: target.label,
        breadcrumb: target.breadcrumb,
      });
      return;
    }
    if (target.locked) {
      this.pendingLockedTarget.set(target);
      return;
    }
    void this.commit(target);
  }

  cancelConfirmation(): void {
    this.pendingLockedTarget.set(null);
    afterNextRender(() => this.picker()?.focusDestinationSelection(), {
      injector: this.injector,
    });
  }

  confirmLockedMove(): void {
    const target = this.pendingLockedTarget();
    if (target) {
      this.pendingLockedTarget.set(null);
      void this.commit(target);
    }
  }

  onConfirmationKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      this.cancelConfirmation();
      return;
    }
    if (event.key !== "Tab") {
      return;
    }
    const cancel = this.confirmCancel()?.nativeElement;
    const commit = this.confirmCommit()?.nativeElement;
    if (!cancel || !commit) {
      return;
    }
    if (event.shiftKey && document.activeElement === cancel) {
      event.preventDefault();
      commit.focus();
    } else if (!event.shiftKey && document.activeElement === commit) {
      event.preventDefault();
      cancel.focus();
    }
  }

  private async commit(target: DestinationPickerTarget): Promise<void> {
    const request = this.request();
    if (!request || this.busy()) {
      return;
    }
    const operation = ++this.operation;
    this.busy.set(true);
    this.error.set(null);
    try {
      if (request.execute) {
        await request.execute(target.containerId, target.locked);
      } else {
        switch (request.kind) {
          case "scope":
            return;
          case "meeting":
            await this.folders.moveNote(
              request.id,
              target.containerId,
              target.locked,
            );
            break;
          case "note":
          case "document":
            if (!target.containerId) {
              throw new Error(
                "The Notes root is not available as a destination yet.",
              );
            }
            await this.ipc.moveNoteDoc(
              request.id,
              target.containerId,
              target.locked,
            );
            break;
          case "task":
            await this.ipc.setTaskContainer(request.id, target.containerId);
            break;
          case "dashboard":
            await this.ipc.moveDashboardToContainer(
              request.id,
              target.containerId,
            );
            break;
          case "container":
            await this.ipc.moveContainer(request.id, target.containerId);
            break;
          case "shared":
            if (!request.orgId || !request.sharedTargetKind) {
              throw new Error("This shared item has no placement identity.");
            }
            await this.ipc.setSharedPlacement(
              request.orgId,
              request.sharedTargetKind,
              request.id,
              target.containerId,
              0,
            );
            break;
        }
      }
      if (operation !== this.operation) {
        this.moves.finish({ moved: true, containerId: target.containerId });
        return;
      }
      try {
        await this.workspace.reload();
        if (operation !== this.operation) {
          this.moves.finish({ moved: true, containerId: target.containerId });
          return;
        }
        await request.afterMove?.();
      } catch {
        // The canonical write succeeded. A failed refresh must not offer a duplicate write.
        if (operation === this.operation) {
          this.toast.danger("Moved successfully, but the view couldn’t refresh. Reopen it to update.");
        }
      }
      if (operation !== this.operation) {
        this.moves.finish({ moved: true, containerId: target.containerId });
        return;
      }
      this.toast.success(
        `${this.actionLabel() === "Add a copy" ? "Added" : "Moved"} “${request.title}” to ${target.breadcrumb}`,
      );
      this.moves.finish({ moved: true, containerId: target.containerId });
    } catch (cause) {
      if (operation !== this.operation) {
        this.moves.finish({ moved: false });
        return;
      }
      const text = cause instanceof Error ? cause.message : String(cause);
      this.error.set(
        text.slice(0, 240) || "Couldn’t move this item. Please try again.",
      );
    } finally {
      if (operation === this.operation) this.busy.set(false);
    }
  }
}
