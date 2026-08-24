import { Injectable, computed, signal } from "@angular/core";

/**
 * What can be dragged into a container.
 *
 * Tasks and dashboards are absent on purpose: neither has a container anchor yet,
 * so a drop would have nowhere to file it. They join when their backend halves do.
 */
/**
 * What can be dragged into a container.
 *
 * All four kinds the hierarchy renders, because a row a user can see under a project is a row
 * they will try to drag out of it — and the three that silently ignored the gesture were the
 * ones that had no mover behind them, not the ones that were meant to sit still. Each kind
 * moves through its OWN backend command (see `WorkspaceService.moveItem`); the kind travels with
 * the payload precisely because the id alone cannot say which one.
 */
export type DraggableKind = "meeting" | "note" | "dashboard" | "task";

/** The in-flight drag: an id is not enough, because the two kinds move differently. */
interface DragPayload {
  id: string;
  kind: DraggableKind;
}

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

  private readonly _dragging = signal<DragPayload | null>(null);

  /**
   * The id currently being dragged, or null when no drag is in flight.
   *
   * Kept as-is for every existing caller: the Library and both old trees were
   * written when only a meeting could be dragged, and they ask this question and
   * no other.
   */
  readonly draggingId = computed(() => this._dragging()?.id ?? null);

  /**
   * WHAT is being dragged. The workspace hierarchy can drag a note as readily as a
   * meeting, and they move through different commands — so a target that acts on a
   * drop has to know which, and an id alone cannot say.
   */
  readonly draggingKind = computed(() => this._dragging()?.kind ?? null);

  /** Mark a drag as started (called from the row's `dragstart`). */
  begin(id: string, kind: DraggableKind = "meeting"): void {
    this._dragging.set({ id, kind });
  }

  /** Clear the drag (called on `dragend` / after a `drop`). */
  end(): void {
    this._dragging.set(null);
  }
}
