import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import { MurTableComponent } from "../../../design-system/table/table.component";
import { MurTableColumnComponent } from "../../../design-system/table/table-column.component";
import type {
  PropertySchemaField,
  PropertyValue,
  TypedNoteRow,
} from "../../../core/models";

/** One resolved property column: the schema field it renders. */
interface PropColumn {
  key: string;
  label: string;
  field: PropertySchemaField;
}

/**
 * Feature C — the TABLE view over a note-folder's TYPED notes. A thin
 * presentation layer over `<mur-table>`: it renders one column PER folder-schema
 * field (cell switches on the row value's {@link PropertyValue} kind) plus a
 * fixed Title column (routerLink `/notes/:id`) and an "Updated" column. It owns
 * NO data + NO IPC — every row is a (backend-gated) {@link TypedNoteRow}; a
 * sealed folder yields no rows, so a locked folder shows no typed view at all.
 * No inline cell editing in v1 — the row's title links to the editor.
 */
@Component({
  selector: "app-notes-table-view",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink, MurTableComponent, MurTableColumnComponent],
  templateUrl: "./notes-table-view.component.html",
  styleUrl: "./notes-table-view.component.scss",
})
export class NotesTableViewComponent {
  /** The typed rows for the active folder (each carries its property values). */
  readonly rows = input.required<TypedNoteRow[]>();
  /** The active folder's property schema — one column per field, in order. */
  readonly schema = input.required<PropertySchemaField[]>();

  /** One column per schema field (unknown value kinds fall back to text render). */
  readonly propColumns = computed<PropColumn[]>(() =>
    this.schema().map((field) => ({
      key: field.key,
      label: field.key,
      field,
    })),
  );

  /** `mur-table` track key — stable per note id. */
  readonly trackByRow = (row: TypedNoteRow): string => row.id;

  /** The typed value for a row's property key (undefined when the note has none). */
  valueOf(row: TypedNoteRow, key: string): PropertyValue | undefined {
    return row.values[key];
  }

  /** True when a checkbox value is checked (safe on a missing / off-kind value). */
  isChecked(value: PropertyValue | undefined): boolean {
    return value?.kind === "checkbox" && value.value === true;
  }

  /** The display string for a non-checkbox value (empty for a missing value). */
  displayValue(value: PropertyValue | undefined): string {
    if (!value) {
      return "";
    }
    switch (value.kind) {
      case "number":
        return Number.isFinite(value.value) ? String(value.value) : "";
      case "checkbox":
        return value.value ? "Yes" : "No";
      default:
        return value.value;
    }
  }

  /** Presentational only: epoch-ms → a friendly local date. */
  formatDate(updatedAt: number): string {
    const d = new Date(updatedAt);
    if (Number.isNaN(d.getTime())) {
      return "";
    }
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  }
}
