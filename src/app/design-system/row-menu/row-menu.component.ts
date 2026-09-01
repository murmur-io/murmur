import {
  afterNextRender,
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  inject,
  input,
  signal,
  viewChild,
} from "@angular/core";

import { RepositionOnScrollDirective } from "../../features/notes/note-brain-popover/reposition-on-scroll.directive";
import { TeleportToBodyDirective } from "../teleport-to-body.directive";

const PANEL_GAP_PX = 4;
const VIEWPORT_MARGIN_PX = 8;

/**
 * Design System — `<mur-row-menu>`: THE ellipsis-trigger dropdown for a tree row's
 * per-item actions (extracted 2026-07-12, after Meetings' folder rows and
 * Notes' folder rows drifted into TWO differently-styled action clusters
 * twice in one day — first a ⋯ kebab-vs-flat-icons split, then a "flat icons
 * that measured the same size but read visibly different in color/weight"
 * near-miss. Per the user's explicit correction: this is now ONE component,
 * not two files copying each other's CSS by hand.
 *
 * OWNS the SHELL only: a single ellipsis trigger button (hover-revealed via
 * `:host-context(mur-tree-row:hover)`/`:focus-within`, so it lights up
 * exactly when the row it's projected into is hovered/focused — no feature
 * file has to re-declare that rule), the dropdown panel's OPEN/CLOSE state,
 * outside-click + Escape dismissal, and the panel's visual chrome — which is
 * simply the app's existing global `.menu`/`.menu-item`/`.menu-item-danger`/
 * `.menu-group` class primitives (`design-system/primitives.css`, already
 * OPAQUE `--surface-overlay` per T3 — the same visual language the note
 * editor's own `⋯` "more" menu and the old Meetings kebab both used). This
 * component does NOT invent new panel CSS — it only supplies the trigger +
 * positioning + open-state, and content-projects the actual items.
 *
 * Feature-owned via `<ng-content>`: WHICH actions appear and what each one
 * does (Rename / lock-state / Delete-with-confirm) — same shell-vs-logic
 * split `<mur-sidebar-section>` already established. The shell automatically
 * closes after an enabled `[role=menuitem]` is activated, including when that
 * item launches a multi-step flow elsewhere.
 *
 * Usage:
 * ```html
 * <mur-row-menu [label]="'Actions for ' + node().name">
 *   <button type="button" class="menu-item" role="menuitem"
 *     (click)="startRename()">Rename</button>
 *   <button type="button" class="menu-item menu-item-danger" role="menuitem"
 *     (click)="startDelete()">Delete</button>
 * </mur-row-menu>
 * ```
 */
@Component({
  selector: "mur-row-menu",
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: {
    "[class.is-open]": "isOpen()",
    "[class.is-prominent]": "prominent()",
    "(document:click)": "onDocumentClick($event)",
    "(document:keydown.escape)": "onEscape()",
  },
  imports: [RepositionOnScrollDirective, TeleportToBodyDirective],
  templateUrl: "./row-menu.component.html",
  styleUrl: "./row-menu.component.scss",
})
export class MurRowMenuComponent {
  private readonly host = inject(ElementRef<HTMLElement>);
  private readonly injector = inject(Injector);
  private readonly trigger =
    viewChild.required<ElementRef<HTMLButtonElement>>("trigger");
  private readonly panel = viewChild<ElementRef<HTMLElement>>("panel");

  /** Accessible name for the trigger + panel (e.g. "Actions for Product"). */
  readonly label = input.required<string>();
  /** Disables the trigger (mirrors a row's `busy` guard). */
  readonly disabled = input(false);
  /**
   * Opt OUT of the quiet, hover-revealed trigger. The default suits a tree row,
   * where the ellipsis should stay faint until the row is hovered; a menu that
   * stands ALONE (a page header's actions) has no row to hover, so at 0.45
   * opacity and 21px it reads as disabled. Prominent gives it full contrast and
   * a touch target sized for a header. Everything else is identical — one
   * control, two placements, rather than a second ellipsis menu.
   */
  readonly prominent = input(false);

  private readonly _open = signal(false);
  /** Whether the dropdown panel is showing. */
  readonly isOpen = this._open.asReadonly();
  private readonly _panelPositioned = signal(false);
  /** Prevents an unanchored first paint while the teleported panel is measured. */
  readonly panelPositioned = this._panelPositioned.asReadonly();
  private readonly _panelLeft = signal(0);
  readonly panelLeft = this._panelLeft.asReadonly();
  private readonly _panelTop = signal(0);
  readonly panelTop = this._panelTop.asReadonly();
  private readonly _panelMaxHeight = signal<number | null>(null);
  /** Maximum visible height inside the active viewport/scroll boundary. */
  readonly panelMaxHeight = this._panelMaxHeight.asReadonly();
  private focusPanelOnOpen = false;

  /** Pointer activation opens/closes without stealing focus from the clicked trigger. */
  onTriggerClick(event: MouseEvent): void {
    // Programmatic/assistive activation has detail=0 and should receive the
    // same keyboard focus treatment. Pointer clicks keep focus on the trigger.
    this.toggle(event.detail === 0);
  }

  /** Standard menu-button keyboard entry: Enter, Workspace or ArrowDown. */
  onTriggerKeydown(event: KeyboardEvent): void {
    if (!["Enter", " ", "ArrowDown"].includes(event.key)) {
      return;
    }
    event.preventDefault();
    if (this._open()) {
      this.enabledMenuItems()[0]?.focus();
      return;
    }
    this.toggle(true);
  }

  private toggle(focusPanel: boolean): void {
    if (this.disabled()) {
      return;
    }
    if (this._open()) {
      this.close();
      return;
    }
    this.focusPanelOnOpen = focusPanel;
    this._panelPositioned.set(false);
    this._open.set(true);
    this.positionPanel();
  }

  /** Close the panel. Safe to call whether or not it's open. */
  close(): void {
    this._open.set(false);
    this._panelPositioned.set(false);
    this._panelMaxHeight.set(null);
    this.focusPanelOnOpen = false;
  }

  /** Close after any enabled projected action; feature code still owns the action. */
  onPanelClick(event: MouseEvent): void {
    if (!this._open()) {
      return;
    }
    const target = event.target;
    if (!(target instanceof Element)) {
      return;
    }
    const item = target.closest<HTMLElement>("[role='menuitem']");
    if (
      !item ||
      item.getAttribute("aria-disabled") === "true" ||
      (item instanceof HTMLButtonElement && item.disabled)
    ) {
      return;
    }
    this.close();
  }

  onPanelKeydown(event: KeyboardEvent): void {
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
      return;
    }
    const items = this.enabledMenuItems();
    if (items.length === 0) {
      return;
    }
    event.preventDefault();
    const current = document.activeElement;
    const currentIndex = items.findIndex((item) => item === current);
    const nextIndex =
      event.key === "Home"
        ? 0
        : event.key === "End"
          ? items.length - 1
          : event.key === "ArrowDown"
            ? (currentIndex + 1 + items.length) % items.length
            : (currentIndex - 1 + items.length) % items.length;
    items[nextIndex]?.focus();
  }

  /** Outside-click dismissal — a click landing outside this host closes the panel. */
  onDocumentClick(event: MouseEvent): void {
    if (!this._open()) {
      return;
    }
    const target = event.target as Node | null;
    if (
      target &&
      (this.host.nativeElement.contains(target) ||
        this.panel()?.nativeElement.contains(target))
    ) {
      return; // inside the trigger or teleported panel — let its own click run.
    }
    this.close();
  }

  onEscape(): void {
    if (this._open()) {
      this.close();
      this.trigger().nativeElement.focus();
    }
  }

  onViewportChange(): void {
    if (this._open()) {
      this._panelPositioned.set(false);
      this.positionPanel();
    }
  }

  /**
   * Prefer the familiar below-trigger placement, but flip above when the panel
   * would cross the nearest scrolling ancestor (or the viewport). The hierarchy
   * sidebar has a fixed footer outside its `.context-body`; viewport-only math
   * therefore still lets the last row's menu paint underneath that footer.
   */
  private positionPanel(): void {
    afterNextRender(
      () => {
        const panel = this.panel()?.nativeElement;
        if (!panel || !this._open()) {
          return;
        }

        const anchorRect = this.trigger().nativeElement.getBoundingClientRect();
        let boundaryTop = VIEWPORT_MARGIN_PX;
        let boundaryBottom = window.innerHeight - VIEWPORT_MARGIN_PX;

        for (
          let ancestor = this.host.nativeElement.parentElement;
          ancestor;
          ancestor = ancestor.parentElement
        ) {
          const overflowY = getComputedStyle(ancestor).overflowY;
          if (overflowY !== "auto" && overflowY !== "scroll") {
            continue;
          }
          const rect = ancestor.getBoundingClientRect();
          boundaryTop = Math.max(boundaryTop, rect.top);
          boundaryBottom = Math.min(boundaryBottom, rect.bottom);
        }

        const below = Math.max(
          0,
          boundaryBottom - anchorRect.bottom - PANEL_GAP_PX,
        );
        const above = Math.max(0, anchorRect.top - boundaryTop - PANEL_GAP_PX);
        const flipAbove = panel.offsetHeight > below && above > below;
        const availableHeight = Math.floor(flipAbove ? above : below);
        const viewportLeft = Math.max(
          VIEWPORT_MARGIN_PX,
          Math.min(
            anchorRect.right - panel.offsetWidth,
            window.innerWidth - panel.offsetWidth - VIEWPORT_MARGIN_PX,
          ),
        );
        const viewportTop = flipAbove
          ? anchorRect.top -
            Math.min(panel.offsetHeight, availableHeight) -
            PANEL_GAP_PX
          : anchorRect.bottom + PANEL_GAP_PX;

        this._panelLeft.set(Math.round(viewportLeft));
        this._panelTop.set(Math.round(Math.max(boundaryTop, viewportTop)));
        this._panelMaxHeight.set(availableHeight);
        this._panelPositioned.set(true);
        if (this.focusPanelOnOpen) {
          // Consume the one-shot request before scheduling focus. Positioning
          // can run again on a nested-scroll/resize render; leaving the flag
          // set would race keyboard navigation and pull focus back to item 1.
          this.focusPanelOnOpen = false;
          afterNextRender(
            () => {
              if (this._open()) {
                this.enabledMenuItems()[0]?.focus();
              }
            },
            { injector: this.injector },
          );
        }
      },
      { injector: this.injector },
    );
  }

  private enabledMenuItems(): HTMLElement[] {
    const panel = this.panel()?.nativeElement;
    if (!panel) {
      return [];
    }
    return Array.from(
      panel.querySelectorAll<HTMLElement>("[role='menuitem']"),
    ).filter(
      (item) =>
        item.getAttribute("aria-disabled") !== "true" &&
        (!(item instanceof HTMLButtonElement) || !item.disabled),
    );
  }
}
