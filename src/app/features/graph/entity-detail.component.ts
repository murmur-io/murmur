import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
} from "@angular/core";
import { IpcService } from "../../core/ipc.service";
import type { EntityDetail, EntityNeighbor } from "../../core/models";
import { SourcesComponent } from "../../shared/sources.component";
import { EntityNeighborhoodComponent } from "./entity-neighborhood.component";

/**
 * The right-hand detail panel for one selected entity. Loads its
 * {@link EntityDetail} via IPC (re-loading whenever the `entityId` input
 * changes), then renders: a header (name + kind chip), the bounded
 * neighborhood SVG, the backlinked meetings as reusable `app-sources` chips
 * (→ /meeting/:id), and a neighbors list where each row re-selects that entity.
 *
 * Loading/error/empty are all handled honestly. The IPC call is a one-shot
 * awaited promise (not a data stream), so it's loaded imperatively inside an
 * effect that tracks the input signal — no `subscribe`, no markForCheck.
 */
@Component({
  selector: "app-entity-detail",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [SourcesComponent, EntityNeighborhoodComponent],
  template: `
    <aside class="ed card" aria-label="Entity detail">
      <button
        type="button"
        class="ed-close btn btn-ghost"
        aria-label="Close detail"
        (click)="close.emit()"
      >
        <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
          <path
            d="M4 4l8 8M12 4l-8 8"
            stroke="currentColor"
            stroke-width="1.6"
            stroke-linecap="round"
          />
        </svg>
      </button>

      @if (loading()) {
        <div class="ed-state">
          <p class="empty">Loading…</p>
        </div>
      } @else if (error()) {
        <div class="ed-state">
          <p class="empty-title">Couldn’t load this entity</p>
          <p class="empty">{{ error() }}</p>
        </div>
      } @else if (detail()) {
        @if (detail(); as d) {
          <header class="ed-head">
            <span
              class="ed-kind-dot"
              [class.is-project]="d.entity.kind === 'project'"
              aria-hidden="true"
            ></span>
            <div class="ed-head-text">
              <h3 class="ed-name">{{ d.entity.name }}</h3>
              <span class="ed-kind">
                {{ d.entity.kind === "project" ? "Project" : "Person" }}
                · {{ meetingCountLabel() }}
              </span>
            </div>
          </header>

          <!-- Bounded neighborhood decoration (computed once, no simulation). -->
          <app-entity-neighborhood
            class="ed-neighborhood"
            [neighbors]="d.neighbors"
            [centerName]="d.entity.name"
            (select)="select.emit($event)"
          />

          <!-- Backlinked meetings → reuse the shared sources chip component. -->
          <section class="ed-section">
            <h4 class="ed-section-title">Mentioned in</h4>
            @if (d.meetings.length) {
              <app-sources [sources]="d.meetings" [limit]="6" />
            } @else {
              <p class="empty ed-section-empty">
                No visible meetings mention this entity right now.
              </p>
            }
          </section>

          <!-- Neighbors: click to pivot the detail panel to that entity. -->
          @if (d.neighbors.length) {
            <section class="ed-section">
              <h4 class="ed-section-title">Connected entities</h4>
              <ul class="ed-neighbors">
                @for (nb of d.neighbors; track nb.id) {
                  <li>
                    <button
                      type="button"
                      class="ed-neighbor"
                      [class.is-project]="nb.kind === 'project'"
                      (click)="select.emit(nb.id)"
                    >
                      <span class="ed-neighbor-dot" aria-hidden="true"></span>
                      <span class="ed-neighbor-name">{{ nb.name }}</span>
                      <span
                        class="count ed-neighbor-count"
                        [attr.title]="sharedLabel(nb)"
                      >
                        {{ nb.sharedMeetings }}
                      </span>
                    </button>
                  </li>
                }
              </ul>
            </section>
          }
        }
      }
    </aside>
  `,
  styles: [
    `
      :host {
        display: block;
      }
      .ed {
        position: relative;
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
        padding: var(--space-5);
        animation: rise 360ms var(--transition) both;
      }
      .ed-close {
        position: absolute;
        top: var(--space-3);
        right: var(--space-3);
        width: 32px;
        height: 32px;
        padding: 0;
      }
      .ed-state {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        padding: var(--space-6) var(--space-2);
        text-align: center;
      }

      /* --- Header --- */
      .ed-head {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        padding-right: var(--space-6);
      }
      .ed-kind-dot {
        flex: none;
        width: 12px;
        height: 12px;
        border-radius: var(--radius-pill);
        background: var(--accent);
        box-shadow: 0 0 0 4px var(--accent-soft);
      }
      .ed-kind-dot.is-project {
        background: #9d7bff;
        box-shadow: 0 0 0 4px rgba(157, 123, 255, 0.18);
      }
      .ed-head-text {
        display: flex;
        flex-direction: column;
        gap: 2px;
        min-width: 0;
      }
      .ed-name {
        margin: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .ed-kind {
        color: var(--text-muted);
        font-size: 0.75rem;
        font-weight: 600;
        letter-spacing: 0.04em;
        text-transform: uppercase;
        font-variant-numeric: tabular-nums;
      }

      .ed-neighborhood {
        display: block;
      }

      /* --- Sections --- */
      .ed-section {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .ed-section-title {
        margin: 0;
        color: var(--text-secondary);
        font-size: 0.6875rem;
        font-weight: 600;
        letter-spacing: 0.08em;
        text-transform: uppercase;
      }
      .ed-section-empty {
        margin: 0;
        font-size: 0.875rem;
      }

      /* --- Neighbors list --- */
      .ed-neighbors {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .ed-neighbor {
        display: grid;
        grid-template-columns: auto 1fr auto;
        align-items: center;
        gap: var(--space-3);
        width: 100%;
        padding: var(--space-2) var(--space-3);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        color: var(--text-primary);
        font-family: inherit;
        font-size: 0.875rem;
        text-align: left;
        cursor: pointer;
        transition:
          background var(--transition),
          border-color var(--transition);
      }
      .ed-neighbor:hover {
        background: var(--surface-hover);
        border-color: var(--border-strong);
      }
      .ed-neighbor:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .ed-neighbor-dot {
        flex: none;
        width: 7px;
        height: 7px;
        border-radius: var(--radius-pill);
        background: var(--accent);
      }
      .ed-neighbor.is-project .ed-neighbor-dot {
        background: #9d7bff;
      }
      .ed-neighbor-name {
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        font-weight: 550;
      }
      .ed-neighbor-count {
        flex: none;
        min-width: 22px;
        height: 22px;
      }
    `,
  ],
})
export class EntityDetailComponent {
  private readonly ipc = inject(IpcService);

  /** The entity to show detail for; changing it re-loads the panel. */
  readonly entityId = input.required<string>();
  /** Emits when the user picks a neighbor — the container re-selects it. */
  readonly select = output<string>();
  /** Emits when the user dismisses the panel. */
  readonly close = output<void>();

  readonly detail = signal<EntityDetail | null>(null);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

  protected readonly meetingCountLabel = computed(() => {
    const n = this.detail()?.meetings.length ?? 0;
    return n === 1 ? "1 meeting" : `${n} meetings`;
  });

  /**
   * Re-load the detail whenever `entityId` changes. The IPC call is a one-shot
   * promise, so we await it inside an effect that tracks the input signal — a
   * stale-result guard drops responses that resolve after the id moved on
   * (fast neighbor-to-neighbor pivots), so the panel never shows mismatched data.
   */
  private readonly _load = effect(
    () => {
      const id = this.entityId();
      this.loading.set(true);
      this.error.set(null);
      void this.fetch(id);
    },
    // Sets loading/error synchronously inside the tracked effect, so writes
    // must be permitted here.
    { allowSignalWrites: true },
  );

  private async fetch(id: string): Promise<void> {
    try {
      const result = await this.ipc.getEntityDetail(id);
      // Guard against an out-of-order resolution (the selection moved on).
      if (this.entityId() !== id) {
        return;
      }
      this.detail.set(result);
    } catch (e) {
      if (this.entityId() !== id) {
        return;
      }
      this.detail.set(null);
      this.error.set(String(e));
    } finally {
      if (this.entityId() === id) {
        this.loading.set(false);
      }
    }
  }

  protected sharedLabel(nb: EntityNeighbor): string {
    return nb.sharedMeetings === 1
      ? "1 shared meeting"
      : `${nb.sharedMeetings} shared meetings`;
  }
}
