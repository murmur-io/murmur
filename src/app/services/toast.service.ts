import {
  DestroyRef,
  Injectable,
  computed,
  inject,
  signal,
} from "@angular/core";

/** A single live toast. `kind` tints the strip; `id` keys the @for + dismissal. */
export interface Toast {
  id: number;
  message: string;
  kind: "info" | "success" | "danger";
}

/** How long a toast lingers before it auto-dismisses (ms). */
const DEFAULT_TTL_MS = 4200;

/**
 * App-wide toast queue, signal-based. Components read {@link toasts} (a readonly
 * signal) and render them; anything can `push(...)` a transient message.
 *
 * Timer lifecycle is owned here, not in components: each toast schedules ONE
 * tracked `setTimeout`, all of which are cleared in `DestroyRef.onDestroy`
 * (sanctioned service-timer pattern — never a bare component setTimeout). The
 * service is `providedIn: "root"`, so its DestroyRef is the root injector's and
 * the timers live for the app's lifetime, but the cleanup keeps it leak-clean
 * and HMR-safe.
 */
@Injectable({ providedIn: "root" })
export class ToastService {
  private readonly destroyRef = inject(DestroyRef);

  private readonly _toasts = signal<Toast[]>([]);
  /** The live toast queue, oldest first. */
  readonly toasts = this._toasts.asReadonly();
  /** True when at least one toast is showing (drives the host viewport). */
  readonly hasToasts = computed(() => this._toasts().length > 0);

  /** Monotonic id source so re-used messages still get a unique key. */
  private nextId = 1;
  /** Active auto-dismiss timers, keyed by toast id, so we can clear them. */
  private readonly timers = new Map<number, ReturnType<typeof setTimeout>>();

  constructor() {
    // Clear every pending auto-dismiss timer on teardown — no orphaned timers.
    this.destroyRef.onDestroy(() => {
      for (const handle of this.timers.values()) {
        clearTimeout(handle);
      }
      this.timers.clear();
    });
  }

  /** Push a toast; returns its id. Auto-dismisses after `ttlMs` (0 = sticky). */
  push(
    message: string,
    kind: Toast["kind"] = "info",
    ttlMs: number = DEFAULT_TTL_MS,
  ): number {
    const id = this.nextId++;
    this._toasts.update((list) => [...list, { id, message, kind }]);
    if (ttlMs > 0) {
      const handle = setTimeout(() => this.dismiss(id), ttlMs);
      this.timers.set(id, handle);
    }
    return id;
  }

  /** Convenience: a neutral info toast. */
  info(message: string, ttlMs?: number): number {
    return this.push(message, "info", ttlMs);
  }

  /** Convenience: a success-tinted toast. */
  success(message: string, ttlMs?: number): number {
    return this.push(message, "success", ttlMs);
  }

  /** Convenience: a danger-tinted toast (sticks a little longer by default). */
  danger(message: string, ttlMs = 6000): number {
    return this.push(message, "danger", ttlMs);
  }

  /** Remove a toast by id (also cancels its pending auto-dismiss timer). */
  dismiss(id: number): void {
    const handle = this.timers.get(id);
    if (handle !== undefined) {
      clearTimeout(handle);
      this.timers.delete(id);
    }
    this._toasts.update((list) => list.filter((t) => t.id !== id));
  }

  /** Clear every toast at once (and all their timers). */
  clear(): void {
    for (const handle of this.timers.values()) {
      clearTimeout(handle);
    }
    this.timers.clear();
    this._toasts.set([]);
  }
}
