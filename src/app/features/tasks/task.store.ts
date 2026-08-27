import { DestroyRef, Injectable, inject, signal } from "@angular/core";
import type {
  OrgStatus,
  OrgTask,
  TaskAssignee,
  TaskDraft,
  TaskLocalRef,
} from "../../core/models";
import { IpcService } from "../../core/ipc.service";
import { ErrorCopyService } from "../../core/copy/error-copy.service";

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
  private readonly errors = inject(ErrorCopyService);

  private readonly _tasks = signal<OrgTask[]>([]);
  private readonly _orgs = signal<OrgStatus[]>([]);
  private readonly _loading = signal(false);
  private readonly _error = signal<string | null>(null);
  private readonly _assignees = signal<Record<string, TaskAssignee[]>>({});
  private readonly _scrubEpoch = signal(0);
  private readonly _signedOut = signal(false);

  readonly tasks = this._tasks.asReadonly();
  readonly orgs = this._orgs.asReadonly();
  readonly loading = this._loading.asReadonly();
  readonly error = this._error.asReadonly();
  /**
   * True when the backend refused because this device has no sharing account at all.
   *
   * This is a BEHAVIOUR decision, so it is bound to the stable `[code]`
   * (`errcode::SHARING_ACCOUNT_REQUIRED`), never to the prose — see
   * `core/copy/error-copy.service.ts`. Tasks are org-only, and the default Murmur user has no
   * account, so this is the EXPECTED state for most installs, not a failure: the view renders
   * an invitation instead of an error banner.
   */
  readonly signedOut = this._signedOut.asReadonly();
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
      this._signedOut.set(false);
    } catch (error) {
      if (token === this.loadToken) this.publishFailure(error);
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
      this.publishFailure(error);
      return null;
    }
  }

  async update(id: string, draft: TaskDraft): Promise<OrgTask | null> {
    try {
      const task = await this.ipc.updateTask(id, draft);
      await this.reload();
      return task;
    } catch (error) {
      this.publishFailure(error);
      return null;
    }
  }

  async remove(id: string): Promise<boolean> {
    try {
      await this.ipc.deleteTask(id);
      await this.reload();
      return true;
    } catch (error) {
      this.publishFailure(error);
      return false;
    }
  }

  async setAccess(task: OrgTask, access: "view" | "edit"): Promise<boolean> {
    try {
      await this.ipc.orgSetItemAccess(task.itemId, access);
      await this.reload();
      return true;
    } catch (error) {
      this.publishFailure(error);
      return false;
    }
  }

  async setLocalRefs(id: string, refs: TaskLocalRef[]): Promise<boolean> {
    try {
      await this.ipc.setTaskLocalRefs(id, refs);
      await this.reload();
      return true;
    } catch (error) {
      this.publishFailure(error);
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
        this.publishFailure(error);
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

  /**
   * The ONE place a backend failure becomes state this view renders.
   *
   * Two jobs, in this order:
   *
   * 1. Classify by `[code]`. `sharing-account-required` is not an error on this surface — it is the
   *    default local-first install meeting an org-only feature — so it raises {@link signedOut}
   *    and leaves the banner EMPTY.
   * 2. Humanize everything else. `String(error)` used to render the raw `AppError` `Display` here,
   *    which is exactly the developer prose `src-tauri/src/error.rs` says must never be what the
   *    user reads. `ErrorCopyService` is deny-by-default, so an un-coded failure degrades to a
   *    fixed sentence rather than leaking Rust vocabulary into a banner.
   */
  private publishFailure(error: unknown): void {
    if (this.errors.is(error, "sharing-account-required")) {
      this._signedOut.set(true);
      this._error.set(null);
      this._tasks.set([]);
      this._orgs.set([]);
      this._assignees.set({});
      return;
    }
    this._error.set(this.errors.humanize(error, "tasks"));
  }
}
