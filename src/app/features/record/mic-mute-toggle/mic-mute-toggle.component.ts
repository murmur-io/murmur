import {
  ChangeDetectionStrategy,
  Component,
  OnDestroy,
  OnInit,
  inject,
  input,
  signal,
} from "@angular/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "../../../core/ipc.service";
import { ToastService } from "../../../services/toast.service";

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
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./mic-mute-toggle.component.html",
  styleUrl: "./mic-mute-toggle.component.scss",
})
export class MicMuteToggleComponent implements OnInit, OnDestroy {
  private readonly ipc = inject(IpcService);
  private readonly toast = inject(ToastService);

  /** Slim, icon-only variant for the floating bar (hides the label + hint). */
  readonly compact = input<boolean>(false);

  /** Whether the mic is currently muted (init from the backend; flips optimistically). */
  readonly muted = signal(false);
  /** True until the initial backend state arrives, and during a mute IPC call. */
  readonly busy = signal(true);
  private unlistenAutoUnmuted: UnlistenFn | null = null;
  private destroyed = false;
  private stateRevision = 0;

  async ngOnInit(): Promise<void> {
    // Establish the event stream before taking the snapshot. If the helper dies
    // during startup, either the listener observes it or the later snapshot sees
    // the backend's restored state — there is no gap in between.
    await this.installAutoUnmuteListener();
    // Seed from the live recorder so the icon reflects reality on mount.
    // Best-effort: a failure (or "not recording" → false) leaves it un-muted.
    const revision = this.stateRevision;
    try {
      const muted = await this.ipc.isMicMuted();
      if (revision === this.stateRevision) {
        this.muted.set(muted);
      }
    } catch {
      if (revision === this.stateRevision) {
        this.muted.set(false);
      }
    } finally {
      // Do not accept a click before the initial read settles: its stale response
      // could otherwise overwrite a newer, successfully persisted toggle.
      this.busy.set(false);
    }
  }

  ngOnDestroy(): void {
    this.destroyed = true;
    this.unlistenAutoUnmuted?.();
    this.unlistenAutoUnmuted = null;
  }

  private async installAutoUnmuteListener(): Promise<void> {
    try {
      const unlisten = await this.ipc.onMicAutoUnmuted(() => {
        this.stateRevision += 1;
        this.muted.set(false);
        this.toast.info(
          "Microphone restored because system audio stopped unexpectedly.",
        );
      });
      if (this.destroyed) {
        unlisten();
      } else {
        this.unlistenAutoUnmuted = unlisten;
      }
    } catch {
      // Best-effort UI resync only. Rust restores the mic independently.
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
      if (next) {
        this.toast.danger(
          "Microphone stayed on — Murmur hasn't confirmed system audio yet. Check Audio Recording or Screen Recording access.",
        );
      } else {
        this.toast.danger("Couldn't unmute the microphone. Try again.");
      }
    } finally {
      this.busy.set(false);
    }
  }
}
