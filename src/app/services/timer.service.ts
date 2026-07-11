import { DestroyRef, Injectable, inject } from "@angular/core";

/**
 * A minimal root-owned timer service — the ONLY sanctioned home for `setTimeout`
 * (rule §5: a bare `setTimeout`/`requestAnimationFrame` in a COMPONENT is banned;
 * only a `providedIn:"root"` service may hold tracked timers, mirroring
 * {@link ToastService}). Components that need a delayed tick (e.g. a step
 * animation that can't be expressed with the one-shot `afterNextRender`) schedule
 * through here so the raw timer lives in a service, is tracked, and is cleared on
 * root teardown — never orphaned.
 *
 * Each scheduled callback gets a monotonic handle id; the caller cancels a single
 * pending tick with {@link clear}, and every outstanding tick is cleared on
 * `DestroyRef.onDestroy`. The callback auto-forgets its own handle when it fires.
 */
@Injectable({ providedIn: "root" })
export class TimerService {
  private readonly destroyRef = inject(DestroyRef);

  /** Active timeouts, keyed by our monotonic handle id, so we can cancel them. */
  private readonly timers = new Map<number, ReturnType<typeof setTimeout>>();
  private nextId = 1;

  constructor() {
    // Clear every pending tick on teardown — no orphaned timers (HMR-safe).
    this.destroyRef.onDestroy(() => this.clearAll());
  }

  /**
   * Run `fn` after `ms`. Returns a handle id for {@link clear}. The handle is
   * forgotten automatically once `fn` fires.
   */
  after(ms: number, fn: () => void): number {
    const id = this.nextId++;
    const handle = setTimeout(() => {
      this.timers.delete(id);
      fn();
    }, ms);
    this.timers.set(id, handle);
    return id;
  }

  /** Cancel a single pending tick by its handle id (a no-op if already fired). */
  clear(id: number): void {
    const handle = this.timers.get(id);
    if (handle !== undefined) {
      clearTimeout(handle);
      this.timers.delete(id);
    }
  }

  /** Cancel every outstanding tick (used on host teardown / supersede). */
  clearAll(): void {
    for (const handle of this.timers.values()) {
      clearTimeout(handle);
    }
    this.timers.clear();
  }
}
