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
import type { ContainerNoun } from "../../../core/hierarchy-vocabulary";

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
  /**
   * The DISPLAY word, from `core/hierarchy-vocabulary.ts` — not a kind or a level.
   *
   * This used to be typed `"space" | "folder"`, which is the code's vocabulary; the sheet then
   * told the user it was deleting a "space" while the sheet that created the same thing called
   * it a Workspace. `ContainerNoun` keeps the two vocabularies from drifting again: a domain
   * identifier no longer typechecks where a sentence is being written.
   */
  readonly noun = input.required<ContainerNoun>();
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
