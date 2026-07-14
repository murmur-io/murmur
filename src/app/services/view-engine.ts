import type {
  Meeting,
  MeetingActionSummary,
  ViewConfig,
  ViewFilter,
  ViewSort,
} from "../core/models";

/**
 * A meeting row enriched with its merged action-item counts, ready for the
 * Table/Board views. This is the ROW SHAPE the {@link ViewEngine} emits and
 * the view components render — it carries the same (already-gated) `Meeting`
 * the backend returned, never any additional content.
 */
export interface ViewRow {
  /** The masked-or-real meeting exactly as the gated list command returned it. */
  meeting: Meeting;
  /** Open action items (0 when no summary — incl. a locked meeting the backend omits). */
  openCount: number;
  /** Done action items (0 when no summary). */
  doneCount: number;
}

/** One selectable field a view can filter / sort / show a column by. */
export interface ViewField {
  readonly id: string;
  readonly label: string;
  /**
   * `text` fields support eq/neq/contains/isEmpty/isNotEmpty; `date` fields
   * support before/after/isEmpty/isNotEmpty; `status` is an enum (eq/neq).
   * Drives which ops the filter menu offers and how a value is compared.
   */
  readonly type: "text" | "date" | "status" | "number";
}

/**
 * The fields available on a meetings saved view. Deliberately a small, fixed
 * set drawn from the {@link Meeting} DTO the gated list command returns
 * (title / date / folder / status / duration) plus the merged action counts —
 * no field here can read anything the backend didn't already disclose.
 */
export const MEETING_VIEW_FIELDS: readonly ViewField[] = [
  { id: "title", label: "Title", type: "text" },
  { id: "date", label: "Date", type: "date" },
  { id: "folder", label: "Folder", type: "text" },
  { id: "status", label: "Status", type: "status" },
  { id: "duration", label: "Duration", type: "number" },
  { id: "open", label: "Open actions", type: "number" },
  { id: "done", label: "Done actions", type: "number" },
];

/**
 * Pure, deterministic view engine. Given the (already-gated) meeting rows, a
 * parsed {@link ViewConfig}, and the action summaries, it returns the
 * filtered+sorted rows for a Table view. NO IPC, NO signals, NO mutation of the
 * inputs — a plain function so it's trivial to reason about and unit-test.
 * (Board layout was removed 2026-07-14.)
 *
 * It NEVER unmasks: it only reads the fields the backend already disclosed on a
 * `Meeting` (a locked meeting arrives with a masked title "🔒 Locked", null
 * folderId, `LOCKED`/whatever status the gate set) and re-presents them. A
 * locked row stays exactly as masked in every view.
 */
export class ViewEngine {
  /**
   * Build the enriched, filtered, sorted rows for a Table view.
   * `folderName(meeting)` resolves a display folder name (the component owns
   * the folder tree, so the name lookup is injected rather than duplicated).
   */
  static rows(
    meetings: readonly Meeting[],
    config: ViewConfig,
    summaries: readonly MeetingActionSummary[],
    folderName: (m: Meeting) => string | null,
  ): ViewRow[] {
    const byId = summaryMap(summaries);
    const enriched: ViewRow[] = meetings.map((meeting) => {
      const s = byId.get(meeting.id);
      return {
        meeting,
        openCount: s?.openCount ?? 0,
        doneCount: s?.doneCount ?? 0,
      };
    });
    const filtered = enriched.filter((row) =>
      config.filters.every((f) => matches(row, f, folderName)),
    );
    return sortRows(filtered, config.sort, folderName);
  }
}

function summaryMap(
  summaries: readonly MeetingActionSummary[],
): Map<string, MeetingActionSummary> {
  const map = new Map<string, MeetingActionSummary>();
  for (const s of summaries) {
    map.set(s.meetingId, s);
  }
  return map;
}

/** The comparable value of a field on a row — used by both filter + sort. */
function fieldValue(
  row: ViewRow,
  field: string,
  folderName: (m: Meeting) => string | null,
): string | number | null {
  const m = row.meeting;
  switch (field) {
    case "title":
      return m.title ?? "";
    case "date": {
      const t = Date.parse(m.startedAt);
      return Number.isNaN(t) ? null : t;
    }
    case "folder":
      return folderName(m);
    case "status":
      return m.status;
    case "duration":
      return m.durationS;
    case "open":
      return row.openCount;
    case "done":
      return row.doneCount;
    default:
      return null;
  }
}

/** Whether a row passes one filter clause. An unknown op is a no-op (keeps the row). */
function matches(
  row: ViewRow,
  filter: ViewFilter,
  folderName: (m: Meeting) => string | null,
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
      // Compare against a date field's epoch-ms value; a non-date field or an
      // unparseable filter value drops the row from a date comparison.
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
  rows: ViewRow[],
  sorts: ViewSort[],
  folderName: (m: Meeting) => string | null,
): ViewRow[] {
  if (sorts.length === 0) {
    return rows;
  }
  // Decorate-sort-undecorate keeps it stable regardless of the JS engine.
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
  // Nulls sort last regardless of direction's later negation? Keep simple:
  // null is treated as the smallest, so an asc sort puts empties first.
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
