import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
  output,
} from "@angular/core";

/**
 * The leading glyph a tree row can show.
 *
 * The four item kinds were added with the workspace hierarchy, where a single
 * tree renders containers AND the items inside them. Before that every row in
 * both trees was a folder, so two variants were enough; a meeting and a note
 * sharing the folder glyph in one list makes the list unreadable.
 */
export type TreeRowIcon =
  | "folder"
  | "locked"
  | "space"
  | "meeting"
  | "note"
  | "task"
  | "dashboard";

/**
 * Design System — <mur-tree-row>: THE folder-tree row (extracted 2026-07-12,
 * one level deeper than the `<mur-sidebar-section>` shell consolidation, after
 * the user correctly called out that the per-folder rows were STILL two
 * separately-authored implementations — `FolderRowComponent`'s `.row-line`/
 * `.folder` vs `NotesSidebarTreeComponent`'s `.tree-row`/`.tree-select` —
 * with visibly different pill placement, paddings and heights).
 *
 * OWNS 100% of the row's VISUAL BOX so divergence is structurally impossible:
 * the container (padding, radius, hover, the whole-row shell-active selected
 * pill), the per-depth indent (`--tree-indent` × `depth`), the leading GUTTER
 * (a disclosure caret when `expandable`, an equal-width spacer otherwise — a
 * FIXED slot for BOTH trees, so a Meetings row and a Notes row at the same
 * depth start their icon/label at the exact same x), the icon (folder/lock
 * glyph), the label, and an optional count chip. The drop-target visual
 * states (`.is-drop-armed`/`.is-drop-target`, classes set by the feature-side
 * `FolderDropDirective` when a consumer attaches it to this host) are styled
 * here too — by CLASS NAME only; this component imports nothing from
 * `features/` (no T2 cycle risk: `FolderRowComponent` keeps owning its own
 * recursion into `FolderTreeComponent` and merely delegates row RENDERING
 * here, a leaf dependency).
 *
 * Everything genuinely feature-specific stays OUTSIDE and projects in as
 * trailing content (`<ng-content>`): Meetings' lock-toggle + ⋯ menu, Notes'
 * hover-revealed action cluster. Consumers reveal that cluster on row hover
 * with their own `mur-tree-row:hover .their-cluster { opacity: 1 }` rule.
 *
 * Usage (Meetings, recursive — the drop directive rides the host):
 * ```html
 * <mur-tree-row
 *   [label]="node().name" [depth]="depth()" [count]="node().noteCount"
 *   [selected]="isSelected()" [expandable]="childCount() > 0"
 *   [expanded]="expanded()" (toggleExpand)="toggleExpanded()"
 *   (activate)="selected.emit(node().id)"
 *   appFolderDrop [dropFolderId]="node().id" (dropNote)="…"
 * >
 *   <div class="row-actions">…lock toggle, ⋯ menu…</div>
 * </mur-tree-row>
 * ```
 */
@Component({
  selector: "mur-tree-row",
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: {
    "[class.is-selected]": "selected()",
    "[style.padding-left]": "indent()",
  },
  templateUrl: "./tree-row.component.html",
  styleUrl: "./tree-row.component.scss",
})
export class MurTreeRowComponent {
  /** The row's visible name. */
  readonly label = input.required<string>();
  /** Nesting depth (0 = top level) — indents by `depth × var(--tree-indent)`. */
  readonly depth = input(0);
  /** Whether the row renders the whole-row shell-active selected pill. */
  readonly selected = input(false);
  /** The leading glyph — `"locked"` for a sealed folder, `"folder"` otherwise. */
  readonly icon = input<TreeRowIcon>("folder");
  /**
   * A user-chosen emoji that REPLACES the glyph (workspace Workspaces can carry
   * one). It stands in the icon's slot rather than beside it, so a row with an
   * emoji and a row without still align.
   */
  readonly emoji = input<string | null>(null);
  /** Optional trailing count chip (e.g. notes-in-folder); null hides it. */
  readonly count = input<number | null>(null);
  /**
   * Whether the leading gutter shows a disclosure caret (a folder with
   * children). When false the SAME-WIDTH spacer renders instead, so icons
   * align across expandable and leaf rows — and across BOTH trees.
   */
  readonly expandable = input(false);
  /** Caret state (only meaningful while `expandable`). */
  readonly expanded = input(false);

  /** The main row button was clicked (select this folder). */
  readonly activate = output<void>();
  /** The disclosure caret was clicked. */
  readonly toggleExpand = output<void>();

  /** Host `padding-left` — the per-depth indent, from the shared token. */
  protected readonly indent = computed(
    () => `calc(${this.depth()} * var(--tree-indent))`,
  );
}
