import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { ActivatedRoute, Router } from "@angular/router";
import { map } from "rxjs";

import type { ContainerNode, ItemKind, ItemRow } from "../../../core/models";
import { WorkspaceService } from "../workspace.service";
import { containerNoun } from "../../../core/hierarchy-vocabulary";

/** The kinds a container can hold, in render order. */
const KINDS: readonly ItemKind[] = ["meeting", "note", "task", "dashboard"];

const KIND_LABEL: Record<ItemKind, string> = {
  meeting: "Meetings",
  note: "Notes",
  task: "Tasks",
  dashboard: "Dashboards",
};

/** Where opening an item goes. These MUST match `app.routes.ts`. */
const KIND_ROUTE: Record<ItemKind, string> = {
  meeting: "/meeting",
  note: "/notes",
  task: "/tasks",
  dashboard: "/dashboards",
};

const PAGE = 25;

/** One kind's loaded page inside this container. */
interface KindPage {
  kind: ItemKind;
  items: ItemRow[];
  total: number;
}

/**
 * Everything one container holds, paged per kind — where the sidebar's
 * "See all" lands.
 *
 * The sidebar shows the first few items of each kind; this is the rest. Without
 * it the tree's own navigation had nowhere to go: `/container/:id` fell through
 * the router's catch-all to `/record`, so clicking a project silently opened the
 * recorder instead of failing visibly.
 */
@Component({
  selector: "app-container-view",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./container-view.component.html",
  styleUrl: "./container-view.component.scss",
})
export class ContainerViewComponent {
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);
  private readonly workspace = inject(WorkspaceService);

  private readonly _container = signal<ContainerNode | null>(null);

  /**
   * The word the user reads for this thing.
   *
   * These states used to say "container" — the CODE's word for "either a Workspace or a folder",
   * which no other surface shows and which names nothing the user ever created. Falls back to
   * "folder" only before the node has loaded, where the sentence has to say something and the
   * narrower word is the safer guess.
   */
  protected readonly noun = computed(() => {
    const c = this._container();
    return c ? containerNoun(c) : "folder";
  });
  private readonly _pages = signal<KindPage[]>([]);
  private readonly _loading = signal(false);
  private readonly _error = signal<string | null>(null);

  protected readonly container = this._container.asReadonly();
  protected readonly pages = this._pages.asReadonly();
  protected readonly loading = this._loading.asReadonly();
  protected readonly error = this._error.asReadonly();

  /**
   * The container id from the route, as a SIGNAL — never a subscription writing a
   * field. `toSignal` hands the subscription's lifecycle to the framework, which is
   * what the zoneless rules require and what `TaskViewComponent` already does.
   */
  private readonly containerId = toSignal(
    this.route.paramMap.pipe(map((params) => params.get("id"))),
    { initialValue: this.route.snapshot.paramMap.get("id") },
  );

  /** A sealed, not-session-unlocked container refuses to describe its contents. */
  protected readonly sealed = computed(() => {
    const container = this._container();
    return !!container && container.locked && !container.unlocked;
  });

  protected readonly isEmpty = computed(
    () => this._pages().every((page) => page.total === 0),
  );

  constructor() {
    // Re-fetch whenever the route's container changes, dropping any response that
    // arrives after the user has already moved on.
    effect(() => {
      const id = this.containerId();
      if (!id) {
        return;
      }
      this._loading.set(true);
      void this.load(id);
    });
  }

  private async load(id: string): Promise<void> {
    try {
      const container = await this.workspace.getContainer(id);
      if (this.containerId() !== id) {
        return;
      }
      this._container.set(container);
      this._error.set(null);

      if (!container || (container.locked && !container.unlocked)) {
        // A sealed container is REFUSED by the item reader, not answered with an
        // empty page — asking would be an error, and an error here would read as
        // a failure rather than the deliberate refusal it is.
        this._pages.set([]);
        return;
      }

      const pages = await Promise.all(
        KINDS.map(async (kind) => {
          const page = await this.workspace.listItems(id, kind, 0, PAGE);
          return { kind, items: page.items, total: page.total };
        }),
      );
      if (this.containerId() !== id) {
        return;
      }
      this._pages.set(pages);
    } catch (error) {
      if (this.containerId() === id) {
        this._error.set(messageOf(error));
      }
    } finally {
      if (this.containerId() === id) {
        this._loading.set(false);
      }
    }
  }

  protected kindLabel(kind: ItemKind): string {
    return KIND_LABEL[kind];
  }

  protected itemTitle(item: ItemRow): string {
    const title = item.title?.trim();
    return title ? title : "Untitled";
  }

  /** `null` for anything that is not a meeting, so the template renders nothing. */
  protected duration(item: ItemRow): string | null {
    if (item.durationS === null || item.durationS <= 0) {
      return null;
    }
    const minutes = Math.round(item.durationS / 60);
    return minutes < 60
      ? `${minutes} min`
      : `${Math.floor(minutes / 60)} h ${minutes % 60} min`;
  }

  protected openItem(item: ItemRow): void {
    void this.router.navigate([KIND_ROUTE[item.kind], item.id]);
  }

  /** Load the next page of one kind, appending to what is already shown. */
  protected async loadMore(page: KindPage): Promise<void> {
    const id = this.containerId();
    if (!id) {
      return;
    }
    const next = await this.workspace.listItems(id, page.kind, page.items.length, PAGE);
    this._pages.update((pages) =>
      pages.map((current) =>
        current.kind === page.kind
          ? { ...current, items: [...current.items, ...next.items], total: next.total }
          : current,
      ),
    );
  }
}

function messageOf(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }
  if (error && typeof error === "object" && "message" in error) {
    return String((error as { message: unknown }).message);
  }
  return "Could not load the contents";
}
