import {
  ChangeDetectionStrategy,
  Component,
  input,
  output,
} from "@angular/core";
import { MurCardComponent } from "../../../design-system/card/card.component";
import { LockBadgeComponent } from "../../folders/lock-badge/lock-badge.component";
import type { FolderExposure } from "../../../services/folders.service";
import type { Meeting, MeetingStatus } from "../../../core/models";
import type { ViewGroup, ViewRow } from "../../../services/view-engine";

/**
 * Feature B — the BOARD (kanban) saved view over the meetings list. Renders the
 * {@link ViewGroup} columns the ViewEngine produced (grouped by status or
 * folder), one `<mur-card>` per meeting. Presentation only — no data, no IPC;
 * every card is the (already-gated) `Meeting` the backend returned. A locked
 * meeting stays MASKED here too: its title renders as "•••" and its counts are
 * suppressed exactly as in the table/list — no field a masked DTO nulled is
 * ever surfaced.
 */
@Component({
  selector: "app-meetings-board-view",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurCardComponent, LockBadgeComponent],
  templateUrl: "./meetings-board-view.component.html",
  styleUrl: "./meetings-board-view.component.scss",
})
export class MeetingsBoardViewComponent {
  /** The board columns (group key/label + its rows) from the ViewEngine. */
  readonly groups = input.required<ViewGroup[]>();

  readonly folderNameOf = input.required<(m: Meeting) => string | null>();
  readonly folderExposureOf = input.required<(m: Meeting) => FolderExposure | null>();
  readonly isMasked = input.required<(m: Meeting) => boolean>();

  /** A card click asks the parent to open the meeting as a tab. */
  readonly openMeeting = output<{ event: Event; meeting: Meeting }>();

  readonly trackByGroup = (g: ViewGroup): string => g.key;
  readonly trackByRow = (row: ViewRow): string => row.meeting.id;

  onOpen(event: Event, meeting: Meeting): void {
    this.openMeeting.emit({ event, meeting });
  }

  statusLabel(s: string): string {
    return s.charAt(0) + s.slice(1).toLowerCase();
  }

  statusPillClass(s: MeetingStatus): string {
    switch (s) {
      case "RECORDING":
      case "ERROR":
        return "is-danger";
      case "TRANSCRIBED":
      case "SUMMARIZED":
        return "is-accent";
      case "EXPORTED":
        return "is-success";
      default:
        return "";
    }
  }

  formatDate(startedAt: string): string {
    const d = new Date(startedAt);
    if (Number.isNaN(d.getTime())) {
      return startedAt;
    }
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  }
}
