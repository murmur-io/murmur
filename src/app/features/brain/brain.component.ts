import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { IpcService } from "../../core/ipc.service";
import type { GraphData } from "../../core/models";
import { FoldersService } from "../../services/folders.service";
import { BrainMapComponent } from "./brain-map.component";
import { DocumentPanelComponent } from "./document-panel.component";

/** The hard cap the map applies — kept in sync with BrainMapComponent's MAX_NODES. */
const MAP_NODE_CAP = 60;

/**
 * The `/brain` page — the "whole brain" view.
 *
 * Two halves over one local-first store:
 *  1. DOCUMENTS — upload `.md`/`.txt` files into a folder to EXPAND the brain,
 *     list + delete them (delegated to {@link DocumentPanelComponent}).
 *  2. BRAIN MAP — the entity co-occurrence graph from `get_graph()`, rendered as
 *     an interactive node-link SVG (delegated to {@link BrainMapComponent}).
 *
 * Like the /graph page, the map is lock-aware: the backend returns only VISIBLE
 * entities + a `hasHidden` flag, and we re-fetch `getGraph()` whenever the
 * {@link FoldersService} tree changes (a session unlock/relock / screen-share
 * relock shifts visibility), so sealed entities drop out — or reappear — live.
 *
 * SCOPE NOTE (v1): the map is ENTITY-focused. Imported documents expand the
 * brain's retrieval corpus (and its entity extraction as notes reference them),
 * but the backend's `get_graph()` exposes only person/project entity nodes — it
 * has no per-document node kind — so documents are NOT yet a distinct node kind
 * on the map. Adding a document node kind would need a backend graph change
 * (out of scope for this FE-only task).
 */
@Component({
  selector: "app-brain",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [DocumentPanelComponent, BrainMapComponent],
  template: `
    <section class="brain">
      <header class="b-head">
        <div class="b-head-text">
          <h2 class="b-title">Brain</h2>
          <p class="b-intro">
            Everything Murmur knows — the documents you’ve added and the people
            and projects it has connected across your meetings.
          </p>
        </div>
      </header>

      <!-- 1 — Documents: expand the brain. -->
      <app-document-panel />

      <!-- 2 — The brain map. -->
      <section class="b-map-wrap">
        <header class="b-map-head">
          <div class="b-map-head-text">
            <h3 class="b-map-title">Map</h3>
            <p class="b-map-sub">
              People and projects, linked by the meetings they share.
            </p>
          </div>
          @if (!loading() && nodeCount() > 0) {
            <span class="count" [attr.title]="nodeCount() + ' entities'">
              {{ nodeCount() }}
            </span>
          }
        </header>

        @if (loading()) {
          <div class="card state-card">
            <p class="empty">Loading the map…</p>
          </div>
        } @else if (error()) {
          <div class="card empty-state">
            <span class="empty-mark" aria-hidden="true"></span>
            <p class="empty-title">Couldn’t load the map</p>
            <p class="empty">{{ error() }}</p>
          </div>
        } @else if (nodeCount() === 0) {
          <div class="card empty-state">
            <span class="empty-mark" aria-hidden="true"></span>
            <p class="empty-title">The map builds itself as you record</p>
            <p class="empty">
              As Murmur recognises the people and projects you talk about,
              they’ll appear here — connected by the meetings they share.
            </p>
          </div>
        } @else {
          @if (disclosure(); as msg) {
            <div class="banner is-accent b-banner" role="status">
              <span class="b-banner-glyph" aria-hidden="true"></span>
              <span>{{ msg }}</span>
            </div>
          }
          <app-brain-map [data]="graphData()" />
        }
      </section>
    </section>
  `,
  styles: [
    `
      :host {
        display: block;
      }
      .brain {
        display: flex;
        flex-direction: column;
        gap: var(--space-6);
      }

      .b-head-text {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .b-title {
        margin: 0;
      }
      .b-intro {
        margin: 0;
        max-width: 64ch;
        color: var(--text-secondary);
        font-size: 0.9375rem;
      }

      .b-map-wrap {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .b-map-head {
        display: flex;
        align-items: flex-start;
        justify-content: space-between;
        gap: var(--space-4);
      }
      .b-map-head-text {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        min-width: 0;
      }
      .b-map-title {
        margin: 0;
        font-size: 1.0625rem;
      }
      .b-map-sub {
        margin: 0;
        color: var(--text-secondary);
        font-size: 0.875rem;
      }

      .b-banner {
        align-items: center;
        animation: rise 320ms var(--transition) both;
      }
      .b-banner-glyph {
        flex: none;
        width: 8px;
        height: 8px;
        border-radius: var(--radius-pill);
        background: var(--accent-hover);
        box-shadow: 0 0 0 4px var(--accent-soft);
      }
      @media (prefers-reduced-motion: reduce) {
        .b-banner {
          animation: none;
        }
      }
    `,
  ],
})
export class BrainComponent {
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);

  readonly graphData = signal<GraphData | null>(null);
  readonly loading = signal(true);
  readonly error = signal<string | null>(null);

  /** VISIBLE entity count from the backend (before the map's display cap). */
  protected readonly nodeCount = computed(
    () => this.graphData()?.nodes.length ?? 0,
  );

  /**
   * One honest disclosure line, or null. Combines TWO truths:
   *  - `hasHidden` — the backend withheld ≥1 sealed entity (unlock to include).
   *  - the map's display cap — when more than {@link MAP_NODE_CAP} entities are
   *    visible, the map shows only the strongest top-K (the rest are still in
   *    Documents/search; just not drawn).
   */
  protected readonly disclosure = computed<string | null>(() => {
    const d = this.graphData();
    if (!d) {
      return null;
    }
    const capped = d.nodes.length > MAP_NODE_CAP;
    if (d.hasHidden && capped) {
      return `Showing the ${MAP_NODE_CAP} most-connected entities. More are hidden in locked folders — unlock to include them.`;
    }
    if (capped) {
      return `Showing the ${MAP_NODE_CAP} most-connected of ${d.nodes.length} entities.`;
    }
    if (d.hasHidden) {
      return "Some entities are hidden — unlock a folder to include them.";
    }
    return null;
  });

  /**
   * Load the graph, and re-load it whenever the folder lock-state changes.
   * Mirrors GraphComponent: reading the folders `tree` signal registers this
   * effect as its dependent, so the initial value drives the first fetch and a
   * later unlock/relock re-runs it (sealed entities drop out / reappear live).
   * `fetchGraph` writes loading/error/data synchronously before its first await,
   * so this tracked effect must be allowed to write (NG0600 guard).
   */
  private readonly _refetchOnLock = effect(
    () => {
      this.folders.tree();
      void this.fetchGraph();
    },
    { allowSignalWrites: true },
  );

  private async fetchGraph(): Promise<void> {
    this.error.set(null);
    try {
      this.graphData.set(await this.ipc.getGraph());
    } catch (e) {
      this.graphData.set(null);
      this.error.set(String(e));
    } finally {
      this.loading.set(false);
    }
  }
}
