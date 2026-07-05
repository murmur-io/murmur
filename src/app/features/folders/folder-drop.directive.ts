import {
  Directive,
  ElementRef,
  computed,
  inject,
  input,
  output,
  signal,
} from "@angular/core";
import { NoteDragService } from "./note-drag.service";

/**
 * Makes its host a DROP TARGET for a meeting being dragged from the Library list
 * (the enhancement path; the row's folder-chip popover is the guaranteed path).
 *
 * Responsibilities, all DOM-event plumbing kept OUT of the components:
 *  - `dragover` → `preventDefault()` so the webview actually allows a drop
 *    (WKWebView, like every browser, treats an un-prevented dragover as "no
 *    drop allowed" and never fires `drop`).
 *  - tracks an `over` signal while a note hovers, exposed via the `isDropTarget`
 *    host class for the frosted highlight; only lights up for OUR payload.
 *  - on `drop`, reads the dragged meeting id off the `DataTransfer` and emits
 *    `dropNote` so the host screen runs the `moveNote` move.
 *
 * It does NOT perform the move itself or touch the folder store — single job.
 */
@Directive({
  selector: "[appFolderDrop]",
  host: {
    "[class.is-drop-target]": "over()",
    "[class.is-drop-armed]": "armed()",
    "(dragenter)": "onDragEnter($event)",
    "(dragover)": "onDragOver($event)",
    "(dragleave)": "onDragLeave($event)",
    "(drop)": "onDrop($event)",
  },
})
export class FolderDropDirective {
  private readonly host = inject(ElementRef<HTMLElement>);
  private readonly drag = inject(NoteDragService);

  /**
   * The folder id this target files INTO (null = vault root / "All notes").
   * A drop emits `dropNote` with the dragged meeting id; the host pairs it with
   * THIS target id to call `moveNote(meetingId, dropFolderId)`.
   */
  readonly dropFolderId = input<string | null>(null);

  /** Emits the dragged meeting id when a note is dropped onto this target. */
  readonly dropNote = output<string>();

  /** True while a valid note is hovering directly over this target. */
  private readonly _over = signal(false);
  readonly over = this._over.asReadonly();

  /**
   * True whenever ANY note drag is in flight — used to gently arm every target
   * (subtle dashed outline) so the user can see where notes can go the instant
   * they pick a row up, not only once they hover a specific folder.
   */
  readonly armed = computed(() => this.drag.draggingId() !== null);

  onDragEnter(event: DragEvent): void {
    if (this.drag.draggingId() === null) {
      return; // not our drag — ignore (e.g. a file dragged in from Finder)
    }
    event.preventDefault();
    this._over.set(true);
  }

  onDragOver(event: DragEvent): void {
    if (this.drag.draggingId() === null) {
      return;
    }
    // MUST preventDefault or the webview never fires `drop`.
    event.preventDefault();
    if (event.dataTransfer) {
      event.dataTransfer.dropEffect = "move";
    }
    this._over.set(true);
  }

  onDragLeave(event: DragEvent): void {
    // Only clear when the pointer actually left the host subtree (dragleave
    // also fires when crossing into a child element).
    const related = event.relatedTarget as Node | null;
    if (related && this.host.nativeElement.contains(related)) {
      return;
    }
    this._over.set(false);
  }

  onDrop(event: DragEvent): void {
    this._over.set(false);
    const id =
      event.dataTransfer?.getData(NoteDragService.MIME) ||
      this.drag.draggingId();
    this.drag.end();
    if (!id) {
      return;
    }
    event.preventDefault();
    this.dropNote.emit(id);
  }
}
