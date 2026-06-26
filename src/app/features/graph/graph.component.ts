import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { IpcService } from "../../core/ipc.service";
import type { GraphData, GraphNode } from "../../core/models";
import { FoldersService } from "../../services/folders.service";
import { EntityCardComponent } from "./entity-card.component";
import { EntityDetailComponent } from "./entity-detail.component";

type KindFilter = "all" | "person" | "project";
type SortKey = "mentions" | "name";

/** A directory section (People or Projects) with its filtered/sorted rows. */
interface DirectorySection {
  kind: "person" | "project";
  label: string;
  entities: GraphNode[];
}

/**
 * The /graph page — the self-assembling People/Projects graph.
 *
 * A structured directory is the load-bearing spine: every VISIBLE entity as a
 * sortable, searchable, filterable card with its visible mention count. A
 * single-entity neighborhood SVG (in the detail panel) is additive decoration.
 *
 * Lock-awareness: the backend returns ONLY visible entities + a `hasHidden`
 * flag; we render exactly that, plus one honest disclosure banner. We re-fetch
 * `getGraph()` whenever {@link FoldersService}'s tree signal changes (a session
 * unlock/relock or screen-share re-lock shifts visibility), so sealed entities
 * drop out — or reappear — live, with no stale view and no client-side security
 * decision.
 */
@Component({
  selector: "app-graph",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [EntityCardComponent, EntityDetailComponent],
  template: `
    <section class="graph">
      <header class="g-head">
        <div class="g-head-text">
          <h2 class="g-title">Graph</h2>
          <p class="g-intro">
            The people and projects across your meetings — your graph builds
            itself as you record.
          </p>
        </div>
        @if (!loading() && total() > 0) {
          <span class="count g-total" [attr.title]="total() + ' entities'">
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
          <p class="empty-title">Couldn’t load your graph</p>
          <p class="empty">{{ error() }}</p>
        </div>
      } @else if (total() === 0) {
        <div class="card empty-state">
          <span class="empty-mark" aria-hidden="true"></span>
          <p class="empty-title">
            Your graph builds itself as you record meetings
          </p>
          <p class="empty">
            As Murmur recognises the people and projects you talk about, they’ll
            appear here — connected by the meetings they share.
          </p>
        </div>
      } @else {
        <!-- One honest disclosure: sealed folders are withheld from the graph. -->
        @if (hasHidden()) {
          <div class="banner is-accent g-banner" role="status">
            <span class="g-banner-glyph" aria-hidden="true"></span>
            <span>
              Some entities are hidden — unlock a folder to include them.
            </span>
          </div>
        }

        <div class="g-layout" [class.has-detail]="selectedId() !== null">
          <!-- Directory (spine) -->
          <div class="g-directory">
            <div class="g-controls">
              <div class="g-search">
                <svg
                  class="g-search-icon"
                  viewBox="0 0 16 16"
                  width="15"
                  height="15"
                  aria-hidden="true"
                >
                  <circle
                    cx="7"
                    cy="7"
                    r="4.5"
                    stroke="currentColor"
                    stroke-width="1.5"
                    fill="none"
                  />
                  <path
                    d="M10.5 10.5L14 14"
                    stroke="currentColor"
                    stroke-width="1.5"
                    stroke-linecap="round"
                  />
                </svg>
                <input
                  type="search"
                  class="g-search-input"
                  placeholder="Search people and projects…"
                  aria-label="Search entities"
                  [value]="query()"
                  (input)="onQuery($event)"
                />
              </div>

              <div class="g-filter" role="group" aria-label="Filter by kind">
                @for (f of kindFilters; track f.key) {
                  <button
                    type="button"
                    class="g-seg"
                    [class.is-active]="kindFilter() === f.key"
                    [attr.aria-pressed]="kindFilter() === f.key"
                    (click)="kindFilter.set(f.key)"
                  >
                    {{ f.label }}
                  </button>
                }
              </div>

              <label class="g-sort">
                <span class="g-sort-label">Sort</span>
                <select
                  class="g-sort-select"
                  aria-label="Sort entities"
                  [value]="sort()"
                  (change)="onSort($event)"
                >
                  <option value="mentions">Most mentioned</option>
                  <option value="name">Name</option>
                </select>
              </label>
            </div>

            @if (visibleTotal() === 0) {
              <div class="card g-no-results">
                <p class="empty-title">No matches</p>
                <p class="empty">
                  Nothing matches your search and filters. Try clearing them.
                </p>
              </div>
            } @else {
              @for (section of sections(); track section.kind) {
                @if (section.entities.length) {
                  <div class="g-section">
                    <div class="g-section-head">
                      <span
                        class="g-section-dot"
                        [class.is-project]="section.kind === 'project'"
                        aria-hidden="true"
                      ></span>
                      <h3 class="g-section-title">{{ section.label }}</h3>
                      <span class="count g-section-count">
                        {{ section.entities.length }}
                      </span>
                    </div>
                    <div class="g-cards" role="list">
                      @for (e of section.entities; track e.id) {
                        <app-entity-card
                          role="listitem"
                          [style.--i]="$index"
                          class="g-card"
                          [entity]="e"
                          [selected]="selectedId() === e.id"
                          (select)="onSelect($event)"
                        />
                      }
                    </div>
                  </div>
                }
              }
            }
          </div>

          <!-- Detail panel (sticky on wide viewports) -->
          @if (selectedId(); as id) {
            <app-entity-detail
              class="g-detail"
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
      .graph {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
      }

      /* --- Head --- */
      .g-head {
        display: flex;
        align-items: flex-start;
        justify-content: space-between;
        gap: var(--space-4);
      }
      .g-head-text {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        min-width: 0;
      }
      .g-title {
        margin: 0;
      }
      .g-intro {
        margin: 0;
        max-width: 60ch;
        color: var(--text-secondary);
        font-size: 0.9375rem;
      }
      .g-total {
        flex: none;
        margin-top: 6px;
      }

      .g-banner {
        align-items: center;
        animation: rise 320ms var(--transition) both;
      }
      .g-banner-glyph {
        flex: none;
        width: 8px;
        height: 8px;
        border-radius: var(--radius-pill);
        background: var(--accent-hover);
        box-shadow: 0 0 0 4px var(--accent-soft);
      }

      /* --- Two-pane layout --- */
      .g-layout {
        display: grid;
        grid-template-columns: 1fr;
        gap: var(--space-5);
        align-items: start;
      }
      .g-layout.has-detail {
        grid-template-columns: minmax(0, 1.5fr) minmax(300px, 1fr);
      }
      .g-detail {
        position: sticky;
        top: 84px;
      }

      /* --- Controls --- */
      .g-directory {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
        min-width: 0;
      }
      .g-controls {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-3);
      }
      .g-search {
        position: relative;
        flex: 1 1 220px;
        min-width: 0;
      }
      .g-search-icon {
        position: absolute;
        left: var(--space-3);
        top: 50%;
        transform: translateY(-50%);
        color: var(--text-muted);
        pointer-events: none;
      }
      .g-search-input {
        padding-left: var(--space-6);
      }
      .g-filter {
        display: inline-flex;
        padding: 3px;
        gap: 2px;
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-pill);
        background: var(--surface-input);
      }
      .g-seg {
        padding: 0 var(--space-3);
        height: 30px;
        border: none;
        border-radius: var(--radius-pill);
        background: transparent;
        color: var(--text-secondary);
        font-family: inherit;
        font-size: 0.8125rem;
        font-weight: 600;
        cursor: pointer;
        transition:
          background var(--transition),
          color var(--transition);
      }
      .g-seg:hover {
        color: var(--text-primary);
      }
      .g-seg:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .g-seg.is-active {
        background: var(--accent-soft);
        color: var(--accent-hover);
      }
      .g-sort {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
      }
      .g-sort-label {
        color: var(--text-muted);
        font-size: 0.75rem;
        font-weight: 600;
        letter-spacing: 0.04em;
        text-transform: uppercase;
      }
      .g-sort-select {
        width: auto;
        height: 36px;
      }

      /* --- Sections + cards --- */
      .g-section {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .g-section-head {
        display: flex;
        align-items: center;
        gap: var(--space-2);
      }
      .g-section-dot {
        flex: none;
        width: 9px;
        height: 9px;
        border-radius: var(--radius-pill);
        background: var(--accent);
        box-shadow: 0 0 0 3px var(--accent-soft);
      }
      .g-section-dot.is-project {
        background: #9d7bff;
        box-shadow: 0 0 0 3px rgba(157, 123, 255, 0.18);
      }
      .g-section-title {
        margin: 0;
        font-size: 1rem;
      }
      .g-section-count {
        margin-left: auto;
      }
      .g-cards {
        display: grid;
        grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
        gap: var(--space-3);
      }
      .g-card {
        display: block;
        animation: rise 320ms var(--transition) both;
        animation-delay: calc(min(var(--i, 0), 12) * 24ms);
      }
      .g-no-results {
        padding: var(--space-6);
        text-align: center;
      }
      .g-no-results .empty-title {
        margin: 0 0 var(--space-1);
      }
      .g-no-results .empty {
        margin: 0;
      }

      /* --- Responsive: collapse to one column, detach the sticky panel. --- */
      @media (max-width: 760px) {
        .g-layout.has-detail {
          grid-template-columns: 1fr;
        }
        .g-detail {
          position: static;
        }
      }
      @media (prefers-reduced-motion: reduce) {
        .g-card,
        .g-banner {
          animation: none;
        }
      }
    `,
  ],
})
export class GraphComponent {
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);

  readonly graphData = signal<GraphData | null>(null);
  readonly loading = signal(true);
  readonly error = signal<string | null>(null);

  readonly kindFilter = signal<KindFilter>("all");
  readonly sort = signal<SortKey>("mentions");
  readonly query = signal("");
  readonly selectedId = signal<string | null>(null);

  protected readonly kindFilters: readonly {
    key: KindFilter;
    label: string;
  }[] = [
    { key: "all", label: "All" },
    { key: "person", label: "People" },
    { key: "project", label: "Projects" },
  ];

  /** Total VISIBLE entities returned by the backend (before any local filter). */
  protected readonly total = computed(
    () => this.graphData()?.nodes.length ?? 0,
  );
  /** Whether the backend withheld ≥1 sealed entity → show the disclosure. */
  protected readonly hasHidden = computed(
    () => this.graphData()?.hasHidden ?? false,
  );

  /** The filtered + searched + sorted nodes (the view-model for the directory). */
  protected readonly visibleEntities = computed<GraphNode[]>(() => {
    const nodes = this.graphData()?.nodes ?? [];
    const kind = this.kindFilter();
    const q = this.query().trim().toLowerCase();
    const sort = this.sort();

    const filtered = nodes.filter((n) => {
      if (kind !== "all" && n.kind !== kind) {
        return false;
      }
      if (q && !n.name.toLowerCase().includes(q)) {
        return false;
      }
      return true;
    });

    return [...filtered].sort((a, b) => {
      if (sort === "name") {
        return a.name.localeCompare(b.name);
      }
      // Most-mentioned first, name as a stable tiebreak.
      return b.mentionCount - a.mentionCount || a.name.localeCompare(b.name);
    });
  });

  protected readonly visibleTotal = computed(
    () => this.visibleEntities().length,
  );

  /** Group the view-model into People / Projects sections (in display order). */
  protected readonly sections = computed<DirectorySection[]>(() => {
    const all = this.visibleEntities();
    return [
      {
        kind: "person" as const,
        label: "People",
        entities: all.filter((e) => e.kind === "person"),
      },
      {
        kind: "project" as const,
        label: "Projects",
        entities: all.filter((e) => e.kind === "project"),
      },
    ];
  });

  /**
   * Load the graph, and re-load it whenever the folder lock-state changes.
   * Reading the folders `tree` signal registers this effect as its dependent,
   * so its initial value drives the first fetch (no separate `ngOnInit`), and a
   * later session unlock/relock — or a screen-share-triggered relock-all —
   * re-runs the fetch so sealed entities drop out, or reappear, live. The
   * backend is the sole authority on visibility; we just re-ask and re-render.
   */
  private readonly _refetchOnLock = effect(
    () => {
      // Establish the dependency; the value itself isn't needed here.
      this.folders.tree();
      void this.fetchGraph();
    },
    // fetchGraph() writes the loading/error/data signals (synchronously before
    // its first await), so this tracked effect must be allowed to write.
    { allowSignalWrites: true },
  );

  private async fetchGraph(): Promise<void> {
    this.error.set(null);
    try {
      const data = await this.ipc.getGraph();
      this.graphData.set(data);
      // If the selected entity is no longer visible (e.g. its folder re-sealed),
      // close the detail panel so we never point at a vanished node.
      const sel = this.selectedId();
      if (sel && !data.nodes.some((n) => n.id === sel)) {
        this.selectedId.set(null);
      }
    } catch (e) {
      this.graphData.set(null);
      this.error.set(String(e));
    } finally {
      this.loading.set(false);
    }
  }

  onQuery(event: Event): void {
    this.query.set((event.target as HTMLInputElement).value);
  }

  onSort(event: Event): void {
    this.sort.set((event.target as HTMLSelectElement).value as SortKey);
  }

  /** Open (or toggle closed) the detail panel for an entity. */
  onSelect(id: string): void {
    this.selectedId.update((cur) => (cur === id ? null : id));
  }

  clearSelection(): void {
    this.selectedId.set(null);
  }
}
