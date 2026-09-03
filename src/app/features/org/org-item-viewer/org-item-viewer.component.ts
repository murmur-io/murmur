import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  Injector,
  computed,
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
import type {
  NoteAttachmentDto,
  OrgAccess,
  OrgItemDetail,
} from "../../../core/models";
import { MarkdownComponent } from "../../../shared/markdown/markdown.component";
import { ConnectionsComponent } from "../../../shared/connections/connections.component";
import { NoteChatComponent } from "../../notes/note-chat/note-chat.component";
import { ToastService } from "../../../services/toast.service";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";
import { DateFormatService } from "../../../core/date-format.service";

const STABLE_UUID_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/**
 * Accept only the backend-issued stable identity for this exact document. A
 * historical/malformed detail falls back to its immutable item id instead of
 * letting arbitrary punctuation turn a route id into a stable-head lookup.
 */
function stableLinkIdOf(item: OrgItemDetail | null): string | null {
  const docId = item?.docId?.trim();
  const linkId = item?.linkId?.trim();
  if (!docId || !linkId) {
    return null;
  }
  const parts = linkId.split(":");
  if (
    parts.length !== 2 ||
    !STABLE_UUID_PATTERN.test(parts[0]) ||
    !STABLE_UUID_PATTERN.test(parts[1]) ||
    !STABLE_UUID_PATTERN.test(docId) ||
    parts[1].toLowerCase() !== docId.toLowerCase()
  ) {
    return null;
  }
  return `${parts[0]}:${parts[1]}`;
}

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
 *
 * WITHDRAWN CONTENT (2026-07-26). This view is kept ALIVE while backgrounded by
 * `TabRouteReuseStrategy` and previously fetched exactly once, on route entry —
 * so an item the org withdrew stayed fully readable here indefinitely, long
 * after the backend evicted it from every other surface. It now subscribes to
 * `org-feed-updated` (fired by the background sync AND by the anti-entropy
 * reconcile sweep whenever the local replica changes) and re-fetches; a
 * `null` detail means the item is gone, so the content is dropped immediately,
 * the view says so plainly, and the tab is closed. Mirrors `TabsService`'s
 * `onContentDeleted` fan-out for notes/meetings.
 */
@Component({
  selector: "app-org-item-viewer",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MarkdownComponent, NoteChatComponent, ConnectionsComponent],
  templateUrl: "./org-item-viewer.component.html",
  styleUrl: "./org-item-viewer.component.scss",
})
export class OrgItemViewerComponent {
  private readonly dates = inject(DateFormatService);

  private readonly ipc = inject(IpcService);
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);
  private readonly injector = inject(Injector);
  private readonly tabsService = inject(TabsService);
  private readonly toast = inject(ToastService);
  private readonly destroyRef = inject(DestroyRef);
  private readonly errorCopy = inject(ErrorCopyService);

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
  /**
   * True once this item has been WITHDRAWN from the org (the re-fetch came back
   * empty). Renders the "no longer shared" state instead of the — now stale —
   * content, which is the whole point: a revoked note must stop being readable
   * here the moment the local replica is evicted, not on the next app launch.
   */
  private readonly _removed = signal(false);
  readonly removed = this._removed.asReadonly();
  /** Decrypted attachment DTOs for this received org item; view-only. */
  readonly attachments = signal<NoteAttachmentDto[]>([]);

  /**
   * Whether THIS device has a live sharing-account session — loaded alongside the item so a
   * non-editable read-only view can give an honest reason instead of silently hiding the Edit
   * button with no explanation. Best-effort (defaults to `true`, i.e. "assume signed in") so a
   * failed `accountStatus()` call never wrongly claims "sign in" when the real reason is
   * something else.
   */
  private readonly _loggedIn = signal(true);

  /**
   * A short, honest reason the Edit button isn't shown, or `null` when it's simply editable.
   * Deliberately does NOT claim to distinguish "someone else's note" from "no author info yet
   * (an older/stale sync)" — the backend's `editable` flag doesn't carry that distinction (both
   * collapse to `author_user_id !== me`), so guessing which one it is would be a fabricated
   * message. It only ever tells the ONE thing this device can know for certain: whether it has a
   * live session at all.
   */
  readonly notEditableReason = computed<string | null>(() => {
    const it = this._item();
    if (!it || this.canEdit(it)) {
      return null;
    }
    return this._loggedIn()
      ? "View only — the author or Org Owner can enable editing."
      : "Sign in to use the permissions granted to your account.";
  });

  // --- Edit-in-place (server-authorized author/owner/editor) ----------------
  /** True while the author is editing this item in place (drives the editor UI). */
  readonly editing = signal(false);
  /** Draft title/body bound to the inline editor while {@link editing}. */
  readonly titleDraft = signal("");
  readonly markdownDraft = signal("");
  /** True while `orgUpdateOwnItem` (seal → publish → tombstone-old) is in flight. */
  readonly saving = signal(false);
  /**
   * Fixed, content-free state for a lost optimistic-concurrency race. The draft
   * signals remain untouched until the user explicitly chooses to open the
   * authoritative head.
   */
  private readonly _editConflict = signal(false);
  readonly editConflict = this._editConflict.asReadonly();
  /** True while the explicit sync + current-head read is in flight. */
  readonly openingLatest = signal(false);
  /** Fixed failure state; raw sync/read errors never reach the template. */
  private readonly _openLatestFailed = signal(false);
  readonly openLatestFailed = this._openLatestFailed.asReadonly();
  /** Strict `orgId:docId` identity used by the explicit recovery read. */
  readonly latestLinkId = computed<string | null>(() =>
    stableLinkIdOf(this._item()),
  );
  /** True while a manager changes this document's organization-wide access. */
  readonly changingAccess = signal(false);

  /**
   * Ask Brain pane open/closed — the read-only viewer's right-hand "Ask about this note" split. It
   * grounds in THIS shared org item (pinned server-side via `pinnedOrgItemId`, since org-feed
   * content is not otherwise reachable by the local Brain's search). Session signal, default
   * COLLAPSED; the toggle is hidden while editing-in-place (a focused writing mode).
   */
  readonly orgChatOpen = signal(false);
  toggleOrgChat(): void {
    this.orgChatOpen.update((v) => !v);
  }

  /** Released on destroy so the org-feed listener never outlives this view. */
  private feedUnlisten: (() => void) | null = null;
  private feedDestroyed = false;

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

    // Live convergence: the backend fires this content-free event whenever the
    // local org replica actually changed (a feed tombstone, the anti-entropy
    // reconcile sweep, or a revoke). A backgrounded tab gets no lifecycle hook,
    // so this subscription is the ONLY thing that can tell an already-rendered
    // viewer its item is gone.
    this.destroyRef.onDestroy(() => {
      this.feedDestroyed = true;
      this.feedUnlisten?.();
    });
    void this.ipc
      .onOrgFeedUpdated(() => {
        void this.revalidate();
      })
      .then((un) => {
        if (this.feedDestroyed) {
          un();
        } else {
          this.feedUnlisten = un;
        }
      })
      .catch(() => {
        /* best-effort: no Tauri host (plain browser) → no live convergence */
      });
  }

  /**
   * Re-fetch the currently displayed item after the org replica changed.
   *
   * `null` ⇒ the item was withdrawn (or its org was disabled/left): drop the
   * content NOW, say so, and close the tab. Otherwise refresh in place —
   * skipping the content swap while the author is editing, so a background sync
   * can never silently overwrite an in-progress draft. Any IPC failure is
   * ignored: a transient error must never be mistaken for "withdrawn".
   */
  private async revalidate(): Promise<void> {
    const routeId = this.itemId();
    if (!routeId || this._removed() || this.loading()) {
      return;
    }
    // A successful edit replaces the immutable feed item id. During conflict
    // recovery, orgSyncNow can emit this event before its Promise resolves, so
    // the route still names the predecessor. Resolve through the validated
    // stable document identity when the loaded detail provides one.
    const lookupId = stableLinkIdOf(this._item()) ?? routeId;
    let detail: OrgItemDetail | null;
    try {
      detail = await this.ipc.orgGetItem(lookupId);
    } catch {
      return;
    }
    if (this.itemId() !== routeId || this._removed()) {
      return;
    }
    if (!detail) {
      // `null` from a validated stable lookup is a real resource withdrawal,
      // not merely a superseded immutable revision. Keep that fail-closed
      // eviction behavior even while an edit/recovery operation is in flight.
      this.markRemoved(routeId);
      return;
    }
    if (this.editing() || this.saving() || this.openingLatest()) {
      return;
    }
    this._item.set(detail);
    this.tabsService.setTitle(
      tabKeyFor("org-item", routeId),
      detail.title || "Shared note",
    );
    void this.reloadAttachments(detail.itemId, routeId);
  }

  /**
   * The item is gone from the org: evict everything this view is holding, tell
   * the user plainly, and close the tab (which, per `TabsService.closeTab`,
   * also destroys the cached detached instance and navigates to a neighbor).
   * A deep-linked view with no tab simply stays on the "no longer shared" state.
   */
  private markRemoved(id: string): void {
    this._removed.set(true);
    this._item.set(null);
    this.attachments.set([]);
    this.editing.set(false);
    this._editConflict.set(false);
    this._openLatestFailed.set(false);
    this.confirmingRemove.set(false);
    this.orgChatOpen.set(false);
    this.toast.info("This shared note is no longer available in the org.");
    void this.tabsService.closeTab(tabKeyFor("org-item", id));
  }

  /**
   * Refresh attachments for the returned live item, independently of the
   * possibly-superseded route id. Stale-guarded, never throws.
   */
  private async reloadAttachments(
    ownerItemId: string,
    routeId: string,
  ): Promise<void> {
    try {
      const rows = await this.ipc.listNoteAttachments("org", ownerItemId);
      if (
        this.itemId() === routeId &&
        this._item()?.itemId === ownerItemId &&
        !this._removed()
      ) {
        this.attachments.set(Array.isArray(rows) ? rows : []);
      }
    } catch {
      /* best-effort: keep whatever is already rendered */
    }
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
    this._removed.set(false);
    this.attachments.set([]);
    this._orgName.set("");
    // A route change (incl. the post-save redirect to the superseded item) always exits edit mode.
    this.editing.set(false);
    this._editConflict.set(false);
    this._openLatestFailed.set(false);
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
      if (!item) {
        // A stale link/citation to an item the org already withdrew. Say so
        // plainly rather than rendering an empty shell. (No tab-close here —
        // the user just opened this; closing out from under them would be
        // disorienting. `revalidate()` owns the disappeared-while-open case.)
        this._removed.set(true);
        this.attachments.set([]);
        return;
      }
      this._item.set(item);
      await this.reloadAttachments(item.itemId, id);
      if (this.itemId() !== id) {
        return;
      }
      // Adopt the real title (mirrors note-editor's setTitle) — the caller
      // already passes a best-known title when opening the tab, but this
      // corrects it once the authoritative decrypted detail loads.
      this.tabsService.setTitle(
        tabKeyFor("org-item", id),
        item.title || "Shared note",
      );
      void this.resolveOrgName(id);
      void this.refreshLoggedIn(id);
    } catch (e) {
      if (this.itemId() !== id) {
        return;
      }
      this.error.set(this.errorCopy.humanize(e));
      this._item.set(null);
      this.attachments.set([]);
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
      // Metadata resolution for an already-admitted replica is local-only.
      // Opening a shared item must not refresh tokens or contact the relay just
      // to render its organization label.
      const orgs = await this.ipc.orgListCachedStatuses();
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

  /**
   * Best-effort session check backing {@link notEditableReason} — never blocks or
   * affects `editable` itself, only which read-only explanation is shown. Any
   * failure leaves `_loggedIn` at its "assume signed in" default so a transient
   * IPC error never wrongly claims "sign in" when the real reason is something
   * else. Stale-guarded on `id` like every other fetch here.
   */
  private async refreshLoggedIn(id: string): Promise<void> {
    try {
      const status = await this.ipc.accountStatus();
      if (this.itemId() === id) {
        this._loggedIn.set(status.loggedIn);
      }
    } catch {
      /* best-effort: leave the default */
    }
  }

  /** Enter edit mode when the server grants it, seeding drafts from the item. */
  startEdit(): void {
    const it = this._item();
    if (!it || !this.canEdit(it)) {
      return;
    }
    this.titleDraft.set(it.title);
    this.markdownDraft.set(it.markdown);
    this._editConflict.set(false);
    this._openLatestFailed.set(false);
    this.editing.set(true);
  }

  /** Leave edit mode without saving (ignored mid-save). */
  cancelEdit(): void {
    if (this.saving() || this.openingLatest()) {
      return;
    }
    this.editing.set(false);
    this._editConflict.set(false);
    this._openLatestFailed.set(false);
  }

  /**
   * Save the edit: re-publish the org item (rev+1) through the backend's consent +
   * seal + verify-before-egress gates. The server mints a NEW item id (feed items
   * are immutable), so on success we navigate to it (`replaceUrl`) — the route-id
   * effect re-loads the fresh, still-editable item. A blank title/body or a
   * no-change save is allowed (the backend short-circuits identical content).
   */
  async saveEdit(): Promise<void> {
    const it = this._item();
    if (!it || this.saving() || this.openingLatest()) {
      return;
    }
    const title = this.titleDraft().trim();
    const markdown = this.markdownDraft();
    this.saving.set(true);
    try {
      const newId = await this.ipc.orgUpdateItem(it.itemId, title, markdown);
      this.editing.set(false);
      this._editConflict.set(false);
      this._openLatestFailed.set(false);
      this.toast.success("Changes shared to the org");
      // The superseded item has a new id — land on it so the viewer reloads fresh.
      await this.router.navigate(["/org-item", newId], { replaceUrl: true });
    } catch (e) {
      if (this.errorCopy.is(e, "org-edit-conflict")) {
        // Keep both draft signals byte-for-byte intact. Recovery is explicit:
        // no automatic re-share, permission write, sync, or content replacement.
        this._editConflict.set(true);
        this._openLatestFailed.set(false);
      } else {
        this.toast.danger(
          "Couldn’t save. Check that you still have edit access; your draft is still here.",
        );
      }
    } finally {
      this.saving.set(false);
    }
  }

  /**
   * Resolve the relay-authoritative current head after a direct-edit conflict.
   *
   * The backend accepts the stable `orgId:docId` link identity in `orgGetItem`.
   * We first sync that org only after this explicit click, then perform the
   * read-only stable-id lookup and replace-navigate to the returned live item.
   * This path never re-shares content or changes document access.
   */
  async openLatestAfterConflict(): Promise<void> {
    const conflicted = this._item();
    const linkId = this.latestLinkId();
    if (!this._editConflict() || !linkId || this.openingLatest()) {
      return;
    }
    const [orgId] = linkId.split(":");

    const routeId = this.itemId();
    this.openingLatest.set(true);
    this._openLatestFailed.set(false);
    try {
      await this.ipc.orgSyncNow(orgId);
      const latest = await this.ipc.orgGetItem(linkId);
      if (this.itemId() !== routeId || this._item() !== conflicted) {
        return;
      }
      if (!latest) {
        this._openLatestFailed.set(true);
        return;
      }

      // The user explicitly chose the current head, so the old draft can now
      // leave the editor. Adopt the already-resolved detail before navigating;
      // the route effect then revalidates it by its current item id.
      this._item.set(latest);
      this.attachments.set([]);
      this.editing.set(false);
      this._editConflict.set(false);
      this.tabsService.setTitle(
        tabKeyFor("org-item", latest.itemId),
        latest.title || "Shared note",
      );
      await this.router.navigate(["/org-item", latest.itemId], {
        replaceUrl: true,
      });
    } catch {
      if (this.itemId() === routeId) {
        this._openLatestFailed.set(true);
      }
    } finally {
      this.openingLatest.set(false);
    }
  }

  /** Change member access, then re-read server-authoritative permissions. */
  async setAccess(access: OrgAccess): Promise<void> {
    const it = this._item();
    if (
      !it ||
      !this.canManage(it) ||
      this.changingAccess() ||
      this.accessOf(it) === access
    ) {
      return;
    }
    this.changingAccess.set(true);
    try {
      await this.ipc.orgSetItemAccess(it.itemId, access);
      const fresh = await this.ipc.orgGetItem(it.itemId);
      if (!fresh) {
        this.markRemoved(it.itemId);
        return;
      }
      this._item.set(fresh);
      this.toast.success(
        access === "edit" ? "Members can now edit" : "Changed to view only",
      );
    } catch {
      this.toast.danger(
        "Couldn’t change access. Only the author or Org Owner can manage it.",
      );
    } finally {
      this.changingAccess.set(false);
    }
  }

  /** Compatibility helper while replicas made by an older client still expose `editable`. */
  canEdit(item: OrgItemDetail): boolean {
    return item.canEdit ?? item.editable ?? false;
  }

  /** Compatibility for author-owned replicas created before the permission split. */
  canManage(item: OrgItemDetail): boolean {
    return item.canManage ?? item.editable ?? false;
  }

  /** Historical replicas predate access metadata and are fail-closed view-only. */
  accessOf(item: OrgItemDetail): OrgAccess {
    return item.access ?? "view";
  }

  /** Back returns to the Notes home (mirrors the note-editor `back()`, B1). */
  back(): void {
    void this.router.navigate(["/notes"]);
  }

  // --- Remove-from-org (author only; the "author has no delete affordance on a
  // second device" gap) — mirrors note-editor's askDelete/cancelDelete/doDelete
  // confirm shape. DELIBERATELY "leave/remove from org", not "destroy the
  // original": see IpcService.deleteOrgItemAsAuthor's doc.
  readonly confirmingRemove = signal(false);

  askRemove(): void {
    this.confirmingRemove.set(true);
  }

  cancelRemove(): void {
    this.confirmingRemove.set(false);
  }

  async doRemove(): Promise<void> {
    const it = this._item();
    if (!it || !this.canManage(it) || this.saving()) {
      return;
    }
    this.saving.set(true);
    try {
      await this.ipc.deleteOrgItemAsAuthor(it.itemId);
      this.toast.success("Removed from the org");
      void this.router.navigate(["/notes"]);
    } catch (e) {
      this.toast.danger(this.errorCopy.because("Couldn’t remove", e));
      this.confirmingRemove.set(false);
    } finally {
      this.saving.set(false);
    }
  }

  /** Presentational: an ISO timestamp → a friendly local date. */
  /** Formatted through {@link DateFormatService} — the one place a date becomes user-visible text. */
  formatDate(iso: string): string {
    return this.dates.day(iso);
  }
}
