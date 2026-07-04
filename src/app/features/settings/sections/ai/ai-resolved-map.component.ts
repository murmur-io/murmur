import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { SettingsStore } from "../../settings.store";

/**
 * AI & Models → "What runs where": the resolved-map card. A read-only,
 * always-visible mirror of the backend role resolver (`resolved_ai_map`) —
 * one row per AI job with the engine serving it RIGHT NOW. This is the
 * honesty layer of the posture redesign: the posture preset chooses, this
 * table shows the outcome, and a routable row's "Change" opens Advanced.
 * In-flow card (frosted .card is correct — not a floating overlay).
 */
@Component({
  selector: "app-ai-resolved-map",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="card map-card">
      <div class="map-head">
        <h3>What runs where</h3>
        <p class="text-secondary map-sub">
          Every AI job and the engine serving it right now.
        </p>
      </div>
      <div class="map-rows">
        @for (row of rows(); track row.job) {
          <div class="map-row" [class.is-inactive]="!row.active">
            <span class="map-title">{{ row.title }}</span>
            <span class="map-engine">
              {{ row.engine }}
              @if (row.model) {
                <span class="map-model text-muted">· {{ row.model }}</span>
              }
              @if (!row.active) {
                <span class="map-off text-muted">— off</span>
              }
            </span>
            <span class="pill map-loc" [class.is-success]="row.onDevice">
              <span class="pill-dot"></span>
              {{ row.onDevice ? "On this Mac" : "Cloud · redacted" }}
            </span>
            @if (row.routable) {
              <button
                type="button"
                class="btn btn-ghost btn-sm map-change"
                (click)="change()"
              >
                Change
              </button>
            } @else {
              <span class="map-change-spacer" aria-hidden="true"></span>
            }
          </div>
        } @empty {
          <p class="text-muted map-empty">Loading the routing map…</p>
        }
      </div>
    </div>
  `,
  styles: [
    `
      :host {
        display: contents;
      }
      .map-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .map-head {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .map-head h3 {
        margin: 0;
      }
      .map-sub {
        margin: 0;
        font-size: 0.8125rem;
      }
      .map-rows {
        display: flex;
        flex-direction: column;
      }
      .map-row {
        display: grid;
        grid-template-columns: minmax(150px, 0.55fr) 1fr auto auto;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-2) 0;
        border-top: 1px solid var(--border-subtle);
      }
      .map-row:first-of-type {
        border-top: none;
      }
      .map-row.is-inactive .map-title,
      .map-row.is-inactive .map-engine {
        opacity: 0.55;
      }
      .map-title {
        color: var(--text-secondary);
        font-size: 0.875rem;
        font-weight: 550;
      }
      .map-engine {
        display: flex;
        align-items: baseline;
        gap: var(--space-1);
        flex-wrap: wrap;
        font-size: 0.875rem;
        color: var(--text-primary);
        min-width: 0;
      }
      .map-model,
      .map-off,
      .map-empty {
        font-size: 0.8125rem;
      }
      .map-empty {
        margin: 0;
      }
      .map-loc {
        flex: none;
      }
      .map-change {
        flex: none;
        white-space: nowrap;
      }
      .map-change-spacer {
        width: 1px;
      }
    `,
  ],
})
export class AiResolvedMapComponent {
  private readonly store = inject(SettingsStore);
  readonly rows = this.store.aiMap;

  /** A routable row's Change → open the Advanced disclosure below. */
  change(): void {
    this.store.expandAdvanced();
  }
}
