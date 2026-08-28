import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  input,
} from "@angular/core";

import { CopyIdService } from "../../core/copy-id.service";

/**
 * Design System — `<mur-copy-id>`: the minimal "copy this thing's id" affordance for a header.
 *
 * # What it is for
 *
 * Murmur's local MCP server addresses everything by id — `get_meeting(meetingId)`,
 * `get_document(documentId)`, `get_dashboard(dashboardId)`. Until now the app rendered none of
 * those ids anywhere, so a user talking to Claude over MCP had no way to say *which* recording
 * or note they meant; they could only describe it and hope search found the right one.
 *
 * # Shape
 *
 * An icon-only ghost button that reads as chrome, not as a command: it sits at
 * `--text-tertiary`, gains a surface only on hover/focus, and swaps its glyph to a tick for
 * {@link CopyIdService} `COPIED_FLASH_MS` after a successful copy. The toast is the primary
 * confirmation (it survives the pointer leaving the button); the tick is the local one.
 *
 * The two glyphs are inline SVG rather than `<mur-icon>` on purpose: `.nav-icon svg` is a GLOBAL
 * 20px rule, and a parent's emulated-encapsulation style cannot reach into a child component's
 * template to shrink it. 20px is the nav tier; this control lives in a 12px meta row.
 */
@Component({
  selector: "mur-copy-id",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./copy-id.component.html",
  styleUrl: "./copy-id.component.scss",
})
export class MurCopyIdComponent {
  private readonly copier = inject(CopyIdService);

  /** The stable id to put on the clipboard, copied verbatim. */
  readonly id = input.required<string>();
  /** Names the kind in the tooltip and the toast — "Meeting", "Note", "Board", "Task". */
  readonly label = input("Item");

  /** True while this exact id is the one just copied (drives the tick). */
  readonly copied = computed(() => this.copier.lastCopied() === this.id().trim());

  /**
   * Accessible name AND tooltip.
   *
   * Deliberately NOT "paste it to point Claude at it". Every one of the four surfaces now has an
   * MCP tool that takes its id (`get_meeting`, `get_document`, `get_dashboard`, `get_task`), so the
   * promise would be true — but the tooltip is also the accessible name, and naming a downstream
   * consumer inside it makes the control's own job harder to read for anyone not using MCP. The
   * label says what the button does; the PR and the MCP catalog say what the id is for.
   */
  readonly hint = computed(() =>
    this.copied()
      ? `${this.label()} ID copied`
      : `Copy this ${this.label().toLowerCase()}’s ID`,
  );

  copy(): void {
    void this.copier.copy(this.id(), this.label());
  }
}
