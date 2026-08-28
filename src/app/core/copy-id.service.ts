import { DestroyRef, Injectable, inject, signal } from "@angular/core";

import { ToastService } from "../services/toast.service";

/** How long the control keeps showing its "copied" tick before reverting (ms). */
const COPIED_FLASH_MS = 1600;

/**
 * Copying an item's stable id to the clipboard, with the transient "copied" flash.
 *
 * # Why this is a service and not three lines in the component
 *
 * The flash needs a timer, and `angular-zoneless.md` §5 bans `setTimeout` in a component
 * ("service timers are the only sanctioned `setTimeout`"). Owning it here keeps the handle
 * tracked and cleared once in {@link DestroyRef.onDestroy}, the same shape
 * `services/toast.service.ts` uses for its auto-dismiss queue.
 *
 * # Why the failure path is loud
 *
 * `navigator.clipboard.writeText` rejects when the webview refuses the write. The pre-existing
 * copy buttons in Settings swallow that in a bare `catch {}` — they can afford to, because the
 * text they copy is also on screen and selectable. An id is NOT on screen: a silent refusal
 * would flash nothing, leave the clipboard holding whatever it held before, and the user would
 * paste the wrong thing into Claude. So a refusal raises a danger toast instead.
 */
@Injectable({ providedIn: "root" })
export class CopyIdService {
  private readonly toast = inject(ToastService);
  private readonly destroyRef = inject(DestroyRef);

  private readonly _lastCopied = signal<string | null>(null);
  /** The id copied within the last {@link COPIED_FLASH_MS}, or `null`. */
  readonly lastCopied = this._lastCopied.asReadonly();

  private flashTimer: ReturnType<typeof setTimeout> | null = null;

  constructor() {
    this.destroyRef.onDestroy(() => {
      if (this.flashTimer !== null) {
        clearTimeout(this.flashTimer);
        this.flashTimer = null;
      }
    });
  }

  /**
   * Copy `id` verbatim and confirm it. `label` names the kind in the toast ("Meeting", "Note").
   *
   * The id is copied RAW — no prefix, no punctuation — because that exact string is what the
   * local MCP server's tools take as `meetingId` / `documentId` / `dashboardId`. Anything the
   * user would have to strip before pasting defeats the point of the control.
   */
  async copy(id: string, label: string): Promise<void> {
    const value = id.trim();
    if (!value) {
      return;
    }
    try {
      await navigator.clipboard.writeText(value);
    } catch {
      this.toast.danger(
        `Couldn’t copy the ${label.toLowerCase()} ID — your Mac refused clipboard access.`,
      );
      return;
    }
    this._lastCopied.set(value);
    if (this.flashTimer !== null) {
      clearTimeout(this.flashTimer);
    }
    this.flashTimer = setTimeout(() => {
      this._lastCopied.set(null);
      this.flashTimer = null;
    }, COPIED_FLASH_MS);
    this.toast.success(`${label} ID copied`);
  }
}
