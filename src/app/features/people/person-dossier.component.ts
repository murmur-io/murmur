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
import { RouterLink } from "@angular/router";

import { IpcService } from "../../core/ipc.service";
import type {
  Commitment,
  DossierData,
  EntityNeighbor,
} from "../../core/models";

/**
 * The `/people` detail pane — a STRUCTURED, glanceable dossier for one person
 * (Tier 4b). Loads the gated {@link DossierData} via {@link IpcService.getPersonDossier}
 * (re-loading whenever the `entityId` input changes) and renders four native
 * sections over it:
 *   • 🕑 a TIMELINE of the meetings that mention this person (newest first → /meeting),
 *   • ⏳ WHO OWES WHAT — the open commitments tied to them (owner · due · item · source),
 *   • 🔵 FACTS — the current bitemporal state (open facts) plus WHAT CHANGED (closed),
 *   • 🧭 co-occurring NEIGHBOURS — click to pivot the pane to that entity.
 *
 * This replaces the reused graph {@link EntityDetailComponent} in the pane: strictly
 * richer, deterministic, and egress-free (no cloud synthesis). The component contract
 * (`entityId` input + `select`/`close` outputs) matches the panel it supersedes, so
 * the container wiring is unchanged.
 *
 * The IPC call is a one-shot awaited promise (not a data stream), loaded imperatively
 * inside an `effect()` that tracks the input signal — a stale-result guard drops a
 * response that resolves after the id moved on (fast neighbour-to-neighbour pivots),
 * and `allowSignalWrites` is required because loading/error are written synchronously
 * inside that tracked effect (NG0600 guard). Mirrors `entity-detail.component.ts`.
 */
@Component({
  selector: "app-person-dossier",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  template: `
    <aside class="dossier card" aria-label="Person dossier">
      <button
        type="button"
        class="dossier-close btn btn-ghost"
        aria-label="Close dossier"
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
        <div class="dossier-state">
          <p class="empty">Loading…</p>
        </div>
      } @else if (error()) {
        <div class="dossier-state">
          <p class="empty-title">Couldn’t load this dossier</p>
          <p class="empty">{{ error() }}</p>
        </div>
      } @else if (dossier()) {
        @if (dossier(); as d) {
          <header class="dossier-head">
            <span
              class="dossier-dot"
              [class.is-project]="d.entity.kind === 'project'"
              aria-hidden="true"
            ></span>
            <div class="dossier-head-text">
              <h3 class="dossier-name">{{ d.entity.name }}</h3>
              <span class="dossier-sub">
                {{ d.entity.kind === "project" ? "Project" : "Person" }}
                · {{ summaryLabel() }}
              </span>
            </div>
          </header>

          <!-- 🕑 Timeline of mentioning meetings (newest first). -->
          <section class="dossier-section">
            <h4 class="dossier-section-title">
              <span class="dossier-section-emoji" aria-hidden="true">🕑</span>
              Timeline
            </h4>
            <ol class="tl">
              @for (m of d.meetings; track m.meetingId) {
                <li class="tl-row">
                  <a class="tl-link" [routerLink]="['/meeting', m.meetingId]">
                    <span class="tl-node" aria-hidden="true"></span>
                    <span class="tl-date">{{ fmtDate(m.startedAt) }}</span>
                    <span class="tl-title">{{ m.title || "(untitled)" }}</span>
                  </a>
                </li>
              } @empty {
                <li class="dossier-empty-row">
                  <p class="empty">
                    No visible meetings mention this person yet.
                  </p>
                </li>
              }
            </ol>
          </section>

          <!-- ⏳ Who owes what — open commitments tied to this person. -->
          <section class="dossier-section">
            <h4 class="dossier-section-title">
              <span class="dossier-section-emoji" aria-hidden="true">⏳</span>
              Who owes what
              @if (d.commitments.length) {
                <span class="count dossier-count">{{
                  d.commitments.length
                }}</span>
              }
            </h4>
            <ul class="ci-list">
              @for (c of d.commitments; track c.meetingId + "|" + c.text) {
                <li class="ci">
                  <div class="ci-main">
                    @if (ownerLabel(c); as o) {
                      <span class="ci-owner">{{ o }}</span>
                    }
                    <span class="ci-text">{{ c.text }}</span>
                  </div>
                  <div class="ci-meta">
                    @if (c.dueDate) {
                      <span class="pill is-warning ci-due"
                        >due {{ c.dueDate }}</span
                      >
                    }
                    <a class="ci-src" [routerLink]="['/meeting', c.meetingId]">
                      {{ c.meetingTitle || "(untitled)" }}
                    </a>
                  </div>
                </li>
              } @empty {
                <li class="dossier-empty-row">
                  <p class="empty">All caught up — no open commitments.</p>
                </li>
              }
            </ul>
          </section>

          <!-- 🔵 Facts — current state (open) + what changed (closed history). -->
          @if (d.facts.length) {
            <section class="dossier-section">
              <h4 class="dossier-section-title">
                <span class="dossier-section-emoji" aria-hidden="true">🔵</span>
                Facts
              </h4>

              @if (currentFacts().length) {
                <p class="dossier-sub-label">Current</p>
                <ul class="fact-list">
                  @for (f of currentFacts(); track f.id) {
                    <li class="fact fact-current">
                      <span class="fact-pred">{{ f.predicate }}</span>
                      <span class="fact-obj">{{ f.object }}</span>
                      <span class="fact-since"
                        >since {{ fmtDate(f.validFrom) }}</span
                      >
                    </li>
                  }
                </ul>
              }

              @if (changedFacts().length) {
                <p class="dossier-sub-label">What changed</p>
                <ul class="fact-list">
                  @for (f of changedFacts(); track f.id) {
                    <li class="fact fact-changed">
                      <span class="fact-pred">{{ f.predicate }}</span>
                      <span class="fact-obj">was “{{ f.object }}”</span>
                      <span class="fact-since">
                        {{ fmtDate(f.validFrom) }} → {{ fmtDate(f.validTo) }}
                      </span>
                    </li>
                  }
                </ul>
              }
            </section>
          }

          <!-- 🧭 Co-occurring neighbours — click to pivot the pane. -->
          @if (d.neighbors.length) {
            <section class="dossier-section">
              <h4 class="dossier-section-title">
                <span class="dossier-section-emoji" aria-hidden="true">🧭</span>
                Also appears with
              </h4>
              <ul class="nb-list">
                @for (nb of d.neighbors; track nb.id) {
                  <li>
                    <button
                      type="button"
                      class="nb"
                      [class.is-project]="nb.kind === 'project'"
                      (click)="select.emit(nb.id)"
                    >
                      <span class="nb-dot" aria-hidden="true"></span>
                      <span class="nb-name">{{ nb.name }}</span>
                      <span
                        class="count nb-count"
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
      .dossier {
        position: relative;
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
        padding: var(--space-5);
        animation: rise 360ms var(--transition) both;
      }
      .dossier-close {
        position: absolute;
        top: var(--space-3);
        right: var(--space-3);
        width: 32px;
        height: 32px;
        padding: 0;
      }
      .dossier-state {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        padding: var(--space-6) var(--space-2);
        text-align: center;
      }

      /* --- Header --- */
      .dossier-head {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        padding-right: var(--space-6);
      }
      .dossier-dot {
        flex: none;
        width: 12px;
        height: 12px;
        border-radius: var(--radius-pill);
        background: var(--accent);
        box-shadow: 0 0 0 4px var(--accent-soft);
      }
      .dossier-dot.is-project {
        background: #9d7bff;
        box-shadow: 0 0 0 4px rgba(157, 123, 255, 0.18);
      }
      .dossier-head-text {
        display: flex;
        flex-direction: column;
        gap: 2px;
        min-width: 0;
      }
      .dossier-name {
        margin: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .dossier-sub {
        color: var(--text-muted);
        font-size: 0.75rem;
        font-weight: 600;
        letter-spacing: 0.04em;
        text-transform: uppercase;
        font-variant-numeric: tabular-nums;
      }

      /* --- Sections --- */
      .dossier-section {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .dossier-section-title {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        margin: 0;
        color: var(--text-secondary);
        font-size: 0.6875rem;
        font-weight: 600;
        letter-spacing: 0.08em;
        text-transform: uppercase;
      }
      .dossier-section-emoji {
        font-size: 0.8125rem;
        line-height: 1;
      }
      .dossier-count {
        margin-left: auto;
      }
      .dossier-sub-label {
        margin: var(--space-1) 0 0;
        color: var(--text-muted);
        font-size: 0.6875rem;
        font-weight: 600;
        letter-spacing: 0.06em;
        text-transform: uppercase;
      }
      .dossier-empty-row {
        list-style: none;
      }
      .dossier-empty-row .empty {
        font-size: 0.875rem;
      }

      /* --- Timeline --- */
      .tl {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: 2px;
      }
      .tl-link {
        display: grid;
        grid-template-columns: auto auto 1fr;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-2) var(--space-3);
        border-radius: var(--radius-md);
        color: var(--text-primary);
        text-decoration: none;
        transition: background var(--transition-fast);
      }
      .tl-link:hover {
        background: var(--surface-hover);
      }
      .tl-link:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .tl-node {
        flex: none;
        width: 7px;
        height: 7px;
        border-radius: var(--radius-pill);
        background: var(--accent);
      }
      .tl-date {
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-size: 0.6875rem;
        font-variant-numeric: tabular-nums;
        white-space: nowrap;
      }
      .tl-title {
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        font-size: 0.875rem;
        font-weight: 550;
      }

      /* --- Commitments (who owes what) --- */
      .ci-list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .ci {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        padding: var(--space-3);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-md);
        background: var(--surface-input);
      }
      .ci-main {
        display: flex;
        flex-wrap: wrap;
        align-items: baseline;
        gap: var(--space-2);
      }
      .ci-owner {
        flex: none;
        padding: 1px var(--space-2);
        border-radius: var(--radius-pill);
        background: var(--accent-soft);
        color: var(--accent-hover);
        font-size: 0.75rem;
        font-weight: 600;
      }
      .ci-text {
        min-width: 0;
        color: var(--text-primary);
        font-size: 0.875rem;
        line-height: 1.4;
      }
      .ci-meta {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-2);
      }
      .ci-due {
        height: 22px;
        padding: 0 var(--space-2);
        font-size: 0.6875rem;
      }
      .ci-src {
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        color: var(--text-muted);
        font-size: 0.75rem;
        text-decoration: none;
      }
      .ci-src:hover {
        color: var(--accent-hover);
        text-decoration: underline;
      }

      /* --- Facts --- */
      .fact-list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .fact {
        display: flex;
        flex-wrap: wrap;
        align-items: baseline;
        gap: var(--space-2);
        padding: var(--space-2) var(--space-3);
        border-radius: var(--radius-sm);
        background: var(--surface-input);
        font-size: 0.8125rem;
      }
      .fact-changed {
        opacity: 0.7;
      }
      .fact-pred {
        color: var(--text-muted);
        text-transform: uppercase;
        letter-spacing: 0.04em;
        font-size: 0.6875rem;
        font-weight: 600;
      }
      .fact-obj {
        color: var(--text-primary);
        font-weight: 550;
      }
      .fact-changed .fact-obj {
        font-weight: 400;
      }
      .fact-since {
        margin-left: auto;
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-size: 0.6875rem;
        font-variant-numeric: tabular-nums;
        white-space: nowrap;
      }

      /* --- Neighbours --- */
      .nb-list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .nb {
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
      .nb:hover {
        background: var(--surface-hover);
        border-color: var(--border-strong);
      }
      .nb:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .nb-dot {
        flex: none;
        width: 7px;
        height: 7px;
        border-radius: var(--radius-pill);
        background: var(--accent);
      }
      .nb.is-project .nb-dot {
        background: #9d7bff;
      }
      .nb-name {
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        font-weight: 550;
      }
      .nb-count {
        flex: none;
        min-width: 22px;
        height: 22px;
      }

      @media (prefers-reduced-motion: reduce) {
        .dossier {
          animation: none;
        }
      }
    `,
  ],
})
export class PersonDossierComponent {
  private readonly ipc = inject(IpcService);

  /** The person/entity to build the dossier for; changing it re-loads the pane. */
  readonly entityId = input.required<string>();
  /** Emits when the user picks a neighbour — the container re-selects it. */
  readonly select = output<string>();
  /** Emits when the user dismisses the pane. */
  readonly close = output<void>();

  readonly dossier = signal<DossierData | null>(null);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

  /** Currently-valid facts (open — `validTo == null`): the present state. */
  protected readonly currentFacts = computed(() =>
    (this.dossier()?.facts ?? []).filter((f) => f.validTo == null),
  );
  /** Superseded facts (closed — `validTo` set): the history / what changed. */
  protected readonly changedFacts = computed(() =>
    (this.dossier()?.facts ?? []).filter((f) => f.validTo != null),
  );

  /** A compact "3 meetings · 2 open · 4 facts" summary for the header. */
  protected readonly summaryLabel = computed(() => {
    const d = this.dossier();
    if (!d) {
      return "";
    }
    const parts: string[] = [
      d.meetings.length === 1 ? "1 meeting" : `${d.meetings.length} meetings`,
    ];
    if (d.commitments.length) {
      parts.push(`${d.commitments.length} open`);
    }
    const facts = this.currentFacts().length;
    if (facts) {
      parts.push(facts === 1 ? "1 fact" : `${facts} facts`);
    }
    return parts.join(" · ");
  });

  /**
   * Re-load the dossier whenever `entityId` changes. The IPC call is a one-shot
   * promise, so we await it inside an effect that tracks the input signal — a
   * stale-result guard drops responses that resolve after the id moved on (fast
   * neighbour-to-neighbour pivots), so the pane never shows mismatched data.
   * `allowSignalWrites` because loading/error are set synchronously here (NG0600).
   */
  private readonly _load = effect(
    () => {
      const id = this.entityId();
      this.loading.set(true);
      this.error.set(null);
      void this.fetch(id);
    },
    { allowSignalWrites: true },
  );

  private async fetch(id: string): Promise<void> {
    try {
      const result = await this.ipc.getPersonDossier(id);
      // Guard against an out-of-order resolution (the selection moved on).
      if (this.entityId() !== id) {
        return;
      }
      this.dossier.set(result);
    } catch (e) {
      if (this.entityId() !== id) {
        return;
      }
      this.dossier.set(null);
      this.error.set(String(e));
    } finally {
      if (this.entityId() === id) {
        this.loading.set(false);
      }
    }
  }

  /** The trimmed owner name, or null when unattributed (so the chip is skipped). */
  protected ownerLabel(c: Commitment): string | null {
    const o = c.owner?.trim();
    return o ? o : null;
  }

  protected sharedLabel(nb: EntityNeighbor): string {
    return nb.sharedMeetings === 1
      ? "1 shared meeting"
      : `${nb.sharedMeetings} shared meetings`;
  }

  /** Short, locale-aware date for timeline rows + fact validity (year only when it differs). */
  protected fmtDate(iso: string | null): string {
    if (!iso) {
      return "";
    }
    const d = new Date(iso);
    if (isNaN(d.getTime())) {
      // Fall back to the raw date portion (facts/meetings always carry ISO strings).
      return iso.split(/[T ]/)[0] ?? "";
    }
    const now = new Date();
    const opts: Intl.DateTimeFormatOptions =
      d.getFullYear() === now.getFullYear()
        ? { month: "short", day: "numeric" }
        : { month: "short", day: "numeric", year: "numeric" };
    return d.toLocaleDateString(undefined, opts);
  }
}
