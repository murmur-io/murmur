import { DestroyRef, Injectable, inject, signal } from "@angular/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "../../core/ipc.service";
import type { BriefRun, BriefSchedule } from "../../core/models";
import { ErrorCopyService } from "../../core/copy/error-copy.service";

/**
 * Brain v2 L5 — the SCHEDULED-BRIEFS store: schedules (config rows) + the
 * pending proposed runs (propose-accept cards), signals-first.
 *
 * `init()` is idempotent: the first caller subscribes ONCE to the backend's
 * `EVENT_BRIEF_PROPOSED` stream (a brief was staged by the 60s runner) and
 * refreshes the pending list on every event — the payload carries id/label/size
 * only, so the store re-fetches the actual rows via `listBriefRuns()`. The
 * `UnlistenFn` is released on destroy (the root injector's teardown).
 *
 * All mutations (create/update/delete/accept/dismiss) round-trip the backend
 * then refresh, so the signals always mirror SQLite (the canonical store).
 */
@Injectable({ providedIn: "root" })
export class BriefsStore {
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);
  private readonly errorCopy = inject(ErrorCopyService);

  private readonly _schedules = signal<BriefSchedule[]>([]);
  readonly schedules = this._schedules.asReadonly();

  private readonly _pending = signal<BriefRun[]>([]);
  readonly pending = this._pending.asReadonly();

  private readonly _loading = signal(true);
  readonly loading = this._loading.asReadonly();

  private readonly _error = signal<string | null>(null);
  readonly error = this._error.asReadonly();

  private initialized = false;
  private unlisten: UnlistenFn | null = null;

  /** Subscribe to the proposal stream ONCE + load both lists. Idempotent. */
  init(): void {
    if (this.initialized) {
      void this.refresh();
      return;
    }
    this.initialized = true;
    void this.ipc
      .onBriefProposed(() => void this.refresh())
      .then((un) => (this.unlisten = un));
    this.destroyRef.onDestroy(() => {
      this.unlisten?.();
      this.unlisten = null;
    });
    void this.refresh();
  }

  /** Reload schedules + pending runs from the backend. */
  async refresh(): Promise<void> {
    try {
      const [schedules, pending] = await Promise.all([
        this.ipc.listBriefSchedules(),
        this.ipc.listBriefRuns(),
      ]);
      this._schedules.set(schedules);
      this._pending.set(pending);
      this._error.set(null);
    } catch (e) {
      this._error.set(this.errorCopy.humanize(e));
    } finally {
      this._loading.set(false);
    }
  }

  async create(input: {
    label: string;
    dayOfWeek: number | null;
    hourLocal: number;
    minuteLocal: number;
    scopeDays?: number;
    promptHint?: string;
  }): Promise<void> {
    await this.ipc.createBriefSchedule(input);
    await this.refresh();
  }

  async update(schedule: BriefSchedule): Promise<void> {
    await this.ipc.updateBriefSchedule(schedule);
    await this.refresh();
  }

  async remove(scheduleId: string): Promise<void> {
    await this.ipc.deleteBriefSchedule(scheduleId);
    await this.refresh();
  }

  /** Accept a proposed brief → the exported vault path. */
  async accept(runId: string): Promise<string> {
    const path = await this.ipc.acceptBrief(runId);
    await this.refresh();
    return path;
  }

  async dismiss(runId: string): Promise<void> {
    await this.ipc.dismissBrief(runId);
    await this.refresh();
  }
}
