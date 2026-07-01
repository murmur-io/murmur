import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { IpcService } from "../../core/ipc.service";
import type { EgressLedger } from "../../core/models";

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
  template: `
    <div class="card panel">
      <div class="panel-head">
        <h3>Egress &amp; Usage</h3>
        <!-- 7 / 30 / 90 day segmented toggle -->
        <div class="seg" role="group" aria-label="Window">
          @for (r of ranges; track r) {
            <button
              type="button"
              class="seg-btn"
              [class.is-active]="days() === r"
              [attr.aria-pressed]="days() === r"
              (click)="days.set(r)"
            >
              {{ r }}d
            </button>
          }
        </div>
      </div>

      @if (loading()) {
        <p class="empty">Loading…</p>
      }
      @if (error(); as err) {
        <p class="empty eg-error" role="alert">{{ err }}</p>
      }
      @if (ledger(); as l) {
        <!-- Three stat tiles -->
        <div class="eg-tiles">
          <div class="eg-tile">
            <span class="eg-tile-label">Cloud calls</span>
            <span class="eg-tile-value">{{ l.totalCalls }}</span>
          </div>
          <div class="eg-tile">
            <span class="eg-tile-label">Tokens sent</span>
            <span class="eg-tile-value">{{ fmtTokens(l.totalTokens) }}</span>
          </div>
          <div class="eg-tile">
            <span class="eg-tile-label">PII scrubbed</span>
            <span class="eg-tile-value">{{ totalPii() }}</span>
          </div>
        </div>

        <!-- Tokens by model -->
        <div class="eg-section">
          <h4 class="eg-section-title">Tokens by model</h4>
          @if (modelBars().length === 0) {
            <p class="empty-state">
              No cloud calls in this window — everything stayed on-device.
            </p>
          } @else {
            <ul class="eg-bars" aria-label="Tokens by model">
              @for (m of modelBars(); track m.model) {
                <li class="eg-bar-row">
                  <span class="eg-bar-label" [title]="m.model">{{ m.model }}</span>
                  <span class="eg-bar-track" role="presentation">
                    <span class="eg-bar-fill" [style.width.%]="m.pct"></span>
                  </span>
                  <span class="eg-bar-count">{{ fmtTokens(m.tokens) }}</span>
                </li>
              }
            </ul>
          }
        </div>

        <!-- Tokens by day -->
        @if (dayBars().length > 0) {
          <div class="eg-section">
            <h4 class="eg-section-title">Tokens by day</h4>
            <ul class="eg-bars" aria-label="Tokens by day">
              @for (d of dayBars(); track d.day) {
                <li class="eg-bar-row">
                  <span class="eg-bar-label eg-bar-label--day" [title]="d.day">{{ d.day }}</span>
                  <span class="eg-bar-track" role="presentation">
                    <span class="eg-bar-fill" [style.width.%]="d.pct"></span>
                  </span>
                  <span class="eg-bar-count">{{ fmtTokens(d.tokens) }}</span>
                </li>
              }
            </ul>
          </div>
        }

        <!-- What left this device -->
        <div class="eg-section">
          <h4 class="eg-section-title">
            What left this device
            <span class="eg-section-note">(scrubbed before sending)</span>
          </h4>
          <div class="eg-chips">
            <span class="eg-chip">
              <span aria-hidden="true">✉</span>
              {{ l.totalRedactions.email }} email{{ l.totalRedactions.email === 1 ? '' : 's' }}
            </span>
            <span class="eg-chip">
              <span aria-hidden="true">▦</span>
              {{ l.totalRedactions.card }} card-like
            </span>
            <span class="eg-chip">
              <span aria-hidden="true">☎</span>
              {{ l.totalRedactions.phone }} phone{{ l.totalRedactions.phone === 1 ? '' : 's' }}
            </span>
            <span class="eg-chip">
              <span aria-hidden="true">🧑</span>
              {{ l.totalRedactions.name }} name{{ l.totalRedactions.name === 1 ? '' : 's' }}
            </span>
          </div>
        </div>

        <!-- Recent calls -->
        @if (l.recent.length > 0) {
          <div class="eg-section">
            <h4 class="eg-section-title">Recent calls</h4>
            <ul class="eg-recent" aria-label="Recent egress calls">
              <!--
                track $index is correct here (not a rule violation): l.recent is a
                read-only snapshot wholesale-replaced by _ledger.set() — rows are
                never reordered or item-mutated in place. ts is a whole-second Unix
                timestamp; two calls in the same second (e.g. summarise + timeline in
                one meeting on a fast gateway) produce duplicate keys = NG0955 crash.
              -->
              @for (r of l.recent; track $index) {
                <li class="eg-recent-row">
                  <span class="eg-recent-dest" [title]="r.destination">{{ r.destination }}</span>
                  <span class="eg-recent-model">{{ r.modelServed ?? '—' }}</span>
                  <span class="eg-recent-tokens">{{ r.totalTokens !== null ? fmtTokens(r.totalTokens) : '—' }}</span>
                </li>
              }
            </ul>
          </div>
        }
      }
    </div>
  `,
  styles: [
    `
      :host {
        display: block;
      }

      .panel {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
        animation: rise 420ms var(--transition) both;
      }
      .panel-head {
        display: flex;
        align-items: baseline;
        justify-content: space-between;
        gap: var(--space-3);
      }
      .panel-head h3 {
        margin: 0;
      }

      /* --- Range toggle (mirrors weekly-digest .seg) --- */
      .seg {
        display: inline-flex;
        padding: 3px;
        gap: 3px;
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        flex: none;
      }
      .seg-btn {
        padding: var(--space-2) var(--space-3);
        border: none;
        border-radius: var(--radius-sm);
        background: transparent;
        color: var(--text-secondary);
        font-family: inherit;
        font-size: 0.8125rem;
        font-weight: 550;
        font-variant-numeric: tabular-nums;
        line-height: 1;
        cursor: pointer;
        transition:
          background var(--transition),
          color var(--transition);
      }
      .seg-btn:hover:not(.is-active) {
        background: var(--surface-hover);
        color: var(--text-primary);
      }
      .seg-btn.is-active {
        background: var(--accent-soft);
        color: var(--accent-hover);
      }
      .seg-btn:focus-visible {
        outline: none;
        box-shadow: 0 0 0 2px var(--accent-ring);
      }

      /* --- Stat tiles --- */
      .eg-tiles {
        display: grid;
        grid-template-columns: repeat(3, 1fr);
        gap: var(--space-3);
      }
      .eg-tile {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        padding: var(--space-4);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-md);
        background: var(--surface-input);
      }
      .eg-tile-label {
        color: var(--text-muted);
        font-size: 0.75rem;
        font-weight: 550;
        letter-spacing: 0.01em;
        text-transform: uppercase;
      }
      .eg-tile-value {
        color: var(--text-primary);
        font-family: var(--font-mono);
        font-size: 1.5rem;
        font-weight: 600;
        letter-spacing: -0.025em;
        font-variant-numeric: tabular-nums;
      }

      /* --- Section headings --- */
      .eg-section {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .eg-section-title {
        margin: 0;
        font-size: 0.875rem;
        font-weight: 600;
        color: var(--text-primary);
        display: flex;
        align-items: baseline;
        gap: var(--space-2);
      }
      .eg-section-note {
        font-size: 0.75rem;
        font-weight: 400;
        color: var(--text-muted);
      }

      /* --- Tokens-by-model bars --- */
      .eg-bars {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .eg-bar-row {
        display: grid;
        grid-template-columns: 140px 1fr 68px;
        align-items: center;
        gap: var(--space-3);
      }
      .eg-bar-label {
        color: var(--text-secondary);
        font-family: var(--font-mono);
        font-size: 0.75rem;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .eg-bar-track {
        position: relative;
        height: 8px;
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        overflow: hidden;
      }
      .eg-bar-fill {
        position: absolute;
        inset: 0 auto 0 0;
        height: 100%;
        min-width: 4px;
        border-radius: var(--radius-pill);
        background: var(--accent-gradient);
        transform-origin: left;
        animation: grow-fill 560ms var(--ease-spring) both;
      }
      @keyframes grow-fill {
        from { transform: scaleX(0); }
        to   { transform: scaleX(1); }
      }
      .eg-bar-count {
        color: var(--text-secondary);
        font-family: var(--font-mono);
        font-size: 0.8125rem;
        font-variant-numeric: tabular-nums;
        text-align: right;
      }

      /* Day-label column is narrower than the model-label column (YYYY-MM-DD is
         10 chars wide; the shared .eg-bar-label width of 140px is sufficient but
         we apply letter-spacing:0 so the date reads as a clean ISO string). */
      .eg-bar-label--day {
        letter-spacing: 0;
      }

      /* --- PII chips --- */
      .eg-chips {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-2);
      }
      .eg-chip {
        display: inline-flex;
        align-items: center;
        gap: var(--space-1);
        padding: var(--space-1) var(--space-3);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        color: var(--text-secondary);
        font-size: 0.8125rem;
        font-variant-numeric: tabular-nums;
      }

      /* --- Recent calls list --- */
      .eg-recent {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: 2px;
      }
      .eg-recent-row {
        display: grid;
        grid-template-columns: 1fr 140px 72px;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-2) var(--space-3);
        border-radius: var(--radius-sm);
        transition: background var(--transition);
      }
      .eg-recent-row:hover {
        background: var(--surface-hover);
      }
      .eg-recent-dest {
        color: var(--text-primary);
        font-size: 0.8125rem;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .eg-recent-model {
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-size: 0.75rem;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .eg-recent-tokens {
        color: var(--text-secondary);
        font-family: var(--font-mono);
        font-size: 0.8125rem;
        font-variant-numeric: tabular-nums;
        text-align: right;
      }

      /* --- Error + empty --- */
      .eg-error {
        color: var(--danger);
      }

      /* --- Responsive --- */
      @media (max-width: 600px) {
        .eg-tiles {
          grid-template-columns: repeat(2, 1fr);
        }
        .eg-bar-row {
          grid-template-columns: 100px 1fr 56px;
        }
        .eg-recent-row {
          grid-template-columns: 1fr auto;
        }
        .eg-recent-model {
          display: none;
        }
      }
    `,
  ],
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
