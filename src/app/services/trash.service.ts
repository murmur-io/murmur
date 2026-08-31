import { DestroyRef, Injectable, computed, inject, signal } from "@angular/core";
import { IpcService } from "../core/ipc.service";
import type { TrashEntry } from "../core/models";

/**
 * Signal store for the Trash — the recoverable holding area for deleted content.
 *
 * `providedIn: "root"` per `angular-zoneless.md` §8: `/trash` is a LIST route, so
 * it is destroyed and recreated on every navigate-away-and-back. Component-local
 * signals would be wiped to `[]` each time and the view would flash empty before
 * the refetch lands; this instance outlives the component, so a return visit
 * renders the last-known rows instantly while the (still unconditional) reload
 * replaces them underneath.
 *
 * It also owns the sidebar BADGE count, which is why `count` is tracked separately
 * from `entries.length`: the badge must be correct on a cold app start before
 * anyone has opened `/trash`, and `countTrash()` reads no snapshot payloads.
 *
 * The backend is the source of truth for masking — a sealed entry arrives with
 * `locked: true` and no label/detail. This store never infers that and never
 * optimistically mutates a row; every op resolves, then we reload.
 */
@Injectable({ providedIn: "root" })
export class TrashService {
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);

  private readonly _entries = signal<TrashEntry[]>([]);
  private readonly _count = signal(0);
  private readonly _loading = signal(false);
  private readonly _error = signal<string | null>(null);
  private readonly _retentionDays = signal(30);
  /** Entry ids with an op in flight — disables just those rows, never the whole list. */
  private readonly _busyIds = signal<ReadonlySet<string>>(new Set());

  readonly entries = this._entries.asReadonly();
  readonly count = this._count.asReadonly();
  readonly loading = this._loading.asReadonly();
  readonly error = this._error.asReadonly();
  readonly retentionDays = this._retentionDays.asReadonly();
  readonly busyIds = this._busyIds.asReadonly();

  readonly isEmpty = computed(() => this._entries().length === 0);
  /** Entries that cannot be acted on until their folder is unlocked. */
  readonly lockedCount = computed(
    () => this._entries().filter((e) => e.locked).length,
  );
  /** True when every entry is masked — the "unlock a folder to manage these" case. */
  readonly allLocked = computed(
    () => !this.isEmpty() && this.lockedCount() === this._entries().length,
  );

  private unlisten?: () => void;
  /**
   * How many mounted views are showing the list. Only they need the ROWS; the
   * sidebar badge needs the count alone.
   *
   * This matters because `listTrash` makes the backend parse every snapshot payload
   * to derive each row's `detail`, and a meeting payload carries its whole
   * transcript plus hex-encoded inline images. Reloading the list on every delete
   * event — which is what this did first — meant a full payload sweep every time
   * anyone deleted anything, with the Trash view closed and nothing to render it
   * into. The count is what the badge reads, and the count is free.
   */
  private watchers = 0;

  private destroyed = false;

  constructor() {
    // ONE subscription for the whole app: the backend emits a content-free count
    // whenever the trash changes, from any surface. Kept here (not in the view) so
    // the sidebar badge stays live while `/trash` is closed.
    //
    // The `destroyed` guard handles the destroy-before-resolve race the way
    // {@link NotesService} does: `listen()` is async, so a service torn down while it
    // is in flight would otherwise store the unlisten handle AFTER `onDestroy` already
    // ran and leak the subscription.
    //
    // The `.catch()` matters for the E2E suite: its hand-written `invoke`/`listen`
    // mocks do not know every event, and a rejected registration with no handler
    // surfaces as an unhandled promise rejection that can fail an unrelated spec. A
    // trash badge that never updates is the correct degradation here.
    this.destroyRef.onDestroy(() => {
      this.destroyed = true;
      this.unlisten?.();
    });
    void this.ipc
      .onTrashUpdated((p) => {
        this._count.set(p.count);
        if (this.watchers > 0) {
          void this.reload();
        }
      })
      .then((un) => {
        if (this.destroyed) {
          un();
        } else {
          this.unlisten = un;
        }
      })
      .catch(() => {
        // No listener: the badge stays at its last value. Never fatal.
      });
  }

  /**
   * Register a mounted list view. Returns the release callback — the caller wires it
   * to its own `DestroyRef` so a destroyed view stops pulling payloads.
   */
  watch(): () => void {
    this.watchers += 1;
    let released = false;
    return () => {
      // Guard against a double release: two decrements from one view would leave the
      // counter negative and silently disable refresh for every OTHER open view.
      if (released) {
        return;
      }
      released = true;
      this.watchers = Math.max(0, this.watchers - 1);
    };
  }

  /** Refresh the badge count only. Safe to call on app start; reads no payloads. */
  async refreshCount(): Promise<void> {
    try {
      this._count.set(await this.ipc.countTrash());
    } catch {
      // A failed count must never break the shell — the badge just stays stale.
    }
  }

  /**
   * Reload the list + retention. Leaves the previous rows in place while it runs,
   * so the view can gate its spinner on `isEmpty() && loading()` and a return
   * visit never flashes empty.
   */
  async reload(): Promise<void> {
    this._loading.set(true);
    this._error.set(null);
    try {
      const [entries, days] = await Promise.all([
        this.ipc.listTrash(),
        this.ipc.getTrashRetentionDays(),
      ]);
      this._entries.set(entries);
      this._retentionDays.set(days);
      this._count.set(entries.length);
    } catch (e) {
      this._error.set(this.message(e));
    } finally {
      this._loading.set(false);
    }
  }

  /**
   * Restore one entry. Returns the error message on failure (the caller toasts
   * it) or `null` on success — a locked entry is refused by the BACKEND, which is
   * the only authority on that.
   */
  async restore(entryId: string): Promise<string | null> {
    return this.run(entryId, () => this.ipc.restoreTrashItem(entryId));
  }

  /** Permanently destroy one entry. Irreversible. */
  async deleteForever(entryId: string): Promise<string | null> {
    return this.run(entryId, () => this.ipc.deleteTrashItemForever(entryId));
  }

  /**
   * Permanently destroy every unlocked entry. Returns the purged count, or an
   * error message. Locked entries are left behind by the backend, so the count
   * can be lower than the list length.
   */
  async emptyAll(): Promise<{ purged: number } | { error: string }> {
    this._loading.set(true);
    try {
      const purged = await this.ipc.emptyTrash();
      await this.reload();
      return { purged };
    } catch (e) {
      this._error.set(this.message(e));
      return { error: this.message(e) };
    } finally {
      this._loading.set(false);
    }
  }

  /**
   * Reconcile expired entries when the view opens, instead of waiting up to an
   * hour for the background tick — otherwise the list can show a row that is
   * already past its date. Best-effort and silent: the caller has already loaded
   * the list, and a purge failure is the tick's problem, not the user's.
   */
  async purgeExpiredOnOpen(): Promise<void> {
    try {
      const purged = await this.ipc.purgeExpiredTrash();
      if (purged > 0) {
        await this.reload();
      }
    } catch {
      // Silent by design — see the doc comment.
    }
  }

  /** Change the retention window. Applies to entries already in the trash. */
  async setRetentionDays(days: number): Promise<string | null> {
    try {
      await this.ipc.setTrashRetentionDays(days);
      await this.reload();
      return null;
    } catch (e) {
      const msg = this.message(e);
      this._error.set(msg);
      return msg;
    }
  }

  private async run(
    entryId: string,
    op: () => Promise<void>,
  ): Promise<string | null> {
    this.markBusy(entryId, true);
    try {
      await op();
      await this.reload();
      return null;
    } catch (e) {
      return this.message(e);
    } finally {
      this.markBusy(entryId, false);
    }
  }

  private markBusy(entryId: string, busy: boolean): void {
    const next = new Set(this._busyIds());
    if (busy) {
      next.add(entryId);
    } else {
      next.delete(entryId);
    }
    this._busyIds.set(next);
  }

  /**
   * The backend's `AppError` crosses IPC as a `{ kind: message }` object, so a
   * bare `String(e)` renders "[object Object]". Pull the message out.
   */
  private message(e: unknown): string {
    if (typeof e === "string") {
      return e;
    }
    if (e && typeof e === "object") {
      const values = Object.values(e as Record<string, unknown>);
      const first = values.find((v) => typeof v === "string");
      if (typeof first === "string") {
        return first;
      }
    }
    return "Something went wrong.";
  }
}
