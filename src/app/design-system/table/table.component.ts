import {
  ChangeDetectionStrategy,
  Component,
  contentChildren,
  input,
} from "@angular/core";
import { NgTemplateOutlet } from "@angular/common";
import { MurTableColumnComponent } from "./table-column.component";

/**
 * Design System — <mur-table>: a dense, database/Notion-style data table
 * (2026-07-12, extracted from `NotesHomeComponent`'s inline `<table>` per
 * §6b — reusable UI belongs here, not rolled one-off per feature).
 *
 * SCOPE (built for what `notes-home` actually needs, not a speculative grid
 * system): an arbitrary number of `<mur-table-column>` definitions, each
 * projecting rich per-row cell content via an `<ng-template>` (icons, pills,
 * buttons — not just plain text), rendered against a flat `rows` array.
 *
 * ROW HEIGHT IS A DESIGN CONSTANT THIS COMPONENT OWNS (the point of the
 * extraction, 2026-07-12): every `tbody tr` is a FIXED height regardless of
 * cell content (`table.component.scss`) — a long snippet, a long title, or a
 * lock glyph can never make one row taller than another. Consumers are not
 * responsible for remembering to truncate; overflowing cell content is
 * clipped (`overflow: hidden` — see the scss note), not wrapped.
 *
 * Usage:
 * ```html
 * <mur-table [rows]="listItems()" [trackBy]="trackById">
 *   <mur-table-column key="title" header="Title">
 *     <ng-template let-row>{{ row.title }}</ng-template>
 *   </mur-table-column>
 *   <mur-table-column key="date" header="Last modified" width="160px" alignEnd>
 *     <ng-template let-row>{{ formatDate(row) }}</ng-template>
 *   </mur-table-column>
 *   <mur-table-column key="actions" header="Actions" [hideHeader]="true" width="44px">
 *     <ng-template let-row><button (click)="…">⋮</button></ng-template>
 *   </mur-table-column>
 * </mur-table>
 * ```
 */
@Component({
  selector: "mur-table",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [NgTemplateOutlet],
  templateUrl: "./table.component.html",
  styleUrl: "./table.component.scss",
})
export class MurTableComponent<T = unknown> {
  /** The row data — plain objects, shape owned entirely by the column templates. */
  readonly rows = input.required<readonly T[]>();
  /** `@for` track key — REQUIRED (Murmur's convention: never `track $index` for keyed data). */
  readonly trackBy = input.required<(row: T) => unknown>();
  /**
   * Per-row extra class(es) — e.g. a locked/masked row, or the row whose
   * overflow menu is open needing to sit above the click-away scrim. Kept as
   * a function (not a static class binding) since it depends on the row.
   */
  readonly rowClass = input<(row: T) => Record<string, boolean>>(() => ({}));

  /** The projected `<mur-table-column>` definitions, in declaration order. */
  readonly columns = contentChildren(MurTableColumnComponent);
}
