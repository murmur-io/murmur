import {
  DestroyRef,
  Directive,
  ElementRef,
  inject,
  output,
} from "@angular/core";

/**
 * Fires `reposition` whenever the viewport geometry that a floating popover is
 * anchored to could have shifted — a scroll ANYWHERE (capture phase, so nested
 * scrollers count) or a window resize. The popover recomputes its own top/left
 * from the live selection rect in the handler.
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
  private readonly host = inject(ElementRef);

  /** Emitted on any scroll/resize so the popover re-measures its anchor rect. */
  readonly reposition = output<void>();

  constructor() {
    const onScroll = (): void => this.reposition.emit();
    const onResize = (): void => this.reposition.emit();
    // Capture-phase scroll catches scrolling inside any ancestor scroller, not
    // just the window (the editor body scrolls, the window usually doesn't).
    window.addEventListener("scroll", onScroll, { capture: true, passive: true });
    window.addEventListener("resize", onResize, { passive: true });
    this.destroyRef.onDestroy(() => {
      window.removeEventListener("scroll", onScroll, { capture: true });
      window.removeEventListener("resize", onResize);
    });
    // Reference the host so the injected ElementRef isn't flagged unused; the
    // popover positions itself from window geometry, the host is just anchor.
    void this.host;
  }
}
