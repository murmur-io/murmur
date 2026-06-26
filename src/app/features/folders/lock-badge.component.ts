import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
} from "@angular/core";
import type { FolderExposure } from "../../services/folders.service";

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
  template: `
    @switch (exposure()) {
      @case ("locked") {
        <!-- Closed padlock: sealed, not visible this session. -->
        <svg
          viewBox="0 0 16 16"
          width="13"
          height="13"
          fill="none"
          aria-hidden="true"
        >
          <rect
            x="3.25"
            y="7"
            width="9.5"
            height="6.5"
            rx="1.6"
            stroke="currentColor"
            stroke-width="1.4"
          />
          <path
            d="M5.25 7V5.25a2.75 2.75 0 0 1 5.5 0V7"
            stroke="currentColor"
            stroke-width="1.4"
            stroke-linecap="round"
          />
          <circle cx="8" cy="10.1" r="1.05" fill="currentColor" />
        </svg>
      }
      @case ("session") {
        <!-- Open padlock: session-unlocked (plaintext visible until relock). -->
        <svg
          viewBox="0 0 16 16"
          width="13"
          height="13"
          fill="none"
          aria-hidden="true"
        >
          <rect
            x="3.25"
            y="7"
            width="9.5"
            height="6.5"
            rx="1.6"
            stroke="currentColor"
            stroke-width="1.4"
          />
          <path
            d="M5.25 7V5.25a2.75 2.75 0 0 1 5.4-0.7"
            stroke="currentColor"
            stroke-width="1.4"
            stroke-linecap="round"
          />
          <circle cx="8" cy="10.1" r="1.05" fill="currentColor" />
        </svg>
      }
      @default {
        <!-- Open folder: no lock; a faint dot holds the slot. -->
        <span class="open-dot" aria-hidden="true"></span>
      }
    }
  `,
  styles: [
    `
      :host {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 16px;
        height: 16px;
        flex: none;
        border-radius: var(--radius-pill);
        transition:
          color var(--transition),
          background var(--transition),
          transform var(--transition-fast);
      }
      :host svg {
        display: block;
      }
      :host.is-open {
        color: var(--text-muted);
      }
      :host.is-locked {
        color: var(--text-secondary);
      }
      :host.is-session {
        color: var(--accent-hover);
      }
      .open-dot {
        width: 5px;
        height: 5px;
        border-radius: 50%;
        background: currentColor;
        opacity: 0.45;
      }
      /* A gentle one-shot pop when a folder seals/unseals, never a loop. */
      :host.is-session,
      :host.is-locked {
        animation: lock-pop 220ms var(--ease-spring) both;
      }
      @keyframes lock-pop {
        from {
          transform: scale(0.6);
          opacity: 0;
        }
        to {
          transform: scale(1);
          opacity: 1;
        }
      }
      @media (prefers-reduced-motion: reduce) {
        :host,
        :host.is-session,
        :host.is-locked {
          animation: none;
          transition: none;
        }
      }
    `,
  ],
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
