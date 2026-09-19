import { DestroyRef, Injectable, computed, inject, signal } from "@angular/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { AskHistoryPrivacyBarrierService } from "../core/ask-history-privacy-barrier.service";
import { ErrorCopyService } from "../core/copy/error-copy.service";
import { IpcService } from "../core/ipc.service";
import type { ProcessingQueueItem } from "../core/models";

/**
 * Root-persisted, stale-while-revalidate queue state. SQLite remains canonical:
 * this cache is replaced after every command/event and never invents a job.
 */
@Injectable({ providedIn: "root" })
export class ProcessingQueueStore {
  private readonly ipc = inject(IpcService);
  private readonly privacy = inject(AskHistoryPrivacyBarrierService);
  private readonly errorCopy = inject(ErrorCopyService);
  private readonly destroyRef = inject(DestroyRef);

  private readonly _items = signal<ProcessingQueueItem[]>([]);
  private readonly _loading = signal(false);
  private readonly _mutating = signal(false);
  private readonly _error = signal<string | null>(null);
  private loadGeneration = 0;
  private unlisten: UnlistenFn | null = null;
  private initPromise: Promise<void> | null = null;

  readonly items = this._items.asReadonly();
  readonly running = computed(() =>
    this._items().some((item) => item.queueRunning === true || item.state === "processing"),
  );
  readonly loading = this._loading.asReadonly();
  readonly mutating = this._mutating.asReadonly();
  readonly error = this._error.asReadonly();

  constructor() {
    const unregister = this.privacy.registerInvalidator(() => {
      ++this.loadGeneration;
      this._items.set([]);
      this._loading.set(false);
      this._error.set(null);
    });
    this.destroyRef.onDestroy(() => {
      unregister();
      this.unlisten?.();
    });
  }

  async init(): Promise<void> {
    if (!this.initPromise) this.initPromise = this.initialize();
    await this.initPromise;
    await this.load();
  }

  async load(): Promise<void> {
    const generation = ++this.loadGeneration;
    this._loading.set(true);
    this._error.set(null);
    try {
      if (!(await this.privacy.ensureReady())) return;
      const rows = await this.ipc.listProcessingQueue();
      if (generation !== this.loadGeneration) return;
      this._items.set([...rows].sort((a, b) => a.position - b.position));
    } catch (error) {
      if (generation === this.loadGeneration) {
        this._error.set(this.errorCopy.humanize(error));
      }
    } finally {
      if (generation === this.loadGeneration) this._loading.set(false);
    }
  }

  processNow(meetingIds: string[]): Promise<boolean> {
    return this.mutate(meetingIds, (ids) => this.ipc.processQueueNow(ids));
  }

  retry(meetingIds: string[]): Promise<boolean> {
    return this.mutate(meetingIds, (ids) =>
      this.ipc.retryProcessingQueue(ids),
    );
  }

  remove(meetingIds: string[]): Promise<boolean> {
    return this.mutate(meetingIds, (ids) =>
      this.ipc.removeProcessingQueue(ids),
    );
  }

  async reorder(orderedMeetingIds: string[]): Promise<boolean> {
    if (orderedMeetingIds.length < 2 || this._mutating()) return false;
    this._mutating.set(true);
    this._error.set(null);
    try {
      await this.ipc.reorderProcessingQueue(orderedMeetingIds);
      await this.load();
      return true;
    } catch (error) {
      this._error.set(this.errorCopy.humanize(error));
      return false;
    } finally {
      this._mutating.set(false);
    }
  }

  private async initialize(): Promise<void> {
    try {
      this.unlisten = await this.ipc.onProcessingQueueChanged(() => {
        // The worker can release its permit while a mutation's refresh is pending.
        // Never discard that last event; loadGeneration rejects stale responses.
        void this.load();
      });
    } catch {
      // An older backend still gets a fresh read whenever the route mounts.
    }
  }

  private async mutate(
    meetingIds: string[],
    command: (ids: string[]) => Promise<void>,
  ): Promise<boolean> {
    const ids = [...new Set(meetingIds)].filter(Boolean);
    if (ids.length === 0 || this._mutating()) return false;
    this._mutating.set(true);
    this._error.set(null);
    try {
      await command(ids);
      await this.load();
      return true;
    } catch (error) {
      this._error.set(this.errorCopy.humanize(error));
      return false;
    } finally {
      this._mutating.set(false);
    }
  }
}
