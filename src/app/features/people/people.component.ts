import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { IpcService } from "../../core/ipc.service";
import type { PersonCard } from "../../core/models";
import { FoldersService } from "../../services/folders.service";
import { EntityDetailComponent } from "../graph/entity-detail.component";

/**
 * The `/people` page — a personal CRM over the people across your meetings.
 *
 * A directory of {@link PersonCard}s (name + when you last talked + how many open
 * commitments and known facts) is the spine; picking a card opens the SAME
 * self-contained {@link EntityDetailComponent} panel the /graph page uses (its
 * neighborhood + backlinked meetings + connected entities) — no graph is rebuilt
 * here, the detail component is reused verbatim.
 *
 * Lock-awareness (mirrors GraphComponent): `listPeople()` returns ONLY visible
 * people with visible-only counts, so we re-fetch whenever {@link FoldersService}'s
 * tree signal changes (a session unlock/relock or screen-share re-lock shifts
 * visibility) — sealed people drop out, or reappear, live, with no client-side
 * security decision. The IPC call is a one-shot awaited promise written into a
 * signal (never subscribed-into a field).
 */
@Component({
  selector: "app-people",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [EntityDetailComponent],
  template: `
    <section class="people">
      <header class="p-head">
        <div class="p-head-text">
          <h2 class="p-title">People</h2>
          <p class="p-intro">
            Everyone across your meetings — how recently you talked, what they
            still owe, and what Murmur knows about them.
          </p>
        </div>
        @if (!loading() && total() > 0) {
          <span class="count p-total" [attr.title]="total() + ' people'">
            {{ total() }}
          </span>
        }
      </header>

      @if (loading()) {
        <div class="card state-card">
          <p class="empty">Loading…</p>
        </div>
      } @else if (error()) {
        <div class="card empty-state">
          <span class="empty-mark" aria-hidden="true"></span>
          <p class="empty-title">Couldn’t load your people</p>
          <p class="empty">{{ error() }}</p>
        </div>
      } @else if (total() === 0) {
        <div class="card empty-state">
          <span class="empty-mark" aria-hidden="true"></span>
          <p class="empty-title">Your people appear as you record</p>
          <p class="empty">
            As Murmur recognises the people you talk about, they’ll show up here —
            with when you last talked and what they still owe.
          </p>
        </div>
      } @else {
        <div class="p-layout" [class.has-detail]="selectedId() !== null">
          <ul class="p-cards" role="list">
            @for (p of people(); track p.id) {
              <li>
                <button
                  type="button"
                  class="p-card card"
                  [class.is-selected]="selectedId() === p.id"
                  [attr.aria-pressed]="selectedId() === p.id"
                  (click)="onSelect(p.id)"
                >
                  <span class="p-avatar" aria-hidden="true">
                    {{ initial(p.name) }}
                  </span>
                  <span class="p-body">
                    <span class="p-name">{{ p.name }}</span>
                    <span class="p-last">{{ lastTalkedLabel(p) }}</span>
                    <span class="p-meta">
                      <span
                        class="p-chip"
                        [class.is-active]="p.openCommitmentCount > 0"
                        [attr.title]="commitmentTitle(p)"
                      >
                        <svg
                          viewBox="0 0 16 16"
                          width="12"
                          height="12"
                          fill="none"
                          aria-hidden="true"
                        >
                          <rect
                            x="2.5"
                            y="3"
                            width="11"
                            height="10.5"
                            rx="1.6"
                            stroke="currentColor"
                            stroke-width="1.3"
                          />
                          <path
                            d="M5 6.7 6.6 8.3 11 4.9"
                            stroke="currentColor"
                            stroke-width="1.3"
                            stroke-linecap="round"
                            stroke-linejoin="round"
                          />
                        </svg>
                        {{ p.openCommitmentCount }} open
                      </span>
                      <span class="p-chip" [attr.title]="factTitle(p)">
                        <svg
                          viewBox="0 0 16 16"
                          width="12"
                          height="12"
                          fill="none"
                          aria-hidden="true"
                        >
                          <path
                            d="M8 2.5a3.2 3.2 0 0 0-2 5.7c.5.4.8 1 .8 1.6v.4h2.4v-.4c0-.6.3-1.2.8-1.6A3.2 3.2 0 0 0 8 2.5Z"
                            stroke="currentColor"
                            stroke-width="1.2"
                            stroke-linejoin="round"
                          />
                          <path
                            d="M6.6 12.4h2.8"
                            stroke="currentColor"
                            stroke-width="1.2"
                            stroke-linecap="round"
                          />
                        </svg>
                        {{ p.currentFactCount }}
                        {{ p.currentFactCount === 1 ? "fact" : "facts" }}
                      </span>
                    </span>
                  </span>
                </button>
              </li>
            }
          </ul>

          @if (selectedId(); as id) {
            <app-entity-detail
              class="p-detail"
              [entityId]="id"
              (select)="onSelect($event)"
              (close)="clearSelection()"
            />
          }
        </div>
      }
    </section>
  `,
  styles: [
    `
      :host {
        display: block;
      }
      .people {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
      }

      /* --- Head --- */
      .p-head {
        display: flex;
        align-items: flex-start;
        justify-content: space-between;
        gap: var(--space-4);
      }
      .p-head-text {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        min-width: 0;
      }
      .p-title {
        margin: 0;
      }
      .p-intro {
        margin: 0;
        max-width: 60ch;
        color: var(--text-secondary);
        font-size: 0.9375rem;
      }
      .p-total {
        flex: none;
        margin-top: 6px;
      }

      /* --- Two-pane layout (directory | sticky detail) --- */
      .p-layout {
        display: grid;
        grid-template-columns: 1fr;
        gap: var(--space-5);
        align-items: start;
      }
      .p-layout.has-detail {
        grid-template-columns: minmax(0, 1.5fr) minmax(300px, 1fr);
      }
      .p-detail {
        position: sticky;
        top: 84px;
      }

      /* --- Cards --- */
      .p-cards {
        list-style: none;
        margin: 0;
        padding: 0;
        display: grid;
        grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
        gap: var(--space-3);
        min-width: 0;
      }
      .p-card {
        display: flex;
        align-items: flex-start;
        gap: var(--space-3);
        width: 100%;
        padding: var(--space-4);
        text-align: left;
        color: inherit;
        font: inherit;
        cursor: pointer;
        transition:
          border-color var(--transition),
          transform var(--transition-fast);
      }
      .p-card:hover {
        border-color: var(--border-strong);
        transform: translateY(-1px);
      }
      .p-card:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .p-card.is-selected {
        border-color: var(--accent);
        box-shadow: 0 0 0 1px var(--accent);
      }
      .p-avatar {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        flex: none;
        width: 38px;
        height: 38px;
        border-radius: var(--radius-pill);
        background: var(--accent-soft);
        border: 1px solid var(--accent-ring);
        color: var(--accent-hover);
        font-weight: 600;
        text-transform: uppercase;
      }
      .p-body {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        min-width: 0;
      }
      .p-name {
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        color: var(--text-primary);
        font-size: 0.95rem;
        font-weight: 600;
      }
      .p-last {
        color: var(--text-muted);
        font-size: 0.8125rem;
      }
      .p-meta {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-2);
        margin-top: var(--space-1);
      }
      .p-chip {
        display: inline-flex;
        align-items: center;
        gap: 4px;
        padding: 2px var(--space-2);
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
        color: var(--text-secondary);
        font-size: 0.75rem;
        font-weight: 550;
        font-variant-numeric: tabular-nums;
      }
      .p-chip.is-active {
        color: var(--accent-hover);
        background: var(--accent-soft);
        border-color: var(--accent-ring);
      }

      /* --- Responsive: one column, detach the sticky panel. --- */
      @media (max-width: 760px) {
        .p-layout.has-detail {
          grid-template-columns: 1fr;
        }
        .p-detail {
          position: static;
        }
      }
      @media (prefers-reduced-motion: reduce) {
        .p-card {
          transition: none;
        }
      }
    `,
  ],
})
export class PeopleComponent {
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);

  readonly people = signal<PersonCard[]>([]);
  readonly loading = signal(true);
  readonly error = signal<string | null>(null);

  /** The selected person id — opens the reused entity-detail panel. */
  readonly selectedId = signal<string | null>(null);

  protected readonly total = computed(() => this.people().length);

  /**
   * Load the people list, and re-load whenever the folder lock-state changes.
   * Reading the folders `tree` signal registers this effect as its dependent, so
   * its initial value drives the first fetch (no separate `ngOnInit`), and a
   * later session unlock/relock — or a screen-share-triggered relock-all —
   * re-runs the fetch so sealed people drop out, or reappear, live (mirrors
   * GraphComponent). `fetch()` writes loading/error/data synchronously before its
   * first await, so this tracked effect must be allowed to write (NG0600 guard).
   */
  private readonly _refetchOnLock = effect(
    () => {
      this.folders.tree();
      void this.fetch();
    },
    { allowSignalWrites: true },
  );

  private async fetch(): Promise<void> {
    this.error.set(null);
    try {
      const rows = await this.ipc.listPeople();
      this.people.set(rows);
      // If the selected person is no longer visible (e.g. their folder re-sealed),
      // close the detail panel so we never point at a vanished person.
      const sel = this.selectedId();
      if (sel && !rows.some((p) => p.id === sel)) {
        this.selectedId.set(null);
      }
    } catch (e) {
      this.people.set([]);
      this.error.set(String(e));
    } finally {
      this.loading.set(false);
    }
  }

  /** Open (or toggle closed) the detail panel for a person. */
  onSelect(id: string): void {
    this.selectedId.update((cur) => (cur === id ? null : id));
  }

  clearSelection(): void {
    this.selectedId.set(null);
  }

  /** The uppercase leading letter for the avatar (fallback "?" for empty names). */
  protected initial(name: string): string {
    const c = name.trim().charAt(0);
    return c ? c.toUpperCase() : "?";
  }

  /** Human "last talked" label: Today / Yesterday / N days ago / a short date. */
  protected lastTalkedLabel(p: PersonCard): string {
    const iso = p.lastTalked;
    if (!iso) {
      return "No recent meetings";
    }
    const d = new Date(iso);
    if (isNaN(d.getTime())) {
      return "";
    }
    const now = new Date();
    const startOfDay = (x: Date): number =>
      new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
    const days = Math.round(
      (startOfDay(now) - startOfDay(d)) / 86_400_000,
    );
    if (days <= 0) {
      return "Talked today";
    }
    if (days === 1) {
      return "Talked yesterday";
    }
    if (days < 7) {
      return `Talked ${days} days ago`;
    }
    const opts: Intl.DateTimeFormatOptions =
      d.getFullYear() === now.getFullYear()
        ? { month: "short", day: "numeric" }
        : { month: "short", day: "numeric", year: "numeric" };
    return `Last talked ${d.toLocaleDateString(undefined, opts)}`;
  }

  protected commitmentTitle(p: PersonCard): string {
    return p.openCommitmentCount === 1
      ? "1 open commitment"
      : `${p.openCommitmentCount} open commitments`;
  }

  protected factTitle(p: PersonCard): string {
    return p.currentFactCount === 1
      ? "1 known fact"
      : `${p.currentFactCount} known facts`;
  }
}
