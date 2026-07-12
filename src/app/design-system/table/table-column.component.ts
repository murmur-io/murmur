import { ChangeDetectionStrategy, Component, TemplateRef, contentChild, input } from "@angular/core";

/**
 * Design System — <mur-table-column>: a COLUMN DEFINITION for `<mur-table>`,
 * not a rendered element (mirrors the well-established `matColumnDef`
 * idiom — declared as `<mur-table>`'s content, captured via `contentChildren`,
 * never itself projected into the DOM: `MurTableComponent`'s own template has
 * no `<ng-content>`, so this component instantiates + is queryable, but its
 * (empty) template never appears anywhere).
 *
 * The column's cell content is an `<ng-template>` child, captured as a
 * `TemplateRef` and rendered per-row by `MurTableComponent` via
 * `NgTemplateOutlet` with `{ $implicit: row }` — so a column can hold
 * arbitrary rich content (icons, pills, buttons), not just plain text:
 *
 * ```html
 * <mur-table [rows]="items()" [trackBy]="trackById">
 *   <mur-table-column key="title" header="Title">
 *     <ng-template let-row>{{ row.title }}</ng-template>
 *   </mur-table-column>
 * </mur-table>
 * ```
 */
@Component({
  selector: "mur-table-column",
  changeDetection: ChangeDetectionStrategy.OnPush,
  // Genuinely empty — this component is a DEFINITION, never rendered (see the
  // class doc). Still its own file per the directory-per-component convention.
  templateUrl: "./table-column.component.html",
})
export class MurTableColumnComponent {
  /** Stable column identity (also used as the `@for` track key). */
  readonly key = input.required<string>();
  /** The header cell's visible text (empty + `hideHeader` for an icon-only actions column). */
  readonly header = input<string>("");
  /** Screen-reader-only header (e.g. "Actions") when the column has no visible label. */
  readonly hideHeader = input(false);
  /** A fixed CSS width for this column (e.g. `"160px"`); omit for the flexible title column. */
  readonly width = input<string | null>(null);
  /** Right-align this column's cells (dates, numbers). */
  readonly alignEnd = input(false);

  /**
   * The column's per-row cell template — `let-row` binds to the row object.
   * Untyped (`TemplateRef<any>`, Angular's own default) rather than forcing a
   * generic through both this component and `MurTableComponent<T>`: the two
   * are independently instantiated (`contentChildren` doesn't unify a content
   * child's generic with its parent's), so a tight generic here would just be
   * cosmetic. The consumer's `<ng-template let-row>` still reads naturally.
   */
  readonly cellTemplate = contentChild(TemplateRef);
}
