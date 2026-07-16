import {
  ChangeDetectionStrategy,
  Component,
  effect,
  inject,
  signal,
} from "@angular/core";
import { takeUntilDestroyed } from "@angular/core/rxjs-interop";
import { FormControl, ReactiveFormsModule } from "@angular/forms";
import { AuditStore } from "../../../../audit/audit.store";
import { ToastService } from "../../../../../services/toast.service";
import { SettingsStore } from "../../../settings.store";
import { MurToggleComponent } from "../../../../../design-system/toggle/toggle.component";

/**
 * AI & Models → "Search index" block.
 *
 * Owns the semantic-search toggle, the embedding-model download flow, and the
 * re-index controls. The always-on-device honesty rows (Embeddings / Name
 * redaction / Transcription) now live in the "What runs where" map card, so
 * the badges that used to sit here were removed.
 *
 * Also hosts the "Vault hygiene" group: the weekly-vault-audit toggle. Unlike
 * the config-form toggles above it, the schedule is its own backend state
 * (`get_audit_schedule` / `set_audit_schedule`, shared with the Brain-page
 * inbox via {@link AuditStore}), so it binds a standalone control with
 * confirm-then-update: the control only settles on the schedule the backend
 * CONFIRMED — a rejection toasts and the visual reverts.
 *
 * All work is on-device — no cloud calls, no consent requirement.
 */
@Component({
  selector: "app-on-device-intelligence-block",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    MurToggleComponent,ReactiveFormsModule],
  templateUrl: "./on-device-intelligence-block.component.html",
  styleUrl: "./on-device-intelligence-block.component.scss",
})
export class OnDeviceIntelligenceBlockComponent {
  private readonly store = inject(SettingsStore);
  private readonly audit = inject(AuditStore);
  private readonly toast = inject(ToastService);

  readonly form = this.store.form;
  readonly embedModelPresent = this.store.embedModelPresent;
  readonly downloadingEmbedModel = this.store.downloadingEmbedModel;
  readonly embedDownloadFrac = this.store.embedDownloadFrac;
  readonly embedPct = this.store.embedPct;
  readonly embedDownloadError = this.store.embedDownloadError;
  readonly reindexing = this.store.reindexing;
  readonly reindexFrac = this.store.reindexFrac;
  readonly reindexPct = this.store.reindexPct;
  readonly reindexResult = this.store.reindexResult;
  readonly reindexError = this.store.reindexError;

  /**
   * Weekly-vault-audit switch — DISABLED until the schedule loads (an unknown
   * state must not present as "off"), then mirrors the store's confirmed
   * schedule via the `_syncAuditSchedule` effect below.
   */
  readonly auditControl = new FormControl(
    { value: false, disabled: true },
    { nonNullable: true },
  );

  /** A `set_audit_schedule` in flight — blocks re-entry + the effect's resync. */
  private readonly auditBusy = signal(false);

  /**
   * Mirror the CONFIRMED schedule into the control whenever it changes and no
   * commit is in flight. Because `auditBusy` is tracked, the flip back to
   * false after a FAILED commit re-runs this and reverts the visual to the
   * last confirmed state (the store keeps it on rejection).
   */
  private readonly _syncAuditSchedule = effect(() => {
    const s = this.audit.schedule();
    const busy = this.auditBusy();
    if (!s || busy) {
      return;
    }
    this.auditControl.setValue(s.enabled, { emitEvent: false });
    if (this.auditControl.disabled) {
      this.auditControl.enable({ emitEvent: false });
    }
  });

  constructor() {
    void this.audit.loadSchedule();
    // The same commit-on-change idiom the settings form's auto-save uses —
    // programmatic writes above pass `emitEvent: false`, so ONLY a user flip
    // lands here.
    this.auditControl.valueChanges
      .pipe(takeUntilDestroyed())
      .subscribe((v) => void this.commitWeeklyAudit(v));
  }

  downloadEmbedModel(): void {
    void this.store.downloadEmbedModel();
  }

  reindexEmbeddings(): void {
    void this.store.reindexEmbeddings();
  }

  /**
   * Confirm-then-update: disable the switch while `set_audit_schedule` is in
   * flight; on success the store signal carries the response, on failure it
   * keeps the previous schedule — either way the effect resyncs the visual
   * from the CONFIRMED state once `auditBusy` drops.
   */
  private async commitWeeklyAudit(enabled: boolean): Promise<void> {
    this.auditBusy.set(true);
    this.auditControl.disable({ emitEvent: false });
    try {
      await this.audit.setSchedule(enabled);
    } catch (e) {
      this.toast.danger(String(e));
    } finally {
      this.auditControl.enable({ emitEvent: false });
      this.auditBusy.set(false);
    }
  }
}
