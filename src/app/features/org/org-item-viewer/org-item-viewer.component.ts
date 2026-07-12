import {
  ChangeDetectionStrategy,
  Component,
  Injector,
  effect,
  inject,
  signal,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { ActivatedRoute, Router } from "@angular/router";
import { map } from "rxjs";
import { IpcService } from "../../../core/ipc.service";
import { TabsService } from "../../../core/tabs.service";
import { tabKeyFor } from "../../../core/tab-keys";
import type { OrgItemDetail } from "../../../core/models";
import { MarkdownComponent } from "../../../shared/markdown/markdown.component";

/**
 * Viewer for ONE org-brain item (`/org-item/:id`). Reached from an org-origin
 * source chip (Ask) or an org card in the Notes / Meetings list.
 *
 * On entry the viewer FIRST resolves the item back to THIS device's local
 * editable source (`org_resolve_source`, F2): if the caller is the AUTHOR it
 * redirects (replaceUrl) to their editable original — a `/notes/:id` note or a
 * `/meeting/:id` detail (whose edits re-publish) — so they land on the thing they
 * can change, not a read-only replica. A non-author (no local source ⇒ `null`)
 * falls through to the rich READ-ONLY document view: an "Org Brain" badge + the
 * org name + author hint + date + revision, the decrypted `OrgItemDetail.markdown`
 * rendered inside a frosted document card. Org items are deliberately-disclosed
 * org content (no lock gate applies), so the read view has no edit/share affordance.
 *
 * The route param drives the load via an IPC-on-signal-change effect (T1) with a
 * stale-result guard, so navigating between org items in place re-fetches
 * correctly. Back returns to the Notes home (mirrors the note-editor `back()`).
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
  private readonly router = inject(Router);
  private readonly injector = inject(Injector);
  private readonly tabsService = inject(TabsService);

  /** The `:id` route param as a signal (re-fetches when it changes in place). */
  private readonly itemId = toSignal(
    this.route.paramMap.pipe(map((p) => p.get("id"))),
    { initialValue: this.route.snapshot.paramMap.get("id") },
  );

  private readonly _item = signal<OrgItemDetail | null>(null);
  readonly item = this._item.asReadonly();
  /**
   * The origin org's display name (drives the metadata strip's org label).
   * Best-effort: resolved by matching the item against each org's browsable
   * items; empty when it can't be determined (offline / already-tombstoned).
   */
  private readonly _orgName = signal("");
  readonly orgName = this._orgName.asReadonly();
  readonly loading = signal(true);
  readonly error = signal<string | null>(null);

  constructor() {
    // Resolve + fetch the org item whenever the route id changes. Async IPC
    // effect (T1) — first resolves the local editable source and (for the author)
    // redirects; otherwise loads the read-only detail. Stale-guarded on the
    // captured id so an in-place route change drops the late reply.
    effect(
      () => {
        const id = this.itemId();
        void this.resolveThenLoad(id);
      },
      { injector: this.injector },
    );
  }

  /**
   * F2 — author-editable routing. Resolve the item's LOCAL editable source; if
   * this user authored it (a non-null ref) redirect to the editable original
   * (`replaceUrl` so Back doesn't bounce into the redirect). Otherwise render the
   * read-only view. The resolve is best-effort: any failure (no Tauri host, an
   * unknown command on an older backend, a transient error) falls through to the
   * read-only load rather than blocking the view. Stale-guarded on `id`.
   */
  private async resolveThenLoad(id: string | null): Promise<void> {
    if (!id) {
      this.loading.set(false);
      this.error.set("No item id.");
      return;
    }
    this.loading.set(true);
    this.error.set(null);
    this._item.set(null);
    this._orgName.set("");
    try {
      const ref = await this.ipc.orgResolveSource(id);
      if (this.itemId() !== id) {
        return; // stale — the route moved on under us
      }
      if (ref) {
        // The author's editable original — replace so Back skips the redirect.
        const path =
          ref.kind === "document"
            ? ["/notes", ref.sourceId]
            : ["/meeting", ref.sourceId];
        await this.router.navigate(path, { replaceUrl: true });
        return;
      }
    } catch {
      // Best-effort: fall through to the read-only load on any resolve failure.
      if (this.itemId() !== id) {
        return;
      }
    }
    await this.load(id);
  }

  /** Load the read-only detail (+ best-effort org name) for a non-author. Stale-guarded. */
  private async load(id: string): Promise<void> {
    try {
      const item = await this.ipc.orgGetItem(id);
      if (this.itemId() !== id) {
        return; // stale — the route moved on under us
      }
      this._item.set(item);
      // Adopt the real title (mirrors note-editor's setTitle) — the caller
      // already passes a best-known title when opening the tab, but this
      // corrects it once the authoritative decrypted detail loads.
      this.tabsService.setTitle(tabKeyFor("org-item", id), item.title || "Shared note");
      void this.resolveOrgName(id);
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

  /**
   * Best-effort org name for the metadata strip: the viewer route carries no org
   * context and `OrgItemDetail` has no `orgId`, so we discover the origin org by
   * matching the item id against each org's browsable items. Never throws; leaves
   * the label empty when it can't be resolved. Stale-guarded on `id`.
   */
  private async resolveOrgName(id: string): Promise<void> {
    try {
      const orgs = await this.ipc.orgListStatuses();
      for (const org of orgs) {
        const items = await this.ipc.listOrgItems(org.orgId).catch(() => []);
        if (items.some((it) => it.itemId === id)) {
          if (this.itemId() === id) {
            this._orgName.set(org.name);
          }
          return;
        }
      }
    } catch {
      /* best-effort: leave the org label empty */
    }
  }

  /** Back returns to the Notes home (mirrors the note-editor `back()`, B1). */
  back(): void {
    void this.router.navigate(["/notes"]);
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
