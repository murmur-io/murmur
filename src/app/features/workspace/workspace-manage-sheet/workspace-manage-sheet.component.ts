import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  inject,
  input,
  output,
  viewChild,
} from "@angular/core";

import { MurIconComponent } from "../../../design-system/icon/icon.component";

export type WorkspaceManageMode = "rename" | "delete";

/** Explicit rename/delete confirmation for a Workspace hierarchy row. */
@Component({
  selector: "app-workspace-manage-sheet",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurIconComponent],
  templateUrl: "./workspace-manage-sheet.component.html",
  styleUrl: "./workspace-manage-sheet.component.scss",
})
export class WorkspaceManageSheetComponent {
  private readonly injector = inject(Injector);

  readonly mode = input.required<WorkspaceManageMode>();
  readonly name = input.required<string>();
  readonly noun = input.required<"space" | "folder">();
  readonly busy = input(false);
  readonly error = input<string | null>(null);
  readonly renamed = output<string>();
  readonly deleteConfirmed = output<void>();
  readonly cancelled = output<void>();

  private readonly nameInput = viewChild<ElementRef<HTMLInputElement>>("nameInput");

  constructor() {
    afterNextRender(() => {
      const input = this.nameInput()?.nativeElement;
      input?.focus();
      input?.select();
    }, { injector: this.injector });
  }

  confirm(): void {
    if (this.busy()) {
      return;
    }
    if (this.mode() === "delete") {
      this.deleteConfirmed.emit();
      return;
    }
    const name = this.nameInput()?.nativeElement.value.trim();
    if (name && name !== this.name()) {
      this.renamed.emit(name);
    }
  }

  onScrimClick(event: MouseEvent): void {
    if (event.target === event.currentTarget && !this.busy()) {
      this.cancelled.emit();
    }
  }
}
