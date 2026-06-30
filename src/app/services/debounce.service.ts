import { DestroyRef, Injectable, inject } from "@angular/core";

/**
 * App-wide keyed debounce. The ONLY sanctioned `setTimeout` outside a component
 * (angular-zoneless §5): every pending timer handle is tracked in a `Map` keyed
 * by caller-chosen id and cleared in `DestroyRef.onDestroy` — the exact pattern
 * `toast.service.ts` uses for its auto-dismiss timers (never a bare component
 * `setTimeout`).
 *
 * `schedule(key, fn, delayMs)` coalesces rapid calls under the SAME key (the
 * classic debounced-autosave shape: each keystroke reschedules, only the last
 * fires). Distinct keys are independent, so two callers (e.g. two meetings)
 * never cancel each other — important for the record screen's per-meeting
 * autosave, where a still-pending save for meeting A must outlive switching to
 * meeting B and still fire.
 *
 * `providedIn: "root"`, so its `DestroyRef` is the root injector's and the
 * timers live for the app's lifetime; the cleanup keeps it leak-clean + HMR-safe.
 */
@Injectable({ providedIn: "root" })
export class DebounceService {
  private readonly destroyRef = inject(DestroyRef);

  /** Pending timers keyed by caller id, so each can be rescheduled / cancelled. */
  private readonly timers = new Map<string, ReturnType<typeof setTimeout>>();

  constructor() {
    // Clear every pending timer on teardown — no orphaned timers.
    this.destroyRef.onDestroy(() => {
      for (const handle of this.timers.values()) {
        clearTimeout(handle);
      }
      this.timers.clear();
    });
  }

  /**
   * Run `fn` after `delayMs` of quiet on `key`. A new call under the same key
   * cancels the previous pending one (debounce). The handle is forgotten the
   * instant it fires so a later `cancel(key)` can never clear an unrelated timer.
   */
  schedule(key: string, fn: () => void, delayMs: number): void {
    const existing = this.timers.get(key);
    if (existing !== undefined) {
      clearTimeout(existing);
    }
    const handle = setTimeout(() => {
      this.timers.delete(key);
      fn();
    }, delayMs);
    this.timers.set(key, handle);
  }

  /** Cancel a pending timer for `key` (no-op if none is scheduled). */
  cancel(key: string): void {
    const handle = this.timers.get(key);
    if (handle !== undefined) {
      clearTimeout(handle);
      this.timers.delete(key);
    }
  }
}
