import { ChangeDetectionStrategy, Component, computed, inject, input, output } from "@angular/core";
import { MurTableComponent } from "../../../design-system/table/table.component";
import { MurTableColumnComponent } from "../../../design-system/table/table-column.component";
import { LockBadgeComponent } from "../../folders/lock-badge/lock-badge.component";
import type { FolderExposure } from "../../../services/folders.service";
import {
  meetingStatusLabel,
  meetingStatusPillClass,
} from "../../../design-system/meeting-status";
import type { Meeting } from "../../../core/models";
import type { ViewRow } from "../../../services/view-engine";
import { DateFormatService } from "../../../core/date-format.service";

/** A {@link ViewRow} plus its pre-derived status-pill presentation. */
export interface DecoratedViewRow extends ViewRow {
  readonly statusPillClass: string;
  readonly statusLabel: string;
}

/** One resolved column: its field id + header label. */
interface TableColumn {
  key: string;
  label: string;
  alignEnd: boolean;
  width: string | null;
}

/** Human labels + alignment for the known meeting columns (in one place). */
const COLUMN_META: Record<
  string,
  { label: string; alignEnd: boolean; width: string | null }
> = {
  title: { label: "Title", alignEnd: false, width: null },
  date: { label: "Date", alignEnd: false, width: "180px" },
  folder: { label: "Folder", alignEnd: false, width: "160px" },
  status: { label: "Status", alignEnd: false, width: "140px" },
  duration: { label: "Duration", alignEnd: true, width: "110px" },
  actions: { label: "Actions", alignEnd: true, width: "170px" },
  // "tags" is accepted (contract column) but the gated `Meeting` list DTO
  // carries no per-meeting tags, so its cell renders empty — see class doc.
  tags: { label: "Tags", alignEnd: false, width: "160px" },
};

/**
 * Feature B — the TABLE saved view over the meetings list. A thin presentation
 * layer over `<mur-table>`: it renders the {@link ViewRow}s the ViewEngine
 * produced against the columns the {@link ViewConfig} named. It owns NO data
 * and NO IPC — every row is the (already-gated) `Meeting` the backend returned,
 * re-presented; a locked meeting stays masked (title "•••", `isMasked` true)
 * exactly as the default list shows it, in every column.
 *
 * NOTE ON `tags`: the meetings list command returns a `Meeting` DTO WITHOUT
 * per-meeting tags (tags are a note/document concept), so a `tags` column
 * renders an empty cell rather than fabricating a value. Board grouping is
 * likewise limited to status/folder for the same reason.
 */
@Component({
  selector: "app-meetings-table-view",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurTableComponent, MurTableColumnComponent, LockBadgeComponent],
  templateUrl: "./meetings-table-view.component.html",
  styleUrl: "./meetings-table-view.component.scss",
})
export class MeetingsTableViewComponent {
  private readonly dates = inject(DateFormatService);

  /** Filtered+sorted rows from the ViewEngine (each carries a masked-or-real Meeting). */
  readonly rows = input.required<ViewRow[]>();
  /** The visible column field-ids, in order, from the active ViewConfig. */
  readonly columns = input.required<string[]>();

  /** Resolve a meeting's folder display name (component owns the folder tree). */
  readonly folderNameOf = input.required<(m: Meeting) => string | null>();
  /** Resolve a meeting's folder lock exposure (null = open / at root). */
  readonly folderExposureOf = input.required<(m: Meeting) => FolderExposure | null>();
  /** Whether a meeting's title must be masked (sealed & not session-unlocked). */
  readonly isMasked = input.required<(m: Meeting) => boolean>();

  /** A title click asks the parent to open the meeting as a tab (mirrors the list). */
  readonly openMeeting = output<{ event: Event; meeting: Meeting }>();

  /** The resolved column definitions (unknown ids fall back to a title-esque cell). */
  readonly resolvedColumns = computed<TableColumn[]>(() =>
    this.columns().map((key) => {
      const meta = COLUMN_META[key] ?? {
        label: key,
        alignEnd: false,
        width: null,
      };
      return { key, label: meta.label, alignEnd: meta.alignEnd, width: meta.width };
    }),
  );

  /**
   * The rows fed to `<mur-table>`, each carrying its status-pill presentation
   * already derived — the cell template must not call a helper per row (a
   * method binding re-runs on every change-detection pass; a `computed` is
   * cached and dependency-tracked).
   */
  readonly decoratedRows = computed<DecoratedViewRow[]>(() =>
    this.rows().map((row) => ({
      ...row,
      statusPillClass: meetingStatusPillClass(row.meeting.status),
      statusLabel: meetingStatusLabel(row.meeting.status),
    })),
  );

  /** `@for` / `mur-table` track key — stable per meeting. */
  readonly trackByRow = (row: ViewRow): string => row.meeting.id;

  /** Per-row extra classes for `<mur-table>` (masked rows read muted). */
  readonly rowClassFor = (row: ViewRow): Record<string, boolean> => ({
    "is-muted": this.isMasked()(row.meeting),
  });

  onOpen(event: Event, meeting: Meeting): void {
    this.openMeeting.emit({ event, meeting });
  }

  /** Formatted through {@link DateFormatService} — the one place a date becomes user-visible text. */
  formatDate(startedAt: string): string {
    return this.dates.day(startedAt);
  }

  formatDuration(durationS: number): string {
    const total = Math.max(0, Math.round(durationS));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    if (h > 0) {
      return `${h}h ${m}m`;
    }
    if (m > 0) {
      return `${m}m ${s}s`;
    }
    return `${s}s`;
  }
}
