import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  inject,
  input,
  signal,
} from "@angular/core";
import { IpcService } from "../../core/ipc.service";

/**
 * MIC-MUTE toggle — shown ONLY while recording (the host decides when to render
 * it). A single clear button flips the LOCAL microphone between on and off;
 * muting silences only the mic — captured system audio ("others") keeps
 * recording, which the hint makes explicit.
 *
 * Presentational + self-owning: it reads the live mute state from the backend
 * on mount (`isMicMuted`) and flips OPTIMISTICALLY on click (`setMicMuted`),
 * rolling back if the IPC call rejects. It never starts/stops a recording, so
 * it can sit beside the Stop / Start controls without interfering with them.
 *
 * Lives in its own file (own inline-style budget) and is reused by BOTH the
 * Record screen and the floating bar — the `compact` input slims it for the
 * pill. Inline SVG glyphs (no icon dependency); tokens only.
 */
@Component({
  selector: "app-mic-mute-toggle",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <button
      type="button"
      class="mic"
      [class.is-muted]="muted()"
      [class.is-compact]="compact()"
      [disabled]="busy()"
      [attr.aria-pressed]="muted()"
      [attr.aria-label]="muted() ? 'Unmute microphone' : 'Mute microphone'"
      [title]="muted() ? 'Unmute microphone' : 'Mute microphone'"
      (click)="toggle()"
    >
      @if (muted()) {
        <!-- Mic-off: mic body + a diagonal slash. -->
        <svg
          class="mic-ico"
          viewBox="0 0 24 24"
          fill="none"
          aria-hidden="true"
          focusable="false"
        >
          <path
            d="M9 9V5a3 3 0 0 1 5.12-2.12M15 9.34V10a3 3 0 0 1-3 3"
            stroke="currentColor"
            stroke-width="1.9"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
          <path
            d="M17 11a5 5 0 0 1-.54 2.27M5 11a7 7 0 0 0 7 7m0 0v3m0-3a6.97 6.97 0 0 0 1.6-.18"
            stroke="currentColor"
            stroke-width="1.9"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
          <path
            d="M3 3l18 18"
            stroke="currentColor"
            stroke-width="1.9"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
      } @else {
        <!-- Mic-on: classic capsule mic + stand. -->
        <svg
          class="mic-ico"
          viewBox="0 0 24 24"
          fill="none"
          aria-hidden="true"
          focusable="false"
        >
          <rect
            x="9"
            y="2"
            width="6"
            height="12"
            rx="3"
            stroke="currentColor"
            stroke-width="1.9"
          />
          <path
            d="M5 11a7 7 0 0 0 14 0M12 18v3"
            stroke="currentColor"
            stroke-width="1.9"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
      }
      @if (!compact()) {
        <span class="mic-label">{{ muted() ? "Muted" : "Mute" }}</span>
      }
    </button>
    @if (muted() && !compact()) {
      <span class="mic-hint" role="status">
        Mic muted — still capturing others
      </span>
    }
  `,
  styles: [
    `
      :host {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
      }

      /* The toggle — neutral when live, unmistakable accent/live when muted. */
      .mic {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        gap: var(--space-2);
        height: 40px;
        padding: 0 var(--space-4);
        border: 1px solid var(--border);
        border-radius: var(--radius-pill);
        background: rgba(255, 255, 255, 0.05);
        color: var(--text-secondary);
        font: inherit;
        font-size: 0.875rem;
        font-weight: 550;
        cursor: pointer;
        transition:
          background var(--transition),
          border-color var(--transition),
          color var(--transition),
          box-shadow var(--transition),
          transform var(--transition-fast);
      }
      .mic:hover:not(:disabled) {
        background: var(--surface-hover);
        border-color: var(--border-strong);
        color: var(--text-primary);
      }
      .mic:active:not(:disabled) {
        transform: translateY(1px);
      }
      .mic:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .mic:disabled {
        opacity: 0.55;
        cursor: not-allowed;
      }

      /* Muted — loud, alive: warm "live" fill so it reads at a glance. */
      .mic.is-muted {
        background: var(--live-soft);
        border-color: transparent;
        color: var(--live-hover);
      }
      .mic.is-muted:hover:not(:disabled) {
        background: var(--live-soft);
        color: var(--live-hover);
        filter: brightness(1.08);
      }
      .mic.is-muted:focus-visible {
        box-shadow: 0 0 0 3px rgba(255, 122, 92, 0.6);
      }

      /* Compact (floating bar): icon-only circle, sized to the slim pill. */
      .mic.is-compact {
        width: 36px;
        height: 36px;
        min-width: 36px;
        padding: 0;
        border-radius: 50%;
      }

      .mic-ico {
        width: 18px;
        height: 18px;
        flex: none;
      }
      .mic-label {
        line-height: 1;
      }

      .mic-hint {
        color: var(--live-hover);
        font-size: 0.8125rem;
        font-weight: 500;
        line-height: 1.3;
        white-space: nowrap;
      }
    `,
  ],
})
export class MicMuteToggleComponent implements OnInit {
  private readonly ipc = inject(IpcService);

  /** Slim, icon-only variant for the floating bar (hides the label + hint). */
  readonly compact = input<boolean>(false);

  /** Whether the mic is currently muted (init from the backend; flips optimistically). */
  readonly muted = signal(false);
  /** True while a setMicMuted IPC call is in flight (debounces double-clicks). */
  readonly busy = signal(false);

  async ngOnInit(): Promise<void> {
    // Seed from the live recorder so the icon reflects reality on mount.
    // Best-effort: a failure (or "not recording" → false) leaves it un-muted.
    try {
      this.muted.set(await this.ipc.isMicMuted());
    } catch {
      this.muted.set(false);
    }
  }

  /**
   * Optimistically flip the mic state, then persist via `setMicMuted`. On an
   * IPC failure we roll the icon back so it never lies about the real state.
   * Never touches start/stop — only the mic mute flag.
   */
  async toggle(): Promise<void> {
    if (this.busy()) {
      return;
    }
    const next = !this.muted();
    this.muted.set(next);
    this.busy.set(true);
    try {
      await this.ipc.setMicMuted(next);
    } catch {
      this.muted.set(!next);
    } finally {
      this.busy.set(false);
    }
  }
}
