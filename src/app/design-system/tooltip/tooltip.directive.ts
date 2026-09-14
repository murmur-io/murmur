import {
  DestroyRef,
  Directive,
  ElementRef,
  inject,
  input,
} from "@angular/core";

/** How long the pointer must rest on a control before its tooltip appears. */
const SHOW_DELAY_MS = 350;
/** Gap between the control's edge and the bubble. */
const OFFSET_PX = 8;
/** Keep the bubble this far from the viewport edges. */
const MARGIN_PX = 8;

/**
 * `[appTooltip]` — a styled hover/focus tooltip for a control whose meaning is
 * carried by an icon alone.
 *
 * WHY THIS EXISTS AT ALL. Every icon-only control in this app already had a
 * native `title`, and the user's report was simply that hovering an icon does
 * not tell you what it does. Both halves of that are true: `title` waits
 * roughly one and a half seconds before the OS draws anything, renders in the
 * system's own style rather than the app's, cannot be triggered by keyboard at
 * all, and is invisible to a touch user. For a row that is now almost entirely
 * glyphs, that is not a tooltip so much as a rumour of one.
 *
 * IT REPLACES `title`, it does not sit beside it — a control carrying both
 * shows two tooltips, ours immediately and the OS's a second later, on top of
 * each other. So every call site drops its `title` when it adopts this, and
 * where a host still has one the directive strips it and adopts its text.
 *
 * IT IS NOT THE ACCESSIBLE NAME. The bubble is `aria-hidden`, and the control
 * keeps naming itself the way it already did (`aria-label`, or visible/`.sr-only`
 * text). A tooltip that carried the name would announce it twice to a screen
 * reader and leave voice control with nothing stable to address.
 *
 * ZONELESS + CONTAINING BLOCKS. The bubble is created on `document.body`
 * rather than inside the host's subtree, for the reason
 * `teleport-to-body.directive.ts` documents at length: a `position: fixed` box
 * positioned from a viewport rect lands off-target under any ancestor with a
 * `transform` / `filter` / `backdrop-filter`, which this app's glass chrome has
 * everywhere. Nothing here touches a signal, so no change detection is
 * involved; the one timer is tracked and cleared on destroy.
 */
@Directive({
  selector: "[appTooltip]",
  host: {
    "(mouseenter)": "schedule()",
    "(mouseleave)": "hide()",
    "(focusin)": "schedule()",
    "(focusout)": "hide()",
    // A control that was just activated should not keep explaining itself over
    // whatever its activation opened.
    "(click)": "hide()",
    "(keydown.escape)": "hide()",
  },
})
export class TooltipDirective {
  /** The explanation to show. Falls back to the host's `title`. */
  readonly text = input<string>("", { alias: "appTooltip" });

  private readonly host = inject<ElementRef<HTMLElement>>(ElementRef);
  private readonly destroyRef = inject(DestroyRef);

  private bubble: HTMLDivElement | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;

  constructor() {
    this.destroyRef.onDestroy(() => this.hide());
  }

  /**
   * Arm the show timer.
   *
   * Focus shows the tooltip unconditionally, with no `:focus-visible` test.
   * Gating on it would be the more refined answer — a pointer user's
   * click-induced focus does not need an explanation of what they just
   * pressed — but that case is ALREADY covered by hiding on `click`, so the
   * gate could only ever take tooltips away, and it would take them away in
   * the one situation nothing here can verify: whether a given engine sets
   * `:focus-visible` on Tab. An unverifiable branch whose only power is to
   * suppress an accessibility affordance is not worth keeping (angular-zoneless.md,
   * T5 — do not depend on engine behaviour you have not confirmed).
   */
  protected schedule(): void {
    const text = this.resolveText();
    if (!text) {
      return;
    }
    this.clearTimer();
    this.timer = setTimeout(() => this.show(text), SHOW_DELAY_MS);
  }

  protected hide(): void {
    this.clearTimer();
    this.bubble?.remove();
    this.bubble = null;
  }

  /** The bound text, or the host's `title` — which is then removed, so the OS
   *  tooltip can never draw a second bubble underneath ours. */
  private resolveText(): string {
    const bound = this.text().trim();
    if (bound) {
      return bound;
    }
    const title = this.host.nativeElement.getAttribute("title")?.trim() ?? "";
    if (title) {
      this.host.nativeElement.removeAttribute("title");
      this.host.nativeElement.dataset["tooltip"] = title;
    }
    return title || (this.host.nativeElement.dataset["tooltip"] ?? "");
  }

  private show(text: string): void {
    this.timer = null;
    // A control removed while its timer was running has nothing to point at.
    if (!this.host.nativeElement.isConnected) {
      return;
    }
    this.hide();
    const bubble = document.createElement("div");
    bubble.className = "mur-tooltip";
    bubble.setAttribute("aria-hidden", "true");
    bubble.textContent = text;
    document.body.appendChild(bubble);
    this.bubble = bubble;
    this.place(bubble);
  }

  /**
   * Below the control by default, flipped above when there is no room, and
   * clamped to the viewport so a control at either end of a row cannot push
   * its own explanation off screen.
   */
  private place(bubble: HTMLDivElement): void {
    const anchor = this.host.nativeElement.getBoundingClientRect();
    const box = bubble.getBoundingClientRect();
    const below = anchor.bottom + OFFSET_PX;
    const above = anchor.top - OFFSET_PX - box.height;
    const fitsBelow = below + box.height <= window.innerHeight - MARGIN_PX;
    const top = fitsBelow ? below : Math.max(MARGIN_PX, above);

    const centred = anchor.left + anchor.width / 2 - box.width / 2;
    const maxLeft = window.innerWidth - box.width - MARGIN_PX;
    const left = Math.max(MARGIN_PX, Math.min(centred, maxLeft));

    bubble.style.top = `${Math.round(top)}px`;
    bubble.style.left = `${Math.round(left)}px`;
  }

  private clearTimer(): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
  }
}
