import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  inject,
  input,
  signal,
} from "@angular/core";

/**
 * Design System — `<mur-row-menu>`: THE gear-trigger dropdown for a tree row's
 * per-item actions (extracted 2026-07-12, after Meetings' folder rows and
 * Notes' folder rows drifted into TWO differently-styled action clusters
 * twice in one day — first a ⋯ kebab-vs-flat-icons split, then a "flat icons
 * that measured the same size but read visibly different in color/weight"
 * near-miss. Per the user's explicit correction: this is now ONE component,
 * not two files copying each other's CSS by hand.
 *
 * OWNS the SHELL only: a single ⚙ trigger button (hover-revealed via
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
 * split `<mur-sidebar-section>` already established. A menu item that
 * launches a multi-step flow (e.g. a delete confirm that replaces the row
 * entirely) should close this menu FIRST via the local template reference
 * (`#actions` → `(click)="actions.close(); startDelete()"`) since opening a
 * different template branch elsewhere doesn't reliably tear this instance
 * down before its next paint.
 *
 * Usage:
 * ```html
 * <mur-row-menu #actions [label]="'Actions for ' + node().name">
 *   <button type="button" class="menu-item" role="menuitem"
 *     (click)="actions.close(); startRename()">Rename</button>
 *   <button type="button" class="menu-item menu-item-danger" role="menuitem"
 *     (click)="actions.close(); startDelete()">Delete</button>
 * </mur-row-menu>
 * ```
 */
@Component({
  selector: "mur-row-menu",
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: {
    "[class.is-open]": "isOpen()",
    "(document:click)": "onDocumentClick($event)",
    "(document:keydown.escape)": "onEscape()",
  },
  templateUrl: "./row-menu.component.html",
  styleUrl: "./row-menu.component.scss",
})
export class MurRowMenuComponent {
  private readonly host = inject(ElementRef<HTMLElement>);

  /** Accessible name for the trigger + panel (e.g. "Actions for Product"). */
  readonly label = input.required<string>();
  /**
   * Which glyph the trigger shows. `"actions"` is the gear that opens a menu ABOUT
   * the row; `"add"` is the plus that opens a menu of things to CREATE in it. Same
   * panel, same dismissal, same opacity — only the affordance differs, and giving
   * the second its own component would be two implementations of one dropdown
   * drifting apart, which is exactly what this component was extracted to stop.
   */
  readonly icon = input<"actions" | "add">("actions");
  /** Disables the trigger (mirrors a row's `busy` guard). */
  readonly disabled = input(false);

  private readonly _open = signal(false);
  /** Whether the dropdown panel is showing. */
  readonly isOpen = this._open.asReadonly();

  /** Open/close the panel. */
  toggle(): void {
    if (this.disabled()) {
      return;
    }
    this._open.update((v) => !v);
  }

  /** Close the panel. Safe to call whether or not it's open. */
  close(): void {
    this._open.set(false);
  }

  /** Outside-click dismissal — a click landing outside this host closes the panel. */
  onDocumentClick(event: MouseEvent): void {
    if (!this._open()) {
      return;
    }
    const target = event.target as Node | null;
    if (target && this.host.nativeElement.contains(target)) {
      return; // inside the trigger or panel — let the item's own click run.
    }
    this._open.set(false);
  }

  onEscape(): void {
    if (this._open()) {
      this._open.set(false);
    }
  }
}
