import {
  ChangeDetectionStrategy,
  Component,
  input,
  output,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import type { DocumentInfo } from "../../../core/models";

/**
 * One "knowledge source" card on the Brain page (in-flow `.card`, NOT floating).
 *
 * Two shapes, chosen by inputs:
 *  - a READ-ONLY source (Meetings): an accent icon-tile + title + count + a link
 *    to its page — no list, no add.
 *  - an EDITABLE source (Documents / Notes): an accent icon-tile + title + count,
 *    a "+ Add" button (emitting {@link add}), and an expandable list of items
 *    (name + date + delete) fed by {@link items}. A sealed-selected folder
 *    disables the add + shows a locked note (owned by the parent via {@link blocked}).
 *
 * Pure/presentational: all IPC + folder-state lives in the parent
 * `BrainComponent`; this card only renders + emits `add`/`remove`/`toggle`.
 */
@Component({
  selector: "app-brain-source-card",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  templateUrl: "./brain-source-card.component.html",
  styleUrl: "./brain-source-card.component.scss",
})
export class BrainSourceCardComponent {
  /** Which line-art source icon to render in the accent tile. */
  readonly icon = input.required<"meetings" | "documents" | "notes">();
  readonly title = input.required<string>();
  readonly subtitle = input.required<string>();
  readonly count = input.required<number>();

  /** When set, the card is a READ-ONLY link source (no list/add). e.g. "/library". */
  readonly linkTo = input<string | null>(null);
  readonly linkLabel = input("Open");

  /** Editable-source inputs (Documents / Notes). */
  readonly items = input<DocumentInfo[]>([]);
  readonly expanded = input(false);
  readonly loading = input(false);
  readonly busy = input(false);
  readonly deletingId = input<string | null>(null);
  /** True when the selected folder is sealed → add disabled + a note. */
  readonly blocked = input(false);
  readonly addLabel = input("Add");
  readonly busyLabel = input("Adding…");
  readonly emptyLabel = input("Nothing here yet.");

  readonly add = output<void>();
  readonly deleteItem = output<DocumentInfo>();
  readonly toggleList = output<void>();

  /** Epoch-millis → a short local date string. */
  protected formatDate(epochMs: number): string {
    return new Date(epochMs).toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  }
}
