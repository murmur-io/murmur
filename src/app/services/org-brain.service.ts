import { DestroyRef, Injectable, inject, signal } from "@angular/core";
import { IpcService } from "../core/ipc.service";
import type {
  MeetingOrgShareRow,
  OrgItemHeader,
  OrgStatus,
} from "../core/models";

/**
 * Signal store for the raw "Shared Brains" (org) roster + items — the ONE
 * source of `_orgs`/`_orgItems`/`_myOrgShares` that {@link NotesHomeComponent}
 * and {@link LibraryComponent} both used to duplicate as component-local
 * signals. `providedIn: 'root'` is the point: a component-local signal is
 * WIPED to its initial empty value every time the component is destroyed and
 * recreated (e.g. leaving `/notes` for a note, then coming back) — a root
 * service instance outlives that, so the chip row / merged list render with
 * the LAST-KNOWN org data INSTANTLY on return, while {@link loadOrgs} quietly
 * re-fetches underneath (never gated behind a blocking "loading" state — see
 * the pattern note in `angular-zoneless.md` §9).
 *
 * The live-refresh wiring (the `org-feed-updated` event + window-focus) is
 * subscribed ONCE here, for the app's lifetime, instead of once per component
 * mount — a strict improvement (was: re-subscribed/torn-down on every visit).
 *
 * Each consumer keeps its OWN derived view over the shared raw data (Library
 * narrows to `kind === "meeting"`, Notes excludes it — "notes has notes,
 * meetings has meetings", PR #259) and its OWN `activeOrgId` chip-row
 * selection — those are legitimately per-view UI state, not shared.
 */
@Injectable({ providedIn: "root" })
export class OrgBrainService {
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);

  private readonly _orgs = signal<OrgStatus[]>([]);
  private readonly _orgItems = signal<Record<string, OrgItemHeader[]>>({});
  private readonly _myOrgShares = signal<MeetingOrgShareRow[]>([]);
  private readonly _loading = signal(false);

  /** Every org (Shared Brain) this user belongs to. */
  readonly orgs = this._orgs.asReadonly();
  /** Each org's shared items keyed by orgId (`listOrgItems`) — UNFILTERED (every kind). */
  readonly orgItems = this._orgItems.asReadonly();
  /** The caller's own outbound meeting org-shares (Library row badge). */
  readonly myOrgShares = this._myOrgShares.asReadonly();
  /** True while a (re)load is in flight — a HINT (e.g. the chip-row spinner), never a
   * render gate: a consumer's template must keep showing {@link orgs}/{@link orgItems}
   * while this is true, not hide them behind it. */
  readonly loading = this._loading.asReadonly();

  /** Bumped per load so a late (stale) reload result is dropped. */
  private loadSeq = 0;
  private feedUnlisten: (() => void) | null = null;
  private feedDestroyed = false;
  private readonly onWindowFocus = (): void => {
    // Focus is passive navigation, not an explicit sync action. A background
    // sync/feed event will update the local replica independently.
    void this.loadLocalOrgs();
  };

  constructor() {
    window.addEventListener("focus", this.onWindowFocus);
    this.destroyRef.onDestroy(() => {
      // A root service is never actually destroyed in practice (it outlives every
      // component), but honor the contract in case a test harness tears it down.
      this.feedDestroyed = true;
      this.feedUnlisten?.();
      window.removeEventListener("focus", this.onWindowFocus);
    });
    void this.ipc
      .onOrgFeedUpdated(() => void this.loadLocalOrgs())
      .then((un) => {
        if (this.feedDestroyed) {
          un();
        } else {
          this.feedUnlisten = un;
        }
      })
      .catch(() => {
        /* best-effort: no Tauri host (e.g. plain browser) → no live refresh */
      });
  }

  /**
   * (Re)load the org roster + every org's shared items + the caller's own
   * meeting org-shares. Stale-guarded on {@link loadSeq}; best-effort
   * throughout. Remote refresh failure falls through to the local replica;
   * failure to read the local membership gate itself clears rendered org data
   * fail-closed.
   *
   * This is the explicit sync path: it may ask the backend to refresh the
   * encrypted org replica before reading it. A passive Shared Brains route
   * mount must use {@link loadLocalOrgs} instead so navigation itself never
   * introduces network egress.
   */
  async loadOrgs(): Promise<void> {
    const seq = ++this.loadSeq;
    this._loading.set(true);
    try {
      try {
        await this.ipc.orgRefresh();
      } catch {
        /* offline / no server → fall through to the local replica */
      }
      await this.readLocalReplica(seq);
    } finally {
      if (seq === this.loadSeq) {
        this._loading.set(false);
      }
    }
  }

  /**
   * Read only the already-admitted local org replica. This path deliberately
   * excludes `org_refresh`: opening Shared Brains is a local database read,
   * not an implicit synchronization or consent event.
   */
  async loadLocalOrgs(): Promise<void> {
    const seq = ++this.loadSeq;
    this._loading.set(true);
    try {
      await this.readLocalReplica(seq);
    } finally {
      if (seq === this.loadSeq) {
        this._loading.set(false);
      }
    }
  }

  private async readLocalReplica(seq: number): Promise<void> {
    let orgs: OrgStatus[];
    try {
      // PER-INSTANCE ORG TOGGLE: a disabled org must not appear as a pickable
      // "Shared brains" entry — Settings keeps its own unfiltered fetch so
      // every joined org's toggle stays reachable there.
      orgs = (await this.ipc.orgListCachedStatuses()).filter(
        (o) => o.contextEnabled,
      );
    } catch {
      if (seq === this.loadSeq) {
        this.clearReplica();
      }
      return;
    }
    if (seq !== this.loadSeq) {
      return;
    }
    const [itemLists, ownShares] = await Promise.all([
      Promise.all(
        orgs.map((o) =>
          this.ipc.listOrgItems(o.orgId).catch(() => [] as OrgItemHeader[]),
        ),
      ),
      this.ipc.listMeetingOrgShares().catch(() => [] as MeetingOrgShareRow[]),
    ]);
    if (seq !== this.loadSeq) {
      return;
    }
    const byOrg: Record<string, OrgItemHeader[]> = {};
    orgs.forEach((o, i) => {
      byOrg[o.orgId] = itemLists[i];
    });
    this._orgs.set(orgs);
    this._orgItems.set(byOrg);
    this._myOrgShares.set(ownShares);
  }

  private clearReplica(): void {
    this._orgs.set([]);
    this._orgItems.set({});
    this._myOrgShares.set([]);
  }
}
