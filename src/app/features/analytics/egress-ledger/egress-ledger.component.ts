import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import type { EgressLedger } from "../../../core/models";

/** Selectable rolling-window lengths (in days). */
const RANGES = [7, 30, 90] as const;
type Range = (typeof RANGES)[number];

/**
 * "Egress & Usage" panel — shows the content-free local egress ledger:
 * total cloud calls + tokens, per-model bar chart, PII-scrubbed receipt,
 * and recent calls, for a 7/30/90-day rolling window.
 *
 * Lives in its own file for a separate per-component style budget.
 * Data arrives from the `get_egress_ledger` Tauri command via
 * {@link IpcService.getEgressLedger}; no transcript text ever reaches this view.
 */
@Component({
  selector: "app-egress-ledger",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./egress-ledger.component.html",
  styleUrl: "./egress-ledger.component.scss",
})
export class EgressLedgerComponent {
  private readonly ipc = inject(IpcService);

  readonly ranges = RANGES;

  /** The selected rolling window. Writing this signal re-triggers the load effect. */
  readonly days = signal<Range>(30);

  private readonly _ledger = signal<EgressLedger | null>(null);
  /** The loaded ledger (null while loading or on error). */
  readonly ledger = this._ledger.asReadonly();

  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

  /**
   * Per-model bars with their width % precomputed (denominator = the max model's tokens).
   * Derived once per ledger change so the template reads a plain array — no per-cell method
   * that reads a signal (the zoneless "derive, don't recompute in the template" rule).
   */
  readonly modelBars = computed(() => {
    const l = this._ledger();
    if (!l || l.byModel.length === 0) {
      return [] as { model: string; tokens: number; pct: number }[];
    }
    const max = Math.max(1, ...l.byModel.map((m) => m.tokens));
    return l.byModel.map((m) => ({
      model: m.model,
      tokens: m.tokens,
      pct: Math.round((m.tokens / max) * 100),
    }));
  });

  /**
   * Per-day bars with width % precomputed (denominator = the highest-token day).
   * Same pattern as {@link modelBars} — derived once per ledger change.
   */
  readonly dayBars = computed(() => {
    const l = this._ledger();
    if (!l || l.byDay.length === 0) {
      return [] as { day: string; tokens: number; pct: number }[];
    }
    const max = Math.max(1, ...l.byDay.map((d) => d.tokens));
    return l.byDay.map((d) => ({
      day: d.day,
      tokens: d.tokens,
      pct: Math.round((d.tokens / max) * 100),
    }));
  });

  /** Sum of all four PII redaction kinds. */
  readonly totalPii = computed(() => {
    const r = this._ledger()?.totalRedactions;
    if (!r) {
      return 0;
    }
    return r.email + r.card + r.phone + r.name;
  });

  // T1 — this effect writes `loading` / `error` / `_ledger` (all signals) before
  // and after the async IPC call, so `allowSignalWrites` is REQUIRED in Angular 18.
  private readonly _load = effect(
    () => {
      const d = this.days();
      this.loading.set(true);
      this.error.set(null);
      // Clear the previous window's data so the loading state doesn't render stale bars
      // alongside the spinner during the refetch (clean transition on 7↔30↔90 toggles).
      this._ledger.set(null);
      void this.fetch(d);
    },
    { allowSignalWrites: true },
  );

  /** Compact token formatter: ≥1 000 → "1.2k", ≥1 000 000 → "1.2M". */
  fmtTokens(n: number): string {
    if (n >= 1_000_000) {
      return `${(n / 1_000_000).toFixed(1)}M`;
    }
    if (n >= 1_000) {
      return `${(n / 1_000).toFixed(1)}k`;
    }
    return String(n);
  }

  private async fetch(days: number): Promise<void> {
    try {
      const result = await this.ipc.getEgressLedger(days);
      this._ledger.set(result);
      this.error.set(null);
    } catch (e) {
      this._ledger.set(null);
      this.error.set(e instanceof Error ? e.message : String(e));
    } finally {
      this.loading.set(false);
    }
  }
}
