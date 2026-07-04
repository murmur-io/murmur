import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
} from "@angular/core";
import type { AiMapRow } from "../../../../core/models";
import { SettingsStore } from "../../settings.store";

/**
 * AI & Models → "What runs where": the resolved-map card. A read-only,
 * always-visible mirror of the backend role resolver (`resolved_ai_map`) —
 * one row per AI job with the engine serving it RIGHT NOW, split into two
 * honest groups: rows whose text LEAVES the Mac (cloud, redacted first) and
 * rows that STAY on the Mac. This grouping is the fix for the "I picked Cloud —
 * why is transcription local?" confusion: the always-on-device jobs (Whisper,
 * search, NER, reactions) live under their own heading instead of reading as a
 * contradiction next to the cloud rows.
 *
 * A routable row's "Change" opens Advanced AND asks the role rows to scroll to +
 * flash that role's override row (`requestHighlightRole`). In-flow card (frosted
 * .card is correct — not a floating overlay).
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
          Every AI job and the engine serving it right now — grouped by where your
          words go.
        </p>
      </div>

      @if (rows().length === 0) {
        <p class="text-muted map-empty">Loading the routing map…</p>
      } @else {
        <!-- ☁ Cloud group — text leaves the Mac (redacted first). -->
        <div class="map-group">
          <p class="map-group-label">☁ Goes to the cloud — redacted first</p>
          @if (groups().cloud.length > 0) {
            @for (row of groups().cloud; track row.job) {
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
                <span class="pill map-loc">
                  <span class="pill-dot"></span>
                  {{ row.redacted ? "Cloud · redacted" : "Cloud" }}
                </span>
                @if (row.routable) {
                  <button
                    type="button"
                    class="btn btn-ghost btn-sm map-change"
                    (click)="change(row.job)"
                  >
                    Change
                  </button>
                } @else {
                  <span class="map-change-spacer" aria-hidden="true"></span>
                }
              </div>
            }
          } @else {
            <p class="map-empty-cloud">✓ Nothing leaves this Mac.</p>
          }
        </div>

        <!-- 🖥 On-Mac group — always private. -->
        <div class="map-group">
          <p class="map-group-label">🖥 Stays on your Mac — always private</p>
          @for (row of groups().mac; track row.job) {
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
              <span class="pill map-loc is-success">
                <span class="pill-dot"></span>
                On this Mac
              </span>
              @if (row.routable) {
                <button
                  type="button"
                  class="btn btn-ghost btn-sm map-change"
                  (click)="change(row.job)"
                >
                  Change
                </button>
              } @else {
                <span class="map-change-spacer" aria-hidden="true"></span>
              }
            </div>
          }
        </div>
      }
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
        line-height: 1.5;
      }

      /* ── Cloud vs on-Mac group ── */
      .map-group + .map-group {
        margin-top: var(--space-2);
        padding-top: var(--space-4);
        border-top: 1px solid var(--border-subtle);
      }
      .map-group-label {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        margin: 0 0 var(--space-1);
        font-size: 0.78rem;
        font-weight: 650;
        letter-spacing: 0.03em;
        text-transform: uppercase;
        color: var(--text-muted);
      }
      .map-empty-cloud {
        margin: var(--space-1) 0 0;
        font-size: 0.875rem;
        color: var(--success);
      }

      .map-row {
        display: grid;
        grid-template-columns: minmax(150px, 0.55fr) 1fr auto auto;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-2) 0;
        border-top: 1px solid var(--border-subtle);
      }
      .map-group-label + .map-row {
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
      .btn-sm {
        height: 30px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
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

  /** Rows split by egress: cloud (`!onDevice`) vs on-Mac (`onDevice`). */
  readonly groups = computed<{
    cloud: readonly AiMapRow[];
    mac: readonly AiMapRow[];
  }>(() => {
    const all = this.rows();
    return {
      cloud: all.filter((r) => !r.onDevice),
      mac: all.filter((r) => r.onDevice),
    };
  });

  /**
   * A routable row's "Change" → open Advanced AND ask the role rows to scroll
   * to + flash that role's override row. `job` is `notes`/`ask`/`live` on
   * routable rows (the only rows with a Change button — backend `ai_map_rows`).
   */
  change(job: string): void {
    this.store.expandAdvanced();
    this.store.requestHighlightRole(job as "notes" | "ask" | "live");
  }
}
