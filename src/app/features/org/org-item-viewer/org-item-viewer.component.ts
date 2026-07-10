import {
  ChangeDetectionStrategy,
  Component,
  Injector,
  effect,
  inject,
  signal,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { ActivatedRoute } from "@angular/router";
import { map } from "rxjs";
import { IpcService } from "../../../core/ipc.service";
import { NavHistoryService } from "../../../core/nav-history.service";
import type { OrgItemDetail } from "../../../core/models";
import { MarkdownComponent } from "../../../shared/markdown/markdown.component";

/**
 * Read-only viewer for ONE org-brain item (`/org-item/:id`). Reached from an
 * org-origin source chip in the Ask `SourcesComponent`. Renders the decrypted
 * `OrgItemDetail.markdown` with an author + date header. Org items are
 * deliberately-disclosed org content (no lock gate applies), so this is a plain
 * read-only render — there is no edit/share affordance here.
 *
 * The route param drives the load via an IPC-on-signal-change effect (T1) with a
 * stale-result guard, so navigating between org items in place re-fetches
 * correctly. The back-affordance uses {@link NavHistoryService} to return to
 * wherever the user came from (Ask, most commonly).
 */
@Component({
  selector: "app-org-item-viewer",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MarkdownComponent],
  templateUrl: "./org-item-viewer.component.html",
  styleUrl: "./org-item-viewer.component.scss",
})
export class OrgItemViewerComponent {
  private readonly ipc = inject(IpcService);
  private readonly route = inject(ActivatedRoute);
  private readonly injector = inject(Injector);
  readonly nav = inject(NavHistoryService);

  /** The `:id` route param as a signal (re-fetches when it changes in place). */
  private readonly itemId = toSignal(
    this.route.paramMap.pipe(map((p) => p.get("id"))),
    { initialValue: this.route.snapshot.paramMap.get("id") },
  );

  private readonly _item = signal<OrgItemDetail | null>(null);
  readonly item = this._item.asReadonly();
  readonly loading = signal(true);
  readonly error = signal<string | null>(null);

  constructor() {
    // Fetch the org item whenever the route id changes. Async IPC effect (T1) —
    // writes loading/error/item, stale-guarded on the captured id.
    effect(
      () => {
        const id = this.itemId();
        void this.load(id);
      },
      { injector: this.injector },
    );
  }

  private async load(id: string | null): Promise<void> {
    if (!id) {
      this.loading.set(false);
      this.error.set("No item id.");
      return;
    }
    this.loading.set(true);
    this.error.set(null);
    try {
      const item = await this.ipc.orgGetItem(id);
      if (this.itemId() !== id) {
        return; // stale — the route moved on under us
      }
      this._item.set(item);
    } catch (e) {
      if (this.itemId() !== id) {
        return;
      }
      this.error.set(String(e));
      this._item.set(null);
    } finally {
      if (this.itemId() === id) {
        this.loading.set(false);
      }
    }
  }

  /** Presentational: an ISO timestamp → a friendly local date. */
  formatDate(iso: string): string {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) {
      return iso;
    }
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  }
}
