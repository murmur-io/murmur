import type {
  NotesListItem,
  ViewConfig,
  ViewFilter,
  ViewSort,
} from "../core/models";
import type { ViewField } from "./view-engine";

/**
 * The fields available on a NOTES saved view. A small, fixed set drawn from the
 * already-gated note/org rows the pane holds (title / folder / last-modified /
 * tags / shared) — no field reads anything the backend didn't already disclose,
 * and a masked (locked) note row re-presents exactly as masked ("🔒 Locked"
 * title, empty tags), so a view can never unmask it.
 */
export const NOTE_VIEW_FIELDS: readonly ViewField[] = [
  { id: "title", label: "Title", type: "text" },
  { id: "folder", label: "Folder", type: "text" },
  { id: "updated", label: "Last modified", type: "date" },
  { id: "tags", label: "Tags", type: "text" },
  { id: "shared", label: "Shared", type: "status" },
];

/**
 * Pure, deterministic view engine for the Notes list — the twin of the meetings
 * {@link import("./view-engine").ViewEngine}, operating on the {@link
 * NotesListItem} union instead of `Meeting`. Given the (already-gated) rows and
 * a parsed {@link ViewConfig}, it returns the filtered+sorted rows for a Table
 * view. NO IPC, NO signals, NO mutation of the inputs.
 *
 * It NEVER unmasks: it only reads the fields the row already carries (a locked
 * note arrives with a masked title + empty tags), so a locked row stays exactly
 * as masked in every view. `folderName(item)` resolves a display folder name
 * (the component owns the folder tree, so the lookup is injected).
 */
export class NotesViewEngine {
  static rows(
    items: readonly NotesListItem[],
    config: ViewConfig,
    folderName: (item: NotesListItem) => string | null,
  ): NotesListItem[] {
    const filtered = items.filter((row) =>
      config.filters.every((f) => matches(row, f, folderName)),
    );
    return sortRows(filtered, config.sort, folderName);
  }
}

/** The comparable value of a field on a row — used by both filter + sort. */
function fieldValue(
  row: NotesListItem,
  field: string,
  folderName: (item: NotesListItem) => string | null,
): string | number | null {
  switch (field) {
    case "title":
      return row.kind === "note" ? (row.note.title ?? "") : row.item.title;
    case "folder":
      // Org rows have no note-folder (they're a Shared-Brain replica); a null
      // folder is treated as empty by the filter/sort helpers.
      return row.kind === "note" ? folderName(row) : null;
    case "updated":
      // `sortAt` is the epoch-ms of the last edit (note) / creation (org),
      // already normalized by the component when it built the union.
      return row.sortAt;
    case "tags":
      return row.kind === "note" ? row.note.tags.join(" ") : "";
    case "shared":
      // A note carries its own share flag; an org row is inherently shared.
      return row.kind === "note"
        ? row.note.shared
          ? "true"
          : "false"
        : "true";
    default:
      return null;
  }
}

/** Whether a row passes one filter clause. An unknown op is a no-op (keeps the row). */
function matches(
  row: NotesListItem,
  filter: ViewFilter,
  folderName: (item: NotesListItem) => string | null,
): boolean {
  const raw = fieldValue(row, filter.field, folderName);
  const empty = raw === null || raw === "";
  switch (filter.op) {
    case "isEmpty":
      return empty;
    case "isNotEmpty":
      return !empty;
    case "eq":
      return String(raw ?? "").toLowerCase() === filter.value.toLowerCase();
    case "neq":
      return String(raw ?? "").toLowerCase() !== filter.value.toLowerCase();
    case "contains":
      return String(raw ?? "")
        .toLowerCase()
        .includes(filter.value.toLowerCase());
    case "before":
    case "after": {
      if (typeof raw !== "number") {
        return false;
      }
      const bound = Date.parse(filter.value);
      if (Number.isNaN(bound)) {
        return false;
      }
      return filter.op === "before" ? raw < bound : raw > bound;
    }
    default:
      return true; // unknown op → no-op, never silently drops rows.
  }
}

/** Stable multi-key sort over the sort clauses (empty ⇒ input order preserved). */
function sortRows(
  rows: NotesListItem[],
  sorts: ViewSort[],
  folderName: (item: NotesListItem) => string | null,
): NotesListItem[] {
  if (sorts.length === 0) {
    return rows;
  }
  return rows
    .map((row, index) => ({ row, index }))
    .sort((a, b) => {
      for (const s of sorts) {
        const cmp = compareValues(
          fieldValue(a.row, s.field, folderName),
          fieldValue(b.row, s.field, folderName),
        );
        if (cmp !== 0) {
          return s.direction === "asc" ? cmp : -cmp;
        }
      }
      return a.index - b.index;
    })
    .map((d) => d.row);
}

function compareValues(
  a: string | number | null,
  b: string | number | null,
): number {
  if (a === null && b === null) return 0;
  if (a === null) return -1;
  if (b === null) return 1;
  if (typeof a === "number" && typeof b === "number") {
    return a - b;
  }
  const sa = String(a).toLowerCase();
  const sb = String(b).toLowerCase();
  return sa < sb ? -1 : sa > sb ? 1 : 0;
}
