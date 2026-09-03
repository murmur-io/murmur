import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { ActivatedRoute } from "@angular/router";
import { map } from "rxjs";
import { IpcService } from "../../../core/ipc.service";
import type { GraphData, GraphNode } from "../../../core/models";
import { FoldersService } from "../../../services/folders.service";
import { EntityCardComponent } from "../entity-card/entity-card.component";
import { EntityDetailComponent } from "../entity-detail/entity-detail.component";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";

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
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [EntityCardComponent, EntityDetailComponent],
  templateUrl: "./graph.component.html",
  styleUrl: "./graph.component.scss",
})
export class GraphComponent {
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);
  private readonly route = inject(ActivatedRoute);
  private readonly errorCopy = inject(ErrorCopyService);

  /**
   * An optional `?entity=<id>` query param — the entry point the full-brain
   * graph uses for its entity click-through ("reuse existing nav"). Read as a
   * signal; an effect preselects it once the graph resolves (a `computed`
   * `selectedId` can't be user-toggled, so it seeds a writable signal instead).
   */
  private readonly entityParam = toSignal(
    this.route.queryParamMap.pipe(map((p) => p.get("entity"))),
    { initialValue: null },
  );

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

  /** VISIBLE entities actually rendered (may be capped — see {@link capDisclosure}). */
  protected readonly total = computed(
    () => this.graphData()?.nodes.length ?? 0,
  );
  /** Whether the backend withheld ≥1 sealed entity → show the disclosure. */
  protected readonly hasHidden = computed(
    () => this.graphData()?.hasHidden ?? false,
  );

  /**
   * Whether the backend's 500-row render cap trimmed the roster (independent of
   * {@link hasHidden}, which only reflects LOCKED folders — a vault can have >500
   * visible entities and zero locked folders, in which case `hasHidden` stays false
   * while the cap still silently dropped rows without this check).
   */
  protected readonly isCapped = computed(() => {
    const d = this.graphData();
    return !!d && d.totalVisibleEntities > d.nodes.length;
  });

  /** One honest caption disclosing the render cap, mirroring `brain.component.ts`'s pattern. */
  protected readonly capDisclosure = computed<string | null>(() => {
    const d = this.graphData();
    if (!d || !this.isCapped()) {
      return null;
    }
    return `Showing the ${d.nodes.length} most-mentioned of ${d.totalVisibleEntities} entities.`;
  });

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
  );

  /**
   * Preselect the `?entity=<id>` deep-link once its node is visible in the
   * loaded graph (the full-brain graph navigates here for entity click-through).
   * Only applies while nothing is selected yet, so it seeds the initial view
   * without stomping a later user selection; a missing/sealed entity is ignored.
   */
  private readonly _applyEntityParam = effect(() => {
    const wanted = this.entityParam();
    const data = this.graphData();
    if (!wanted || !data) {
      return;
    }
    if (this.selectedId() === null && data.nodes.some((n) => n.id === wanted)) {
      this.selectedId.set(wanted);
    }
  });

  /**
   * Monotonic fetch id, so an out-of-order response cannot overwrite a newer one.
   *
   * `_refetchOnLock` re-runs on every folders-tree change — a session unlock, a relock, a
   * screen-share-triggered relock-all — so two fetches can be in flight at once, and nothing
   * guarantees they resolve in the order they started. Without this guard the LATER-started fetch
   * could land first and be overwritten by the older one, which for a visibility refetch is not a
   * cosmetic race: it can put sealed entities back on screen after a relock, from a response that was
   * already stale when it arrived. `entity-detail.component.ts` keys the same discipline on the
   * entity id; there is no such identity here, so the sequence number is the identity.
   */
  private fetchSeq = 0;

  private async fetchGraph(): Promise<void> {
    const seq = ++this.fetchSeq;
    this.error.set(null);
    try {
      const data = await this.ipc.getGraph();
      if (seq !== this.fetchSeq) return;
      this.graphData.set(data);
      // If the selected entity is no longer visible (e.g. its folder re-sealed),
      // close the detail panel so we never point at a vanished node.
      const sel = this.selectedId();
      if (sel && !data.nodes.some((n) => n.id === sel)) {
        this.selectedId.set(null);
      }
    } catch (e) {
      if (seq !== this.fetchSeq) return;
      this.graphData.set(null);
      this.error.set(this.errorCopy.humanize(e));
    } finally {
      // Only the newest fetch may clear the spinner; an older one finishing later must not
      // announce "done" while the current request is still running.
      if (seq === this.fetchSeq) this.loading.set(false);
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
