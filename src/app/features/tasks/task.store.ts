import { DestroyRef, Injectable, inject, signal } from "@angular/core";
import type {
  OrgStatus,
  OrgTask,
  TaskAssignee,
  TaskDraft,
  TaskLocalRef,
} from "../../core/models";
import { IpcService } from "../../core/ipc.service";

/**
 * Root signal store for org Tasks.
 *
 * Listener admission happens before the first read: a feed event emitted during initial hydration
 * therefore cannot be lost. Any feed change synchronously scrubs stale task plaintext from the
 * renderer, then refetches the SQLCipher projection through the backend-owned read seam.
 */
@Injectable({ providedIn: "root" })
export class TaskStore {
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);

  private readonly _tasks = signal<OrgTask[]>([]);
  private readonly _orgs = signal<OrgStatus[]>([]);
  private readonly _loading = signal(false);
  private readonly _error = signal<string | null>(null);
  private readonly _assignees = signal<Record<string, TaskAssignee[]>>({});
  private readonly _scrubEpoch = signal(0);

  readonly tasks = this._tasks.asReadonly();
  readonly orgs = this._orgs.asReadonly();
  readonly loading = this._loading.asReadonly();
  readonly error = this._error.asReadonly();
  readonly assignees = this._assignees.asReadonly();
  /** Advances before every feed-driven refetch so mounted editors can synchronously drop drafts. */
  readonly scrubEpoch = this._scrubEpoch.asReadonly();

  private initPromise: Promise<void> | null = null;
  private unlisten: (() => void) | null = null;
  private destroyed = false;
  private loadToken = 0;
  private readonly onFocus = (): void => {
    void this.reload();
  };

  constructor() {
    this.destroyRef.onDestroy(() => {
      this.destroyed = true;
      this.unlisten?.();
      window.removeEventListener("focus", this.onFocus);
    });
  }

  init(): Promise<void> {
    if (this.initPromise) return this.initPromise;
    this.initPromise = this.installListenersThenRead();
    return this.initPromise;
  }

  private async installListenersThenRead(): Promise<void> {
    try {
      const unlisten = await this.ipc.onOrgFeedUpdated(() => {
        this.scrubAndReload();
      });
      if (this.destroyed) unlisten();
      else this.unlisten = unlisten;
    } catch {
      // Plain-browser tests have no Tauri event host; the authoritative pull still runs below.
    }
    window.addEventListener("focus", this.onFocus);
    await this.reload();
  }

  private scrubAndReload(): void {
    ++this.loadToken;
    this._scrubEpoch.update((epoch) => epoch + 1);
    this._tasks.set([]);
    this._orgs.set([]);
    this._assignees.set({});
    void this.reload();
  }

  async reload(): Promise<void> {
    const token = ++this.loadToken;
    this._loading.set(true);
    try {
      try {
        await this.ipc.orgRefresh();
      } catch {
        // Offline is expected; the local SQLCipher replica remains authoritative.
      }
      const [orgs, tasks] = await Promise.all([
        this.ipc.orgListStatuses(),
        this.ipc.listTasks(),
      ]);
      if (token !== this.loadToken) return;
      this._orgs.set(orgs.filter((org) => org.contextEnabled));
      this._tasks.set(tasks);
      this._error.set(null);
    } catch (error) {
      if (token === this.loadToken) this._error.set(this.message(error));
    } finally {
      if (token === this.loadToken) this._loading.set(false);
    }
  }

  async create(draft: TaskDraft): Promise<OrgTask | null> {
    try {
      const task = await this.ipc.createTask(draft);
      await this.reload();
      return task;
    } catch (error) {
      this._error.set(this.message(error));
      return null;
    }
  }

  async update(id: string, draft: TaskDraft): Promise<OrgTask | null> {
    try {
      const task = await this.ipc.updateTask(id, draft);
      await this.reload();
      return task;
    } catch (error) {
      this._error.set(this.message(error));
      return null;
    }
  }

  async remove(id: string): Promise<boolean> {
    try {
      await this.ipc.deleteTask(id);
      await this.reload();
      return true;
    } catch (error) {
      this._error.set(this.message(error));
      return false;
    }
  }

  async setAccess(task: OrgTask, access: "view" | "edit"): Promise<boolean> {
    try {
      await this.ipc.orgSetItemAccess(task.itemId, access);
      await this.reload();
      return true;
    } catch (error) {
      this._error.set(this.message(error));
      return false;
    }
  }

  async setLocalRefs(id: string, refs: TaskLocalRef[]): Promise<boolean> {
    try {
      await this.ipc.setTaskLocalRefs(id, refs);
      await this.reload();
      return true;
    } catch (error) {
      this._error.set(this.message(error));
      return false;
    }
  }

  async loadAssignees(orgId: string): Promise<void> {
    if (this._assignees()[orgId]) return;
    const token = this.loadToken;
    try {
      const rows = await this.ipc.taskListAssignees(orgId);
      if (!this.canCommitAssignees(orgId, token)) return;
      this._assignees.update((all) => ({ ...all, [orgId]: rows }));
    } catch (error) {
      if (this.canCommitAssignees(orgId, token)) {
        this._error.set(this.message(error));
      }
    }
  }

  private canCommitAssignees(orgId: string, token: number): boolean {
    return (
      token === this.loadToken &&
      this._orgs().some((org) => org.orgId === orgId && org.contextEnabled)
    );
  }

  clearError(): void {
    this._error.set(null);
  }

  private message(error: unknown): string {
    if (error instanceof Error) return error.message;
    return String(error);
  }
}
