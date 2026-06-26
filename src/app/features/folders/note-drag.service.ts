import { Injectable, signal } from "@angular/core";

/**
 * Tiny coordinator for note → folder drag-and-drop within the Library.
 *
 * HTML5 drag-and-drop already carries the payload in the `DataTransfer`, but the
 * webview (WKWebView) does NOT expose `getData()` during `dragover` — only on
 * `drop`. The drop-target highlight needs to know "is a note being dragged right
 * now?" DURING the drag, so we mirror the dragged meeting id into a signal here.
 * Folders read {@link draggingId} to light up as valid targets; rows read it to
 * dim the source. The signal is the single source of truth for the live drag.
 *
 * This is intentionally NOT the move itself — the actual `moveNote` IPC is fired
 * by whoever owns the data (the Library), reading the id off the `DataTransfer`
 * on `drop`. This service is purely the cross-component "a drag is in flight"
 * readiness signal, mirroring the lock/exposure split used elsewhere.
 */
@Injectable({ providedIn: "root" })
export class NoteDragService {
  /** MIME-ish key the Library writes the dragged meeting id under. */
  static readonly MIME = "application/x-murmur-note";

  private readonly _draggingId = signal<string | null>(null);

  /** The meeting id currently being dragged, or null when no drag is in flight. */
  readonly draggingId = this._draggingId.asReadonly();

  /** Mark a drag as started (called from the row's `dragstart`). */
  begin(meetingId: string): void {
    this._draggingId.set(meetingId);
  }

  /** Clear the drag (called on `dragend` / after a `drop`). */
  end(): void {
    this._draggingId.set(null);
  }
}
