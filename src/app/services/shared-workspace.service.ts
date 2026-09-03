import {
  DestroyRef,
  Injectable,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";

import { IpcService } from "../core/ipc.service";
import type {
  ContainerShareStatus,
  OrgAccess,
  OrgShareTargetRow,
  SharedContainerNode,
  SharedPlacementTarget,
  SharedWorkspace,
} from "../core/models";

/**
 * The received forest — containers and items other members shared with this
 * user — plus the roster of containers THIS user publishes.
 *
 * `providedIn: "root"` is the point, and it is the same reason
 * {@link WorkspaceService} is root-provided: the sidebar is destroyed and
 * recreated on every navigation, so a component-local signal would empty the
 * shared tree on each return and the user would watch it repopulate. A root
 * instance outlives the component, so the previous rows survive the remount
 * while {@link load} quietly replaces them underneath
 * (`angular-zoneless.md` §8).
 *
 * {@link loading} is a HINT, never a render gate. A template that hides cached
 * rows behind it produces the 2026-07-12 "reload flash".
 */
@Injectable({ providedIn: "root" })
export class SharedWorkspaceService {
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);

  private readonly _spaces = signal<SharedContainerNode[]>([]);
  private readonly _sharedBrains = signal<SharedContainerNode | null>(null);
  private readonly _containerShares = signal<ContainerShareStatus[]>([]);
  private readonly _shareTargets = signal<OrgShareTargetRow[]>([]);
  private readonly _loading = signal(false);
  private readonly _loadFailed = signal(false);

  /** Received SPACES — each renders as its own top-level sidebar row. */
  readonly spaces = this._spaces.asReadonly();
  /**
   * The virtual "Shared Brains" Workspace: received folders with no shared-Workspace
   * parent, plus every received item with no container at all. `null` until the
   * first load resolves.
   */
  readonly sharedBrains = this._sharedBrains.asReadonly();
  /** Containers THIS device publishes — drives the sidebar's shared marker. */
  readonly containerShares = this._containerShares.asReadonly();
  /** Items published on their own, keyed for the row marker. */
  readonly shareTargets = this._shareTargets.asReadonly();
  /** True while a (re)load is in flight. A hint, never a render gate. */
  readonly loading = this._loading.asReadonly();
  /**
   * The last load could not read the shared workspace.
   *
   * Every read here swallowed its error into `null`/`[]`, so a failure rendered as an EMPTY
   * workspace — indistinguishable from "nothing is shared with you". Somebody whose relay was
   * unreachable was told, in effect, that their team had shared nothing, and there was no retry
   * because there was nothing to retry from.
   *
   * Kept separate from the data signals on purpose: the last-known rows stay on screen while this
   * is true, so a failed refresh degrades to "possibly stale" rather than blanking what the user
   * was already reading.
   */
  readonly loadFailed = this._loadFailed.asReadonly();

  /** Nothing shared in either direction — the sidebar renders no shared rows. */
  readonly isEmpty = computed(() => {
    const brains = this._sharedBrains();
    return (
      this._spaces().length === 0 &&
      (brains === null ||
        (brains.folders.length === 0 && brains.items.length === 0))
    );
  });

  /** The org an item was published to on its own, keyed `<kind>:<id>`. */
  readonly shareByItem = computed(() => {
    const map = new Map<string, OrgShareTargetRow>();
    for (const target of this._shareTargets()) {
      map.set(`${target.kind}:${target.id}`, target);
    }
    return map;
  });

  /** The container share for one local folder, if this device publishes it. */
  readonly shareByFolder = computed(() => {
    const map = new Map<string, ContainerShareStatus>();
    for (const share of this._containerShares()) {
      map.set(share.folderId, share);
    }
    return map;
  });

  /** Bumped per load so a late (stale) reload result is dropped. */
  private loadSeq = 0;
  private feedUnlisten: (() => void) | null = null;
  private feedDestroyed = false;

  constructor() {
    // A workspace mutation can change what a shared container HOLDS — a note
    // created in it, moved out of it, or deleted. Reconcile straight away so
    // "the folder is live" is true in seconds rather than at the next
    // background tick. The backend runs the same sweep on that tick, so a
    // missed bump costs latency, never correctness.
    effect(() => {
      if (this.ipc.workspaceMutationRevision() > 0) {
        void this.syncAfterWorkspaceMutation();
      }
    });
    this.destroyRef.onDestroy(() => {
      // A root service is never actually destroyed in practice; honor the
      // contract in case a test harness tears it down.
      this.feedDestroyed = true;
      this.feedUnlisten?.();
    });
    void this.ipc
      .onOrgFeedUpdated(() => void this.load())
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

  private ensureLoadedInFlight: Promise<void> | null = null;

  /**
   * Read the received forest unless it has been read already, coalescing
   * concurrent callers. Both `app-workspace-tree` instances in the sidebar ask
   * for this on construction, and neither sees the other's result yet — see the
   * matching note on `WorkspaceService.ensureLoaded`.
   */
  ensureLoaded(): Promise<void> {
    if (this._sharedBrains() !== null) {
      return Promise.resolve();
    }
    this.ensureLoadedInFlight ??= this.load().finally(() => {
      this.ensureLoadedInFlight = null;
    });
    return this.ensureLoadedInFlight;
  }

  /**
   * Re-read the received forest and the outbound roster. Local reads only — no
   * network, because rendering a sidebar must never be an egress event.
   */
  async load(): Promise<void> {
    const seq = ++this.loadSeq;
    this._loading.set(true);
    try {
      // `allSettled`, not `all`: one unreachable read must not discard the two that succeeded.
      // What changes is that a rejection is now RECORDED rather than silently becoming an empty
      // list — the difference between "nothing is shared with you" and "we could not find out".
      const [workspace, shares, targets] = await Promise.allSettled([
        this.ipc.listSharedWorkspace(),
        this.ipc.listContainerShareStatus(),
        this.ipc.listOrgShareTargets(),
      ]);
      const failed =
        workspace.status === "rejected" ||
        shares.status === "rejected" ||
        targets.status === "rejected";
      if (seq !== this.loadSeq) {
        return;
      }
      this._loadFailed.set(failed);
      // Each leg applies only if it actually resolved. A rejected leg leaves its previous value
      // alone rather than clearing it, so a partial failure never erases rows the user can still
      // legitimately see.
      if (workspace.status === "fulfilled" && workspace.value) {
        this.applyWorkspace(workspace.value);
      }
      if (shares.status === "fulfilled") {
        this._containerShares.set(shares.value);
      }
      if (targets.status === "fulfilled") {
        this._shareTargets.set(targets.value);
      }
    } finally {
      if (seq === this.loadSeq) {
        this._loading.set(false);
      }
    }
  }

  /**
   * Bring every shared container back in line with the local tree, then reload.
   *
   * Called after a workspace mutation so a note added to a shared folder
   * publishes right away. The backend runs the same sweep on a timer, so a
   * missed call costs latency, never correctness.
   */
  async syncAfterWorkspaceMutation(): Promise<void> {
    try {
      const changed = await this.ipc.syncContainerShares();
      if (changed > 0) {
        await this.load();
      }
    } catch {
      /* best-effort: the background tick converges anyway */
    }
  }

  /**
   * File a received container or document somewhere in this user's own tree.
   * Device-local: the owner and every other member see nothing of it.
   */
  async place(
    orgId: string,
    targetKind: SharedPlacementTarget,
    targetId: string,
    localParentId: string | null,
    position: number,
  ): Promise<void> {
    await this.ipc.setSharedPlacement(
      orgId,
      targetKind,
      targetId,
      localParentId,
      position,
    );
    await this.load();
  }

  /** Return a received object to wherever its owner filed it. */
  async unplace(
    orgId: string,
    targetKind: SharedPlacementTarget,
    targetId: string,
  ): Promise<void> {
    await this.ipc.clearSharedPlacement(orgId, targetKind, targetId);
    await this.load();
  }

  /** Publish a whole Workspace or Folder. Reloads so the marker appears at once. */
  async share(
    orgId: string,
    folderId: string,
    access: OrgAccess,
    scrub: boolean,
  ): Promise<void> {
    await this.ipc.shareContainerToOrg(orgId, folderId, access, scrub);
    await this.load();
  }

  /** Stop sharing a container. */
  async unshare(orgId: string, folderId: string): Promise<void> {
    await this.ipc.unshareContainer(orgId, folderId);
    await this.load();
  }

  /** Re-permission a container and every document filed under it. */
  async setAccess(
    orgId: string,
    folderId: string,
    access: OrgAccess,
  ): Promise<void> {
    await this.ipc.setContainerShareAccess(orgId, folderId, access);
    await this.load();
  }

  private applyWorkspace(workspace: SharedWorkspace): void {
    this._spaces.set(workspace.spaces ?? []);
    this._sharedBrains.set(workspace.sharedBrains ?? null);
  }
}
