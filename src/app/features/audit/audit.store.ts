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
  AuditFinding,
  AuditFindingKind,
  AuditRunSummary,
  AuditSchedule,
} from "../../core/models";
import { ErrorCopyService } from "../../core/copy/error-copy.service";

/** Stable render order for the grouped inbox — most consequential kinds first. */
export const AUDIT_KIND_ORDER: readonly AuditFindingKind[] = [
  "contradiction",
  "stale",
  "broken_link",
  "unlinked_mention",
  "orphan",
];

/** One rendered inbox section: a kind plus its pending findings. */
export interface AuditKindGroup {
  kind: AuditFindingKind;
  findings: AuditFinding[];
}

/**
 * Vault Audit — the FINDINGS-INBOX store (propose-accept), signals-first.
 * Root-provided so the pending rows survive the inbox component's
 * destroy+recreate (stale-while-revalidate, angular-zoneless §8).
 *
 * `init()` is idempotent: the first caller subscribes ONCE to the backend's
 * `EVENT_AUDIT_UPDATED` stream (a run finished, or findings were purged by a
 * seal/delete) and refreshes on every event — the payload shape is deliberately
 * NOT trusted, the store always re-fetches via `listAuditFindings()`. The
 * `UnlistenFn` is released on destroy (the root injector's teardown).
 *
 * Resolving a finding is NEVER optimistic: the row only changes after the
 * backend confirms; a rejection propagates to the caller (which toasts) and
 * the row stays pending.
 */
@Injectable({ providedIn: "root" })
export class AuditStore {
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);
  private readonly errorCopy = inject(ErrorCopyService);

  private readonly _findings = signal<AuditFinding[]>([]);
  readonly findings = this._findings.asReadonly();

  private readonly _loading = signal(true);
  readonly loading = this._loading.asReadonly();

  private readonly _running = signal(false);
  readonly running = this._running.asReadonly();

  private readonly _lastRun = signal<AuditRunSummary | null>(null);
  readonly lastRun = this._lastRun.asReadonly();

  private readonly _error = signal<string | null>(null);
  readonly error = this._error.asReadonly();

  /**
   * The weekly-schedule state — read by the inbox chip AND the Settings
   * toggle row. Null until the first {@link loadSchedule} resolves.
   */
  private readonly _schedule = signal<AuditSchedule | null>(null);
  readonly schedule = this._schedule.asReadonly();

  readonly pendingCount = computed(
    () => this._findings().filter((f) => f.status === "pending").length,
  );

  /** Pending findings grouped by kind in {@link AUDIT_KIND_ORDER}; empty kinds dropped. */
  readonly byKind = computed<AuditKindGroup[]>(() => {
    const pending = this._findings().filter((f) => f.status === "pending");
    return AUDIT_KIND_ORDER.map((kind) => ({
      kind,
      findings: pending.filter((f) => f.kind === kind),
    })).filter((g) => g.findings.length > 0);
  });

  private initialized = false;
  private unlisten: UnlistenFn | null = null;

  /** Subscribe to the audit-updated stream ONCE + load the inbox. Idempotent. */
  init(): void {
    if (this.initialized) {
      void this.load();
      void this.loadSchedule();
      return;
    }
    this.initialized = true;
    void this.ipc
      .onAuditUpdated(() => {
        void this.load();
        // Scheduled runs emit the same event — keep "last run" fresh too.
        void this.loadSchedule();
      })
      .then((un) => (this.unlisten = un));
    this.destroyRef.onDestroy(() => {
      this.unlisten?.();
      this.unlisten = null;
    });
    void this.load();
    void this.loadSchedule();
  }

  /** Reload the pending findings. Cached rows stay visible while in flight. */
  async load(): Promise<void> {
    try {
      const findings = await this.ipc.listAuditFindings();
      this._findings.set(findings);
      this._error.set(null);
    } catch (e) {
      this._error.set(this.errorCopy.humanize(e));
    } finally {
      this._loading.set(false);
    }
  }

  /**
   * Run a full audit pass now, then refresh the inbox. Resolves with the run
   * summary; a rejection propagates to the caller (toast there).
   */
  async runNow(): Promise<AuditRunSummary> {
    this._running.set(true);
    try {
      const summary = await this.ipc.runVaultAudit();
      this._lastRun.set(summary);
      await this.load();
      return summary;
    } finally {
      this._running.set(false);
    }
  }

  /**
   * Reload the weekly-schedule state. A failure leaves the signal as-is — the
   * chip / toggle simply don't render fresher state, the inbox stays usable.
   */
  async loadSchedule(): Promise<void> {
    try {
      const s = await this.ipc.getAuditSchedule();
      this._schedule.set(s);
    } catch {
      // Decorative state — never let it error the inbox.
    }
  }

  /**
   * Turn the weekly audit on/off — confirm-then-update, never optimistic:
   * the signal only takes the CONFIRMED schedule from the response; on
   * rejection it keeps the previous state and the error propagates so the
   * caller can toast + revert its visual.
   */
  async setSchedule(enabled: boolean): Promise<AuditSchedule> {
    const s = await this.ipc.setAuditSchedule(enabled);
    this._schedule.set(s);
    return s;
  }

  /**
   * Accept or dismiss one finding — confirm-then-update, never optimistic:
   * the resolved row from the RESPONSE replaces the pending one (dropping it
   * from the pending groups); on rejection nothing changes and the error
   * propagates so the caller can toast.
   */
  async resolve(
    id: string,
    action: "accept" | "dismiss",
  ): Promise<AuditFinding> {
    const resolved = await this.ipc.resolveAuditFinding(id, action);
    this._findings.update((rows) =>
      rows.map((f) => (f.id === id ? resolved : f)),
    );
    return resolved;
  }
}
