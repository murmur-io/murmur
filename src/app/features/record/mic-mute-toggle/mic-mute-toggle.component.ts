import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  inject,
  input,
  signal,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";

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
  templateUrl: "./mic-mute-toggle.component.html",
  styleUrl: "./mic-mute-toggle.component.scss",
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
