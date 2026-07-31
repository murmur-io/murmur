import {
  DestroyRef,
  Injectable,
  computed,
  inject,
  signal,
} from "@angular/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "../../core/ipc.service";
import type {
  ReminderDraft,
  ReminderInboxItem,
  ReminderSourceUpdatedPayload,
  ReminderSuggestionView,
  ReminderView,
} from "../../core/models";

interface ReminderListenerReadiness {
  updatesReady: boolean;
  privacyReady: boolean;
}

const LIVE_UPDATES_WARNING =
  "Live reminder updates are unavailable. Retry to reconnect.";

function validDueInboxCount(value: unknown): value is number {
  return (
    typeof value === "number" &&
    Number.isFinite(value) &&
    Number.isInteger(value) &&
    value >= 0
  );
}

function validSourceInvalidation(
  value: ReminderSourceUpdatedPayload | null | undefined,
): value is ReminderSourceUpdatedPayload {
  return (
    (value?.kind === "meeting" || value?.kind === "note") &&
    typeof value.id === "string" &&
    value.id.length > 0
  );
}

function withoutSourceMetadata(
  reminder: ReminderView,
  invalidation: ReminderSourceUpdatedPayload | null,
): ReminderView {
  const sources =
    invalidation === null
      ? []
      : reminder.sources.filter(
          (source) =>
            source.kind !== invalidation.kind || source.id !== invalidation.id,
        );
  return sources.length === reminder.sources.length
    ? reminder
    : { ...reminder, sources };
}

/**
 * Root-persisted reminder state. Cached rows survive route destruction and remain
 * visible during every unconditional refresh (stale-while-revalidate).
 */
@Injectable({ providedIn: "root" })
export class RemindersStore {
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);

  private readonly _inbox = signal<ReminderInboxItem[]>([]);
  readonly inbox = this._inbox.asReadonly();
  private readonly _upcoming = signal<ReminderView[]>([]);
  readonly upcoming = this._upcoming.asReadonly();
  private readonly _completed = signal<ReminderView[]>([]);
  readonly completed = this._completed.asReadonly();
  private readonly _dueInboxCount = signal(0);
  readonly dueInboxCount = this._dueInboxCount.asReadonly();
  private readonly _loading = signal(false);
  readonly loading = this._loading.asReadonly();
  private readonly _error = signal<string | null>(null);
  readonly error = this._error.asReadonly();
  private readonly _busy = signal<ReadonlySet<string>>(new Set());
  readonly busy = this._busy.asReadonly();
  private readonly _revision = signal(0);
  /** Content-free invalidation clock for contextual Smart suggestion cards. */
  readonly revision = this._revision.asReadonly();

  readonly hasCachedRows = computed(
    () =>
      this._inbox().length +
        this._upcoming().length +
        this._completed().length >
      0,
  );

  private summaryStarted = false;
  private rowsRequested = false;
  private loadSequence = 0;
  private countWriteEpoch = 0;
  private destroyed = false;
  private remindersUnlisten: UnlistenFn | null = null;
  private reminderSourceUnlisten: UnlistenFn | null = null;
  private reminderVisibilityUnlisten: UnlistenFn | null = null;
  /** Coalesces only the current install attempt; failed slots remain retryable. */
  private listenerAttempt: Promise<ReminderListenerReadiness> | null = null;

  constructor() {
    this.destroyRef.onDestroy(() => {
      this.destroyed = true;
      this.remindersUnlisten?.();
      this.remindersUnlisten = null;
      this.reminderSourceUnlisten?.();
      this.reminderSourceUnlisten = null;
      this.reminderVisibilityUnlisten?.();
      this.reminderVisibilityUnlisten = null;
    });
  }

  /**
   * Starts the content-free shell projection exactly once. The count event may
   * update navigation immediately; reminder rows are only fetched after the page
   * has first loaded them.
   */
  async initSummary(): Promise<void> {
    const takeSnapshot = !this.summaryStarted;
    this.summaryStarted = true;
    await this.ensureListeners();
    if (this.destroyed || !takeSnapshot) {
      return;
    }

    // Subscribe BEFORE taking the snapshot. If an event lands while the summary
    // request is in flight, its newer count wins instead of being overwritten
    // by the stale response.
    const summaryEpoch = this.countWriteEpoch;
    try {
      const summary = await this.ipc.getReminderSummary();
      if (!this.destroyed && this.countWriteEpoch === summaryEpoch) {
        this.applyDueInboxCount(summary?.dueInboxCount);
      }
    } catch {
      // Shell startup stays best-effort; the full route owns a visible error.
    }
  }

  /** Unconditional refresh with a stale-result token; cached rows remain visible. */
  async refresh(): Promise<void> {
    const sequence = ++this.loadSequence;
    // Set this before invoking IPC so an event racing the very first request
    // launches a newer request whose sequence supersedes the stale response.
    this.rowsRequested = true;
    this._loading.set(true);
    this._error.set(null);
    try {
      // The first canonical snapshot must not race ahead of either invalidation
      // listener. An event arriving during that snapshot launches a newer
      // sequence, so this response can no longer restore stale source metadata.
      const readiness = await this.ensureListeners();
      if (this.destroyed || sequence !== this.loadSequence) {
        return;
      }
      if (!readiness.privacyReady) {
        throw new Error("Reminder source visibility listeners are unavailable");
      }
      const snapshot = await this.ipc.listReminders();
      if (this.destroyed || sequence !== this.loadSequence) {
        return;
      }
      this._inbox.set(snapshot.inbox);
      this._upcoming.set(snapshot.upcoming);
      this._completed.set(snapshot.completed);
      this.applyDueInboxCount(snapshot.dueInboxCount);
      this._error.set(readiness.updatesReady ? null : LIVE_UPDATES_WARNING);
    } catch {
      if (sequence === this.loadSequence) {
        this._error.set("Couldn’t load reminders. Please try again.");
      }
    } finally {
      if (sequence === this.loadSequence) {
        this._loading.set(false);
      }
    }
  }

  private ensureListeners(): Promise<ReminderListenerReadiness> {
    const readiness = this.listenerReadiness();
    if (this.destroyed || (readiness.updatesReady && readiness.privacyReady)) {
      return Promise.resolve(readiness);
    }
    if (this.listenerAttempt) {
      return this.listenerAttempt;
    }

    const attempt = Promise.all([
      this.remindersUnlisten
        ? Promise.resolve(true)
        : this.installRemindersListener(),
      this.reminderSourceUnlisten
        ? Promise.resolve(true)
        : this.installReminderSourceListener(),
      this.reminderVisibilityUnlisten
        ? Promise.resolve(true)
        : this.installReminderVisibilityListener(),
    ]).then(() => this.listenerReadiness());
    this.listenerAttempt = attempt;
    const clearAttempt = (): void => {
      if (this.listenerAttempt === attempt) {
        this.listenerAttempt = null;
      }
    };
    void attempt.then(clearAttempt, clearAttempt);
    return attempt;
  }

  private listenerReadiness(): ReminderListenerReadiness {
    return {
      updatesReady: this.remindersUnlisten !== null,
      privacyReady:
        this.reminderSourceUnlisten !== null &&
        this.reminderVisibilityUnlisten !== null,
    };
  }

  private async installRemindersListener(): Promise<boolean> {
    if (this.remindersUnlisten) {
      return true;
    }
    try {
      const unlisten = await this.ipc.onRemindersUpdated((payload) => {
        if (this.destroyed) {
          return;
        }
        this.applyDueInboxCount(payload?.dueInboxCount);
        this._revision.update((value) => value + 1);
        if (this.rowsRequested) {
          void this.refresh();
        }
      });
      if (this.destroyed) {
        unlisten();
        return false;
      }
      if (this.remindersUnlisten) {
        unlisten();
        return true;
      }
      this.remindersUnlisten = unlisten;
      return true;
    } catch {
      return false;
    }
  }

  private async installReminderSourceListener(): Promise<boolean> {
    if (this.reminderSourceUnlisten) {
      return true;
    }
    try {
      const unlisten = await this.ipc.onReminderSourceUpdated((payload) => {
        if (this.destroyed || !validSourceInvalidation(payload)) {
          return;
        }

        // Fail closed before any awaited IPC: stale-while-revalidate must never
        // keep a now-sealed title rendered if the refresh stalls or fails.
        this.scrubCachedSourceMetadata(payload);
        if (this.rowsRequested) {
          void this.refresh();
        }
      });
      if (this.destroyed) {
        unlisten();
        return false;
      }
      if (this.reminderSourceUnlisten) {
        unlisten();
        return true;
      }
      this.reminderSourceUnlisten = unlisten;
      return true;
    } catch {
      // `emit()` only confirms dispatch into Tauri's event bus, not that this
      // renderer subscribed. Without this listener a later source lifecycle
      // change could leave cached metadata visible indefinitely.
      this.scrubCachedSourceMetadata(null);
      return false;
    }
  }

  private async installReminderVisibilityListener(): Promise<boolean> {
    if (this.reminderVisibilityUnlisten) {
      return true;
    }
    try {
      const unlisten = await this.ipc.onReminderVisibilityInvalidated(() => {
        if (this.destroyed) {
          return;
        }

        // Lock authority was revoked globally. Remove every live title before
        // the gated canonical refresh is even allowed to begin.
        this.scrubCachedSourceMetadata(null);
        if (this.rowsRequested) {
          void this.refresh();
        }
      });
      if (this.destroyed) {
        unlisten();
        return false;
      }
      if (this.reminderVisibilityUnlisten) {
        unlisten();
        return true;
      }
      this.reminderVisibilityUnlisten = unlisten;
      return true;
    } catch {
      // Never load or retain source titles when global lock revocation cannot
      // be observed by this renderer.
      this.scrubCachedSourceMetadata(null);
      return false;
    }
  }

  private scrubCachedSourceMetadata(
    invalidation: ReminderSourceUpdatedPayload | null,
  ): void {
    this._inbox.update((rows) =>
      rows.map((row) => {
        const reminder = withoutSourceMetadata(row.reminder, invalidation);
        return reminder === row.reminder ? row : { ...row, reminder };
      }),
    );
    this._upcoming.update((rows) =>
      rows.map((row) => withoutSourceMetadata(row, invalidation)),
    );
    this._completed.update((rows) =>
      rows.map((row) => withoutSourceMetadata(row, invalidation)),
    );
  }

  private applyDueInboxCount(value: unknown): boolean {
    if (!validDueInboxCount(value)) {
      return false;
    }
    this.countWriteEpoch += 1;
    this._dueInboxCount.set(value);
    return true;
  }

  async create(draft: ReminderDraft): Promise<ReminderView> {
    return this.confirmThenRefresh("create", () =>
      this.ipc.createMurmurReminder(draft),
    );
  }

  async update(
    reminderId: string,
    draft: ReminderDraft,
  ): Promise<ReminderView> {
    return this.confirmThenRefresh(reminderId, () =>
      this.ipc.updateMurmurReminder(reminderId, draft),
    );
  }

  async delete(reminderId: string): Promise<void> {
    await this.confirmThenRefresh(reminderId, () =>
      this.ipc.deleteMurmurReminder(reminderId),
    );
  }

  async complete(reminderId: string, expectedDueAt: number): Promise<void> {
    await this.confirmThenRefresh(reminderId, () =>
      this.ipc.completeMurmurReminder(reminderId, expectedDueAt),
    );
  }

  async dismissOccurrence(occurrenceId: string): Promise<void> {
    await this.confirmThenRefresh(occurrenceId, () =>
      this.ipc.dismissMurmurReminderOccurrence(occurrenceId),
    );
  }

  async acceptSuggestion(
    suggestion: ReminderSuggestionView,
    draft: ReminderDraft,
  ): Promise<ReminderView> {
    return this.confirmThenRefresh(suggestion.id, () =>
      this.ipc.acceptReminderSuggestion(suggestion.id, draft),
    );
  }

  private async confirmThenRefresh<T>(
    key: string,
    action: () => Promise<T>,
  ): Promise<T> {
    if (this._busy().has(key)) {
      throw new Error("Reminder action already in progress");
    }
    this.setBusy(key, true);
    this._error.set(null);
    try {
      const result = await action();
      await this.refresh();
      this._revision.update((value) => value + 1);
      return result;
    } catch (error) {
      this._error.set("Couldn’t update reminders. Please try again.");
      throw error;
    } finally {
      this.setBusy(key, false);
    }
  }

  private setBusy(key: string, busy: boolean): void {
    this._busy.update((current) => {
      const next = new Set(current);
      if (busy) {
        next.add(key);
      } else {
        next.delete(key);
      }
      return next;
    });
  }
}
