import { Injectable, computed, inject, signal } from "@angular/core";
import { IpcService } from "../core/ipc.service";
import type { ActiveSharesReport } from "../core/models";
import { FoldersService } from "./folders.service";

/**
 * A folder-lock request that is BLOCKED pending the lock×shares dialog: the folder
 * being locked plus the probe outcome. `report` is null when the share PROBE itself
 * failed — in that case the dialog is shown FAIL-CLOSED (warn, never lock silently).
 */
export interface PendingFolderLock {
  folderId: string;
  folderName: string;
  /** The active-shares report, or null when the probe errored (fail-closed warn). */
  report: ActiveSharesReport | null;
  /** True when the probe itself failed (older backend / sharing off / transient). */
  probeFailed: boolean;
  /** Host-supplied refresh, run after a lock actually lands (revoke&lock / lock anyway). */
  onLocked: () => Promise<void> | void;
}

/**
 * Shared lock×shares flow (Shared Brain v1 + PK-F1 remediation).
 *
 * BOTH the meetings tree ({@link FolderRowComponent}) and the Notes rail
 * ({@link NotesHomeComponent}) must run the SAME "probe active shares → warn/revoke
 * dialog → then lock" flow before sealing a folder, so a folder with live shares is
 * never sealed without the owner deciding what happens to those shares. Previously
 * only the tree ran it; the Notes rail called `FoldersService.lock` directly and
 * bypassed the dialog (PK-F1). This root singleton owns the flow ONCE so neither
 * call site duplicates (or diverges on) the probe/revoke/lock orchestration.
 *
 * FAIL-CLOSED (fixes F5): if `folder_active_shares` ERRORS we do NOT lock silently —
 * we surface the dialog (probeFailed) so the user explicitly chooses, rather than the
 * old fail-open behavior that could seal a shared folder on a transient probe error.
 *
 * The service holds only the flow STATE + the async orchestration; each host renders
 * the reusable {@link LockSharesDialogComponent} bound to these signals and wires its
 * button outputs to {@link revokeAndLock} / {@link lockAnyway} / {@link cancel}, then
 * runs its own view refresh via the `onLocked` callback it passed to {@link requestLock}.
 * A lock is only ever performed through {@link FoldersService.lock}, so the backend
 * stays the single source of truth for lock state.
 */
@Injectable({ providedIn: "root" })
export class FolderLockFlowService {
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);

  private readonly _pending = signal<PendingFolderLock | null>(null);
  private readonly _busy = signal(false);
  private readonly _error = signal<string | null>(null);

  /** The blocked lock request while the dialog is open (null = no dialog). */
  readonly pending = this._pending.asReadonly();
  /** True while a revoke/lock sequence driven by the dialog is in flight. */
  readonly busy = this._busy.asReadonly();
  /** Non-null when the last dialog-driven revoke/lock failed (cleared on the next action). */
  readonly error = this._error.asReadonly();

  /** The active-shares report to render, safe even when the probe failed (all-zero). */
  readonly report = computed<ActiveSharesReport>(
    () => this._pending()?.report ?? { links: 0, users: 0, org: [] },
  );

  /**
   * Begin locking `folderId`. Probes active shares FIRST:
   *  - shares exist → open the dialog (Revoke & lock / Lock anyway / Cancel); DON'T lock yet.
   *  - probe ERRORS → open the dialog FAIL-CLOSED (probeFailed); DON'T lock silently.
   *  - no shares → lock directly via {@link FoldersService.lock} + run `onLocked`.
   *
   * `onLocked` runs the HOST's own post-lock refresh (the tree re-renders reactively; the
   * Notes rail must reload its note lists). Rejections from the direct lock propagate so the
   * caller can surface a host-appropriate error.
   */
  async requestLock(
    folderId: string,
    folderName: string,
    onLocked: () => Promise<void> | void,
  ): Promise<void> {
    if (this._busy() || this._pending()) {
      return;
    }
    this._error.set(null);
    let report: ActiveSharesReport | null;
    let probeFailed = false;
    try {
      report = await this.ipc.folderActiveShares(folderId);
    } catch {
      // FAIL-CLOSED (F5): a failed probe must NOT lock silently — warn via the dialog instead.
      report = null;
      probeFailed = true;
    }
    const hasShares =
      !!report && report.links + report.users + report.org.length > 0;
    if (hasShares || probeFailed) {
      this._pending.set({ folderId, folderName, report, probeFailed, onLocked });
      return;
    }
    // No shares and a clean probe → lock straight away.
    await this.folders.lock(folderId);
    await onLocked();
  }

  /** Dialog: revoke EVERY share for the folder, then lock. Then run the host refresh. */
  async revokeAndLock(): Promise<void> {
    const pending = this._pending();
    if (!pending || this._busy()) {
      return;
    }
    this._busy.set(true);
    this._error.set(null);
    try {
      await this.ipc.revokeSharesForFolder(pending.folderId);
      await this.folders.lock(pending.folderId);
      await pending.onLocked();
      this._pending.set(null);
    } catch {
      this._error.set("Couldn’t revoke the shares and lock. Try again.");
    } finally {
      this._busy.set(false);
    }
  }

  /** Dialog: lock while leaving the shares live. Then run the host refresh. */
  async lockAnyway(): Promise<void> {
    const pending = this._pending();
    if (!pending || this._busy()) {
      return;
    }
    this._busy.set(true);
    this._error.set(null);
    try {
      await this.folders.lock(pending.folderId);
      await pending.onLocked();
      this._pending.set(null);
    } catch {
      this._error.set("Couldn’t lock this folder. Try again.");
    } finally {
      this._busy.set(false);
    }
  }

  /** Dialog: cancel — dismiss without locking. */
  cancel(): void {
    if (this._busy()) {
      return;
    }
    this._pending.set(null);
    this._error.set(null);
  }
}
