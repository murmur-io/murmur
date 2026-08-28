import { DestroyRef, Injectable, inject, signal } from "@angular/core";

import { IpcService } from "../core/ipc.service";
import type { FilingRecoveryStatus } from "../core/models";

type FilingRecoveryAction = "retry" | "keepExisting" | null;

/**
 * App-lifetime, content-free filing recovery status.
 *
 * The SQLCipher journal remains canonical. This store only holds its aggregate
 * health DTO and refreshes on app start and window focus so a vault conflict
 * resolved outside Murmur can repair without exposing a path or title here.
 */
@Injectable({ providedIn: "root" })
export class FilingRecoveryService {
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);

  private readonly _status = signal<FilingRecoveryStatus | null>(null);
  readonly status = this._status.asReadonly();
  private readonly _loading = signal(false);
  readonly loading = this._loading.asReadonly();
  private readonly _action = signal<FilingRecoveryAction>(null);
  readonly action = this._action.asReadonly();
  private readonly _error = signal<string | null>(null);
  readonly error = this._error.asReadonly();
  private readonly _statusCheckFailed = signal(false);
  readonly statusCheckFailed = this._statusCheckFailed.asReadonly();

  private requestSequence = 0;
  private readonly onWindowFocus = (): void => {
    void this.refresh();
  };

  constructor() {
    window.addEventListener("focus", this.onWindowFocus);
    this.destroyRef.onDestroy(() =>
      window.removeEventListener("focus", this.onWindowFocus),
    );
    void this.refresh();
  }

  async refresh(): Promise<void> {
    if (this._action() !== null) {
      return;
    }
    const sequence = ++this.requestSequence;
    this._loading.set(true);
    try {
      const status = await this.ipc.getFilingRecoveryStatus();
      if (sequence === this.requestSequence) {
        this._status.set(status);
        this._error.set(null);
        this._statusCheckFailed.set(false);
      }
    } catch {
      if (sequence !== this.requestSequence) {
        return;
      }
      if (this._status()?.degraded) {
        // Fixed copy only: storage errors may mention a private vault path.
        this._error.set(
          "Murmur couldn’t refresh recovery status. Your pending recovery data is unchanged.",
        );
      } else {
        this._statusCheckFailed.set(true);
        this._error.set(
          "Murmur couldn’t check whether filing recovery needs attention. No recovery action was taken.",
        );
      }
    } finally {
      if (sequence === this.requestSequence) {
        this._loading.set(false);
      }
    }
  }

  async retry(): Promise<boolean> {
    if (this._action() !== null) {
      return false;
    }
    const sequence = ++this.requestSequence;
    // Invalidate any passive focus refresh without leaving its loading hint set.
    this._loading.set(false);
    this._action.set("retry");
    this._error.set(null);
    try {
      const status = await this.ipc.retryFilingRecovery();
      if (sequence !== this.requestSequence) {
        return false;
      }
      this._status.set(status);
      if (status.degraded) {
        this._error.set(
          "Recovery is still paused. Resolve the conflicting vault file, then retry.",
        );
      }
      return true;
    } catch {
      if (sequence === this.requestSequence) {
        this._error.set(
          "Murmur couldn’t retry recovery safely. No existing vault file was changed.",
        );
      }
      return false;
    } finally {
      if (sequence === this.requestSequence) {
        this._action.set(null);
      }
    }
  }

  async keepExisting(issueToken: string): Promise<boolean> {
    if (this._action() !== null || !issueToken) {
      return false;
    }
    const sequence = ++this.requestSequence;
    // Invalidate any passive focus refresh without leaving its loading hint set.
    this._loading.set(false);
    this._action.set("keepExisting");
    this._error.set(null);
    try {
      const status = await this.ipc.keepExistingFilingFile(issueToken, true);
      if (sequence !== this.requestSequence) {
        return false;
      }
      this._status.set(status);
      return true;
    } catch {
      if (sequence === this.requestSequence) {
        this._error.set(
          "Murmur couldn’t resolve this recovery issue safely. No existing vault file was changed.",
        );
      }
      return false;
    } finally {
      if (sequence === this.requestSequence) {
        this._action.set(null);
      }
    }
  }
}
