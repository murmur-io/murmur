import {
  DestroyRef,
  Directive,
  ElementRef,
  Injector,
  afterNextRender,
  inject,
} from "@angular/core";

/**
 * `[appTeleportToBody]` — moves the host element to `document.body` so a floating
 * overlay escapes any ancestor that would otherwise be its `position: fixed`
 * CONTAINING BLOCK.
 *
 * WHY (the bug this closes): an overlay that is `position: fixed` and positioned
 * in JS from a viewport `getBoundingClientRect()` only anchors to the VIEWPORT
 * when NO ancestor establishes a fixed-positioning containing block. Any ancestor
 * with `transform` / `filter` / `backdrop-filter` / `perspective` / `will-change`
 * / `contain` becomes that containing block instead, so the viewport-computed
 * `left/top` resolve relative to the ancestor's box and the overlay lands
 * offset / off-screen / clipped. Murmur's frosted `.card` (backdrop-filter) and
 * the note drawer (a `translateX` entry animation) routinely trip this, which is
 * why the source picker "looked dead" and the selection toolbar floated off the
 * text. Teleporting the overlay box to `body` (always the viewport containing
 * block) makes the EXISTING coordinate math correct on every surface, and can
 * never re-break when an ancestor later gains a transform/filter.
 *
 * TEARDOWN (the subtle part — a prior version got this WRONG and made the picker
 * undismissable): when the host `@if` flips false, Angular's `detachView` removes
 * the view's DOM nodes via their CURRENT parent (`document.body`) BEFORE it runs
 * destroy hooks. So the teleported node is ALREADY removed by the time this
 * directive's `onDestroy` fires. We must therefore NOT try to move the node back
 * to its original slot — doing so re-inserts (RESURRECTS) an already-removed
 * overlay into the DOM. We simply detach from `body` as an idempotent safety net
 * (a no-op when Angular already removed it, `parentNode === null`), so a
 * teleported node can never orphan/leak either. Zoneless — `afterNextRender`
 * (never `setTimeout`/`rAF`), cleanup via `DestroyRef`.
 */
@Directive({
  selector: "[appTeleportToBody]",
})
export class TeleportToBodyDirective {
  private readonly el = inject<ElementRef<HTMLElement>>(ElementRef);
  private readonly injector = inject(Injector);
  private readonly destroyRef = inject(DestroyRef);

  /** Whether the node was actually moved to <body> (afterNextRender ran). */
  private teleported = false;

  constructor() {
    afterNextRender(
      () => {
        const node = this.el.nativeElement;
        if (node.parentNode) {
          document.body.appendChild(node);
          this.teleported = true;
        }
      },
      { injector: this.injector },
    );

    this.destroyRef.onDestroy(() => {
      // Idempotent detach ONLY — never re-insert. Angular's view teardown already
      // removed the node (via its current parent) before this hook, so remove() is
      // usually a no-op; it only bites if some path left the node parented to body,
      // guaranteeing no orphan. Re-inserting here would resurrect a dismissed
      // overlay — the exact defect this replaces.
      if (this.teleported) {
        this.el.nativeElement.remove();
      }
    });
  }
}
