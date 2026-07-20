import {
  DestroyRef,
  Directive,
  ElementRef,
  inject,
  input,
  output,
} from "@angular/core";

export type RepositionReason = "scroll" | "resize" | "motion";

/**
 * Fires `reposition` whenever the viewport geometry that a floating popover is
 * anchored to could have shifted — a scroll ANYWHERE (capture phase, so nested
 * scrollers count), a window resize, or the end of an ancestor CSS motion. The
 * popover recomputes its viewport position from the live anchor in the handler.
 *
 * DOM listeners live in a directive (angular-zoneless §5), never ad-hoc in the
 * component, and every listener is torn down via `DestroyRef.onDestroy` — no
 * leaked handler. Passive + capture so it never blocks scrolling. The host is
 * incidental (the directive attaches to the popover element for co-location);
 * it listens on `window`, not the host.
 */
@Directive({
  selector: "[appRepositionOnScroll]",
})
export class RepositionOnScrollDirective {
  private readonly destroyRef = inject(DestroyRef);
  private readonly host = inject<ElementRef<HTMLElement>>(ElementRef);

  /** Optional live DOM anchor; used to filter CSS-motion events to its own ancestors. */
  readonly repositionAnchor = input<HTMLElement | null>(null);
  /** Emitted after external scroll/resize/motion so the owner re-measures its anchor. */
  readonly reposition = output<RepositionReason>();

  constructor() {
    const isInsideHost = (event: Event): boolean => {
      const target = event.target;
      return (
        target instanceof Node &&
        this.host.nativeElement.contains(target)
      );
    };
    const onExternalScroll = (event: Event): void => {
      // Scrolling the overlay's own list does not move its viewport anchor.
      // Re-measuring and rewriting geometry for every internal scroll tick caused
      // layout thrash and blank fixed-layer paints in WKWebView.
      if (isInsideHost(event)) {
        return;
      }
      this.reposition.emit("scroll");
    };
    const onAnchorMotionEnd = (event: Event): void => {
      const anchor = this.repositionAnchor();
      const target = event.target;
      if (
        !anchor ||
        !(target instanceof Element) ||
        isInsideHost(event) ||
        (target !== anchor && !target.contains(anchor))
      ) {
        return;
      }
      this.reposition.emit("motion");
    };
    const onResize = (): void => this.reposition.emit("resize");
    // Capture-phase scroll catches scrolling inside any ancestor scroller, not
    // just the window (the editor body scrolls, the window usually doesn't).
    window.addEventListener("scroll", onExternalScroll, {
      capture: true,
      passive: true,
    });
    window.addEventListener("resize", onResize, { passive: true });
    // A teleported overlay does not inherit an ancestor's transform animation.
    // Re-measure once that motion settles so it cannot retain the rect captured
    // mid-entry (the Note panel's `rise` animation is one real example).
    window.addEventListener("animationend", onAnchorMotionEnd, {
      capture: true,
    });
    window.addEventListener("transitionend", onAnchorMotionEnd, {
      capture: true,
    });
    this.destroyRef.onDestroy(() => {
      window.removeEventListener("scroll", onExternalScroll, {
        capture: true,
      });
      window.removeEventListener("resize", onResize);
      window.removeEventListener("animationend", onAnchorMotionEnd, {
        capture: true,
      });
      window.removeEventListener("transitionend", onAnchorMotionEnd, {
        capture: true,
      });
    });
  }
}
