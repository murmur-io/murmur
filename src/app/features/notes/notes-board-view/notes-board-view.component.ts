import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import { MurCardComponent } from "../../../design-system/card/card.component";
import type {
  PropertySchemaField,
  PropertyValue,
  TypedNoteRow,
} from "../../../core/models";

/** One board column: a group value (a select option, or the "No value" bucket) + its rows. */
interface BoardGroup {
  key: string;
  label: string;
  rows: TypedNoteRow[];
}

/** The empty bucket label + key for rows whose group field has no value. */
const NO_VALUE_KEY = "__none__";

/**
 * Feature C — the BOARD (kanban) view over a note-folder's TYPED notes. Groups
 * the rows by the folder's FIRST `select`-kind field: one column per that
 * field's schema option (plus a "No value" column for rows with none, and any
 * out-of-schema values encountered). When the folder has NO select field there
 * is nothing to group by, so it renders an empty-state prompting to add one.
 * Presentation only — no data, no IPC; every card is a (backend-gated)
 * {@link TypedNoteRow}, so a sealed folder shows nothing. Card title → the
 * editor (routerLink `/notes/:id`).
 */
@Component({
  selector: "app-notes-board-view",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink, MurCardComponent],
  templateUrl: "./notes-board-view.component.html",
  styleUrl: "./notes-board-view.component.scss",
})
export class NotesBoardViewComponent {
  readonly rows = input.required<TypedNoteRow[]>();
  readonly schema = input.required<PropertySchemaField[]>();

  /** The field the board groups by — the folder's first `select` field, or null. */
  readonly groupField = computed<PropertySchemaField | null>(
    () => this.schema().find((f) => f.kind === "select") ?? null,
  );

  /**
   * The board columns: one per the group field's schema options (in order), a
   * trailing "No value" bucket, and a column for any out-of-schema value found on
   * a row (so a passthrough value is never hidden). Empty columns are kept so the
   * board always shows the full option set. Null when there is no group field.
   */
  readonly groups = computed<BoardGroup[]>(() => {
    const field = this.groupField();
    if (!field) {
      return [];
    }
    const rows = this.rows();
    // Build the ordered set of group values: schema options first, then any
    // extra (out-of-schema) value present on a row, then the empty bucket.
    const order: string[] = [...field.options];
    const seen = new Set(order);
    for (const row of rows) {
      const v = this.groupValue(row, field.key);
      if (v !== "" && !seen.has(v)) {
        seen.add(v);
        order.push(v);
      }
    }
    const columns: BoardGroup[] = order.map((opt) => ({
      key: opt,
      label: opt,
      rows: rows.filter((r) => this.groupValue(r, field.key) === opt),
    }));
    columns.push({
      key: NO_VALUE_KEY,
      label: "No value",
      rows: rows.filter((r) => this.groupValue(r, field.key) === ""),
    });
    return columns;
  });

  readonly trackByGroup = (g: BoardGroup): string => g.key;
  readonly trackByRow = (row: TypedNoteRow): string => row.id;

  /** The row's value for the group field as a plain string ("" when absent/off-kind). */
  private groupValue(row: TypedNoteRow, key: string): string {
    const v: PropertyValue | undefined = row.values[key];
    return v && v.kind === "select" ? v.value : "";
  }
}
