import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
} from "@angular/core";
import type { FolderExposure } from "../../../services/folders.service";

/**
 * A pure, presentational 3-state privacy badge for a folder:
 *  - `"open"`    — no glyph (the absence of a lock is itself the signal); a hair-
 *                  line neutral dot keeps the slot from collapsing the row.
 *  - `"locked"`  — a closed padlock (🔒): sealed + not visible this session.
 *  - `"session"` — an open padlock (🔓), accent-tinted: sealed on disk but
 *                  session-unlocked (plaintext visible until relock).
 *
 * Input-driven ONLY — no service, no state. The parent passes `exposure`; this
 * component just paints it (inline SVG, tokens, an `aria-label` per state). It
 * never sizes its own row; it sits inline at a fixed 16px box.
 */
@Component({
  selector: "app-lock-badge",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: {
    class: "lock-badge",
    "[class.is-open]": "exposure() === 'open'",
    "[class.is-locked]": "exposure() === 'locked'",
    "[class.is-session]": "exposure() === 'session'",
    role: "img",
    "[attr.aria-label]": "label()",
    "[attr.title]": "label()",
  },
  templateUrl: "./lock-badge.component.html",
  styleUrl: "./lock-badge.component.scss",
})
export class LockBadgeComponent {
  /** The folder's privacy exposure (open / locked / session). */
  readonly exposure = input.required<FolderExposure>();

  /** Screen-reader + tooltip label, derived from the exposure state. */
  readonly label = computed(() => {
    switch (this.exposure()) {
      case "locked":
        return "Locked — sealed and hidden";
      case "session":
        return "Unlocked for this session";
      default:
        return "Open folder";
    }
  });
}
