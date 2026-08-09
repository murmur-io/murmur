import { DestroyRef, Injectable, computed, inject, signal } from "@angular/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "./ipc.service";

export type AskHistoryPrivacyStatus = "connecting" | "ready" | "error";

const SECURE_UNAVAILABLE = "Ask Brain isn’t available securely right now.";

/**
 * One process-wide registration barrier for durable Ask content.
 *
 * Tauri events are not replayed. A conversation/source read therefore cannot be
 * allowed to race ahead of any privacy invalidation listener: a lock in that
 * gap would leave stale plaintext in the mounted WebView. Consumers register a
 * synchronous scrub callback and gate every durable read/send on {@link ready}.
 */
@Injectable({ providedIn: "root" })
export class AskHistoryPrivacyBarrierService {
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);

  private readonly _status = signal<AskHistoryPrivacyStatus>("connecting");
  readonly status = this._status.asReadonly();
  readonly ready = computed(() => this._status() === "ready");
  readonly error = computed(() =>
    this._status() === "error" ? SECURE_UNAVAILABLE : null,
  );

  private askHistoryUnlisten: UnlistenFn | null = null;
  private contentDeletedUnlisten: UnlistenFn | null = null;
  private reminderVisibilityUnlisten: UnlistenFn | null = null;
  private listenerAttempt: Promise<boolean> | null = null;
  private readonly invalidators = new Set<() => void>();
  private destroyed = false;

  constructor() {
    this.destroyRef.onDestroy(() => {
      this.destroyed = true;
      this.askHistoryUnlisten?.();
      this.askHistoryUnlisten = null;
      this.contentDeletedUnlisten?.();
      this.contentDeletedUnlisten = null;
      this.reminderVisibilityUnlisten?.();
      this.reminderVisibilityUnlisten = null;
      this.invalidators.clear();
    });
  }

  registerInvalidator(invalidate: () => void): () => void {
    this.invalidators.add(invalidate);
    if (this._status() === "error") {
      invalidate();
    }
    return () => this.invalidators.delete(invalidate);
  }

  /**
   * Install only missing listeners and keep one attempt latched until every
   * registration settles. `allSettled` is deliberate: an early rejection must
   * not let Retry overlap still-pending registrations from the first attempt.
   */
  ensureReady(): Promise<boolean> {
    if (this.destroyed) {
      return Promise.resolve(false);
    }
    if (this.listenersReady()) {
      this._status.set("ready");
      return Promise.resolve(true);
    }
    if (this.listenerAttempt) {
      return this.listenerAttempt;
    }

    this._status.set("connecting");
    const attempt = Promise.allSettled([
      this.askHistoryUnlisten
        ? Promise.resolve()
        : this.installAskHistoryListener(),
      this.contentDeletedUnlisten
        ? Promise.resolve()
        : this.installContentDeletedListener(),
      this.reminderVisibilityUnlisten
        ? Promise.resolve()
        : this.installReminderVisibilityListener(),
    ]).then(() => {
      const ready = !this.destroyed && this.listenersReady();
      this._status.set(ready ? "ready" : "error");
      if (!ready) {
        this.invalidateMountedState();
      }
      return ready;
    });
    this.listenerAttempt = attempt;
    const clearAttempt = (): void => {
      if (this.listenerAttempt === attempt) {
        this.listenerAttempt = null;
      }
    };
    void attempt.then(clearAttempt, clearAttempt);
    return attempt;
  }

  private listenersReady(): boolean {
    return (
      this.askHistoryUnlisten !== null &&
      this.contentDeletedUnlisten !== null &&
      this.reminderVisibilityUnlisten !== null
    );
  }

  private async installAskHistoryListener(): Promise<void> {
    const unlisten = await this.ipc.onAskHistoryInvalidated(() =>
      this.invalidateMountedState(),
    );
    this.keepOneListener("askHistory", unlisten);
  }

  private async installContentDeletedListener(): Promise<void> {
    const unlisten = await this.ipc.onContentDeleted(() =>
      this.invalidateMountedState(),
    );
    this.keepOneListener("contentDeleted", unlisten);
  }

  private async installReminderVisibilityListener(): Promise<void> {
    const unlisten = await this.ipc.onReminderVisibilityInvalidated(() =>
      this.invalidateMountedState(),
    );
    this.keepOneListener("reminderVisibility", unlisten);
  }

  private keepOneListener(
    slot: "askHistory" | "contentDeleted" | "reminderVisibility",
    unlisten: UnlistenFn,
  ): void {
    if (this.destroyed) {
      unlisten();
      return;
    }
    const current =
      slot === "askHistory"
        ? this.askHistoryUnlisten
        : slot === "contentDeleted"
          ? this.contentDeletedUnlisten
          : this.reminderVisibilityUnlisten;
    if (current) {
      unlisten();
      return;
    }
    if (slot === "askHistory") {
      this.askHistoryUnlisten = unlisten;
    } else if (slot === "contentDeleted") {
      this.contentDeletedUnlisten = unlisten;
    } else {
      this.reminderVisibilityUnlisten = unlisten;
    }
  }

  private invalidateMountedState(): void {
    for (const invalidate of this.invalidators) {
      try {
        invalidate();
      } catch {
        // One broken consumer must never prevent the remaining WebViews from
        // synchronously scrubbing their own cached content.
      }
    }
  }
}
