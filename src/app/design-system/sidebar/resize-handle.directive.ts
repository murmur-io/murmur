import { Directive, input, output } from "@angular/core";

/** Keyboard step for the arrow keys; Shift multiplies it. */
const KEY_STEP_PX = 16;
const KEY_STEP_SHIFT_MULTIPLIER = 4;

/**
 * `[appResizeHandle]` — turns its host into a vertical splitter that resizes a
 * panel sitting to its LEFT (the primary sidebar). The directive only reports
 * the width the user is asking for; the owner clamps, stores and applies it, so
 * the same handle can sit on any left-hand panel without knowing its limits.
 *
 * Pointer: the drag is measured against the width at pointer-down, and the
 * pointer is captured, so moving faster than the handle — or leaving the
 * window — never drops the drag. Keyboard: it is a focusable `separator`, the
 * arrow keys step the width, Home/End jump to the limits. Double-click (or
 * Enter) asks for the default width back.
 */
@Directive({
  selector: "[appResizeHandle]",
  host: {
    role: "separator",
    tabindex: "0",
    "aria-orientation": "vertical",
    "[attr.aria-valuenow]": "width()",
    "[attr.aria-valuemin]": "min()",
    "[attr.aria-valuemax]": "max()",
    "(pointerdown)": "onPointerDown($event)",
    "(pointermove)": "onPointerMove($event)",
    "(pointerup)": "onPointerEnd($event)",
    "(pointercancel)": "onPointerEnd($event)",
    "(lostpointercapture)": "onPointerEnd($event)",
    "(dblclick)": "resetWidth.emit()",
    "(keydown)": "onKeydown($event)",
  },
})
export class ResizeHandleDirective {
  /** The panel's current width in px — the drag's starting point. */
  readonly width = input.required<number>();
  readonly min = input.required<number>();
  readonly max = input.required<number>();

  /** The width the user is asking for (unclamped for drags; the owner clamps). */
  readonly widthChange = output<number>();
  /** A drag started (`true`) or ended (`false`) — lets the owner drop transitions. */
  readonly resizing = output<boolean>();
  /** Double-click / Enter: go back to the default width. */
  readonly resetWidth = output<void>();

  private drag: { pointerId: number; startX: number; startWidth: number } | null =
    null;

  onPointerDown(event: PointerEvent): void {
    if (event.button !== 0) return;
    event.preventDefault();
    const el = event.currentTarget as Element;
    el.setPointerCapture?.(event.pointerId);
    this.drag = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startWidth: this.width(),
    };
    this.resizing.emit(true);
  }

  onPointerMove(event: PointerEvent): void {
    const drag = this.drag;
    if (!drag || drag.pointerId !== event.pointerId) return;
    this.widthChange.emit(drag.startWidth + (event.clientX - drag.startX));
  }

  onPointerEnd(event: PointerEvent): void {
    if (!this.drag || this.drag.pointerId !== event.pointerId) return;
    this.drag = null;
    this.resizing.emit(false);
  }

  onKeydown(event: KeyboardEvent): void {
    const step =
      KEY_STEP_PX * (event.shiftKey ? KEY_STEP_SHIFT_MULTIPLIER : 1);
    let next: number;
    switch (event.key) {
      case "ArrowLeft":
        next = this.width() - step;
        break;
      case "ArrowRight":
        next = this.width() + step;
        break;
      case "Home":
        next = this.min();
        break;
      case "End":
        next = this.max();
        break;
      case "Enter":
        event.preventDefault();
        this.resetWidth.emit();
        return;
      default:
        return;
    }
    event.preventDefault();
    this.widthChange.emit(next);
  }
}
