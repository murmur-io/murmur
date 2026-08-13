import {
  Injectable,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { filter, fromEvent, interval, merge, scan, startWith } from "rxjs";
import { IpcService } from "../core/ipc.service";
import type { AccountStatus } from "../core/models";
import { ErrorCopyService } from "../core/copy/error-copy.service";

const STATUS_POLL_MS = 60_000;

export type AccountSessionNotice = "signed-out" | "sharing-locked" | null;

/**
 * One app-lifetime snapshot of the optional sharing account. The poll is a
 * local `account_status` read; it never sends content or credentials.
 */
@Injectable({ providedIn: "root" })
export class AccountSessionService {
  private readonly ipc = inject(IpcService);
  private readonly errorCopy = inject(ErrorCopyService);

  private readonly _status = signal<AccountStatus | null>(null);
  readonly status = this._status.asReadonly();
  private readonly _loading = signal(false);
  readonly loading = this._loading.asReadonly();
  private readonly _error = signal<string | null>(null);
  readonly error = this._error.asReadonly();
  private readonly _statusVersion = signal(0);
  private readonly _dismissedStatusVersion = signal<number | null>(null);
  readonly dismissed = computed(
    () => this._dismissedStatusVersion() === this._statusVersion(),
  );
  private refreshSequence = 0;

  readonly notice = computed<AccountSessionNotice>(() => {
    if (this.dismissed()) {
      return null;
    }
    const status = this._status();
    if (!status?.accountExpected) {
      return null;
    }
    if (!status.loggedIn) {
      return "signed-out";
    }
    return status.unlockedForSharing ? null : "sharing-locked";
  });

  private readonly refreshPulse = toSignal(
    merge(
      interval(STATUS_POLL_MS),
      fromEvent(window, "focus"),
      fromEvent(document, "visibilitychange").pipe(
        filter(() => document.visibilityState === "visible"),
      ),
    ).pipe(
      scan((sequence) => sequence + 1, 0),
      startWith(0),
    ),
    { initialValue: 0 },
  );

  constructor() {
    effect(() => {
      this.refreshPulse();
      void this.refresh();
    });
  }

  async refresh(): Promise<void> {
    const sequence = ++this.refreshSequence;
    this._loading.set(true);
    try {
      const status = await this.ipc.accountStatus();
      if (sequence === this.refreshSequence) {
        this.acceptStatus(status);
        this._error.set(null);
      }
    } catch (error) {
      if (sequence === this.refreshSequence) {
        this._error.set(this.errorCopy.because("Couldn’t check sharing status", error));
      }
    } finally {
      if (sequence === this.refreshSequence) {
        this._loading.set(false);
      }
    }
  }

  async unlockWithTouchId(): Promise<void> {
    const sequence = ++this.refreshSequence;
    this._loading.set(true);
    this._error.set(null);
    try {
      const status = await this.ipc.unlockSharingWithBiometric();
      if (sequence === this.refreshSequence) {
        this.accept(status);
      }
    } catch (error) {
      if (sequence === this.refreshSequence) {
        this._error.set(this.errorCopy.because("Couldn’t unlock sharing", error));
      }
    } finally {
      if (sequence === this.refreshSequence) {
        this._loading.set(false);
      }
    }
  }

  accept(status: AccountStatus): void {
    this.refreshSequence += 1;
    this.acceptStatus(status);
    this._error.set(null);
    this._loading.set(false);
  }

  /**
   * Publish a successful logout immediately, without waiting for the next
   * focus/poll read. This carries session flags only — never credentials or
   * note content — and keeps `accountExpected` latched for the sign-in notice.
   */
  acceptLoggedOut(): void {
    const current = this._status();
    this.accept({
      accountExpected: true,
      loggedIn: false,
      email: null,
      unlockedForSharing: false,
      shareConsented: current?.shareConsented ?? false,
      serverConfigured: current?.serverConfigured ?? false,
      biometricUnlockAvailable: false,
    });
  }

  dismissForSession(): void {
    if (this.notice()) {
      this._dismissedStatusVersion.set(this._statusVersion());
    }
  }

  /**
   * Advance the dismissal generation only for a material account transition.
   * Identical focus/poll refreshes keep the current dismissal, while a later
   * signed-out/locked/available state gets its own notice opportunity.
   */
  private acceptStatus(status: AccountStatus): void {
    const current = this._status();
    if (!current || !sameAccountStatus(current, status)) {
      this._statusVersion.update((version) => version + 1);
    }
    this._status.set(status);
  }
}

function sameAccountStatus(left: AccountStatus, right: AccountStatus): boolean {
  return (
    left.accountExpected === right.accountExpected &&
    left.loggedIn === right.loggedIn &&
    left.email === right.email &&
    left.unlockedForSharing === right.unlockedForSharing &&
    left.shareConsented === right.shareConsented &&
    left.serverConfigured === right.serverConfigured &&
    left.biometricUnlockAvailable === right.biometricUnlockAvailable
  );
}
