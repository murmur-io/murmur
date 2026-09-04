import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
  viewChild,
} from "@angular/core";

import { AskHistoryPrivacyBarrierService } from "../../core/ask-history-privacy-barrier.service";
import { IpcService } from "../../core/ipc.service";
import { TabRouteReuseStrategy } from "../../core/tab-route-reuse.strategy";
import type {
  ContainerLevel,
  LinkKind,
  PickerContainerNode,
  PickerGroup,
  PickerItemKind,
  PickerRow,
  SharedContainerNode,
  SharedWorkspace,
} from "../../core/models";
import {
  MurIconComponent,
  type ShellIcon,
} from "../../design-system/icon/icon.component";
import { TeleportToBodyDirective } from "../../design-system/teleport-to-body.directive";
import { DebounceService } from "../../services/debounce.service";
import { FoldersService } from "../../services/folders.service";

/** How many leaves one lazy page carries. Matches the backend's centred bootstrap window. */
const PAGE_SIZE = 24;
/** Bounded search page. The backend clamps to 50; asking for less keeps the list readable. */
const SEARCH_PAGE = 30;
/** Quiet period before a keystroke becomes a gated search IPC. */
const SEARCH_DEBOUNCE_MS = 180;
/** The debounce key. One picker at a time, so one key is enough. */
const SEARCH_DEBOUNCE_KEY = "related-hierarchy-picker:search";

/** The synthetic top-level node's key — it is a presentation device, not a container. */
const UNCLASSIFIED_KEY = "u";
/** Backend-side label for the synthetic node; kept identical so breadcrumbs match the tree. */
const UNCLASSIFIED_LABEL = "Not classified";

/** What a row's primary/trailing action does when activated. */
type PickerLineAction =
  /** Nothing actionable (a locked container, an already-linked row, the "Not classified" node). */
  | "none"
  /** Link this leaf immediately — one call, then close. */
  | "linkLeaf"
  /** Open the confirmation for linking this whole Space/folder as ONE relation. */
  | "linkContainer"
  /** Load an earlier / further page of the enclosing group. */
  | "page";

/** The small trailing tag a row can carry. */
type PickerLineStatus = "current" | "linked" | "contains" | "locked" | null;

/**
 * ONE rendered row of the picker, fully resolved in a `computed` so the template binds FIELDS only
 * (the zoneless computed-only rule) and the keyboard model has a single flat list to traverse.
 */
interface PickerLine {
  /** Stable identity — the `@for` track key, the roving-tabindex key and the DOM id suffix. */
  readonly key: string;
  /** The enclosing row's key, so Left can move to the parent without re-walking the tree. */
  readonly parentKey: string | null;
  /** 1-based `aria-level`. */
  readonly level: number;
  readonly label: string;
  /** Secondary line: a breadcrumb, a group count, or a lock hint. Empty ⇒ not rendered. */
  readonly meta: string;
  readonly glyph: ShellIcon;
  /** Token class suffix for the glyph tone (`meeting` | `note` | `document` | `space` | …). */
  readonly tone: string;
  /** Whether this row has a disclosure triangle at all. */
  readonly expandable: boolean;
  readonly expanded: boolean;
  readonly status: PickerLineStatus;
  readonly action: PickerLineAction;
  /** The endpoint a `linkLeaf` / `linkContainer` action creates a relation to. */
  readonly target: PickerTarget | null;
  /** Space vs folder, for the confirmation copy and the trailing button's label. */
  readonly containerLevel: ContainerLevel | null;
  /** For `action === "page"`: which scope/kind/offset to fetch. */
  readonly page: PickerPageRequest | null;
  /** True for the anchor's own row — never linkable, always the scroll target. */
  readonly isCurrent: boolean;
}

/** One endpoint the picker can link to. */
export interface PickerTarget {
  readonly kind: LinkKind;
  readonly id: string;
  readonly title: string;
}

/** A lazy page fetch a `Load earlier` / `Load more` row triggers. */
interface PickerPageRequest {
  readonly containerId: string | null;
  readonly kind: PickerItemKind;
  readonly offset: number;
}

/** One group's currently-loaded window, keyed `${scopeKey}|${kind}`. */
interface LoadedGroup {
  readonly offset: number;
  readonly items: readonly PickerRow[];
  readonly total: number;
}

/** The pending "link this whole place?" confirmation. */
interface PendingContainer {
  readonly id: string;
  readonly name: string;
  readonly level: ContainerLevel;
  readonly breadcrumb: string;
}

/** A local backend hit or a bounded hit projected from the freshly loaded Shared hierarchy. */
interface PickerSearchResult {
  readonly kind: LinkKind;
  readonly id: string;
  readonly title: string;
  readonly breadcrumb: readonly string[];
  readonly shared: boolean;
  /** Present only for local Space/folder hits. Shared containers are never search results. */
  readonly containerLevel: ContainerLevel | null;
}

/** A bounded client-side projection plus its unbounded match count for honest result copy. */
interface PickerSearchBucket {
  readonly hits: readonly PickerSearchResult[];
  readonly total: number;
}

/**
 * "Add related" — the OPAQUE modal hierarchy picker behind the Related panel's `+ Link` trigger.
 *
 * It replaces the flat autocomplete chooser (which could only ever answer "type a title you already
 * remember") with the shape of the vault: `Not classified`, each local Space and its folders, and
 * the Shared Brains — with recordings, notes and imported documents as the linkable leaves. Tasks
 * and dashboards never appear, because a relation to one is not a thing this product has.
 *
 * # What makes it usable on a large vault
 *
 * The BACKEND owns truth and location. Opening from item #150 does not ship items 1–149: the
 * bootstrap returns the ancestor path plus a bounded window CENTRED on the anchor, and everything
 * else pages lazily in both directions (`Load earlier` / `Load more`). Nothing here reads the
 * sidebar's cached, capped forest — a cached tree cannot answer "where is this item", and a capped
 * one would silently hide the second hundred.
 *
 * # Two kinds of pick
 *
 * A LEAF links immediately. A whole Space or folder can ALSO be linked — as exactly ONE relation to
 * that container's stable id, after a confirmation that says so in as many words. It never fans out
 * to descendants, which is why the disclosure triangle and the trailing `Link Space` / `Link folder`
 * button are separate focusable controls: expanding must never be able to create a relation.
 *
 * # Lock model
 *
 * Every read is the backend's, gated, and taken behind the process privacy-readiness barrier. On
 * relock, privacy invalidation, an org-feed event, an anchor change, a cached-tab detach or a close,
 * {@link scrub} bumps a monotonic epoch, cancels the pending search debounce and synchronously drops
 * every title-bearing signal — so a late bootstrap/page/search reply can never repaint content the
 * user is no longer entitled to see. The modal is teleported to `<body>`, which is exactly why the
 * tab-detach signal matters: a detached route keeps its component alive, and without that signal the
 * overlay would outlive the tab it belongs to.
 */
@Component({
  selector: "app-related-hierarchy-picker",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurIconComponent, TeleportToBodyDirective],
  templateUrl: "./related-hierarchy-picker.component.html",
  styleUrl: "./related-hierarchy-picker.component.scss",
})
export class RelatedHierarchyPickerComponent {
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);
  private readonly privacyBarrier = inject(AskHistoryPrivacyBarrierService);
  private readonly tabRouteReuse = inject(TabRouteReuseStrategy);
  private readonly debounce = inject(DebounceService);
  private readonly injector = inject(Injector);
  private readonly destroyRef = inject(DestroyRef);

  /** The item the new relation starts FROM. */
  readonly anchorKind = input.required<LinkKind>();
  readonly anchorId = input.required<string>();
  /** The anchor's own title, for the modal's subtitle. */
  readonly anchorTitle = input("");
  /**
   * `${kind}:${id}` of every endpoint that is ALREADY related, so those rows read `Linked` and
   * stay inactive. Supplied by the Related panel from its own gated `list_links` read — this
   * component never asks a second time, so it cannot disclose a relation the panel would not.
   */
  readonly linkedKeys = input<ReadonlySet<string>>(new Set<string>());
  /** True while the parent's `link_items` call is in flight. */
  readonly busy = input(false);

  /** A target was picked. The PARENT owns the write, the toast and the reload. */
  readonly picked = output<PickerTarget>();
  /** The modal wants to close (Escape / backdrop / Cancel / after a pick). */
  readonly closed = output<void>();

  private readonly dialogRef = viewChild<ElementRef<HTMLElement>>("dialog");
  private readonly searchRef =
    viewChild<ElementRef<HTMLInputElement>>("searchField");
  private readonly treeRef = viewChild<ElementRef<HTMLElement>>("tree");
  private readonly confirmDialogRef =
    viewChild<ElementRef<HTMLElement>>("confirmDialog");
  private readonly confirmCancelRef =
    viewChild<ElementRef<HTMLButtonElement>>("confirmCancel");

  // ── State ──────────────────────────────────────────────────────────────────────────────────────

  /** Top-level local Spaces (metadata only). */
  private readonly spaces = signal<readonly PickerContainerNode[]>([]);
  /** The synthetic "Not classified" node's groups. */
  private readonly unclassified = signal<readonly PickerGroup[]>([]);
  /** Fresh, picker-owned Shared forest. Never reuse sidebar cache for a privacy-bearing modal. */
  private readonly sharedWorkspace = signal<SharedWorkspace | null>(null);
  /** Where the anchor lives — drives `Current`, the initial expansion and the centring scroll. */
  private readonly anchorPath = signal<readonly string[]>([]);
  private readonly anchorScopeKey = signal<string | null>(null);
  private readonly anchorGroupKey = signal<string | null>(null);
  /** Breadcrumb for an org anchor, whose location is outside the local bootstrap. */
  private readonly sharedAnchorBreadcrumb = signal<readonly string[]>([]);
  /** Expanded row keys. Only the anchor's own path starts expanded. */
  private readonly expandedKeys = signal<ReadonlySet<string>>(
    new Set<string>(),
  );
  /** Loaded leaf windows, keyed `${scopeKey}|${kind}`. */
  private readonly loaded = signal<ReadonlyMap<string, LoadedGroup>>(new Map());
  /** Groups with a fetch in flight, so a scroll burst cannot queue duplicates. */
  private readonly inFlight = signal<ReadonlySet<string>>(new Set<string>());

  /** The live search text. */
  readonly query = signal("");
  /** Search results, or `null` while no search is active (the hierarchy is showing). */
  private readonly hits = signal<readonly PickerSearchResult[] | null>(null);
  private readonly hitsTotal = signal(0);
  /** True until the first bootstrap resolves. */
  readonly loading = signal(true);
  /** A refusal to surface instead of an empty tree (a locked anchor, a dead backend). */
  readonly error = signal<string | null>(null);

  /** The roving-tabindex row. Exactly one row is tabbable at a time. */
  readonly activeKey = signal<string | null>(null);
  /** The pending container confirmation, or `null`. */
  readonly pendingContainer = signal<PendingContainer | null>(null);

  /**
   * Monotonic invalidation token. Every async reply carries the epoch it was issued under and is
   * DROPPED when it no longer matches — which is what makes a relock, an org-feed event or a close
   * unable to be undone by a reply that was already in flight.
   */
  private epoch = 0;
  /** The tree's scroll position when a search started, restored when the search is cleared. */
  private savedScrollTop = 0;
  /** The row we last centred on, so re-centring is once-per-open rather than every render. */
  private centredForKey: string | null = null;
  /** Container row whose trailing action opened the confirmation. Content-free DOM identity. */
  private pendingOriginKey: string | null = null;
  /** Shared is readable only after this picker's own revocation channel is acknowledged. */
  private readonly orgFeedListenerReady = this.installOrgFeedListener();
  private orgFeedUnlisten: (() => void) | null = null;
  private destroyed = false;

  constructor() {
    // ── Invalidation wiring. Each of these is a moment at which the content on screen may no
    //    longer be the user's to see, and none of them is a component destroy. ──
    const unregisterPrivacy = this.privacyBarrier.registerInvalidator(() =>
      this.scrubAndClose(),
    );
    // A cached tab (`/notes/:id`, `/meeting/:id`, `/org-item/:id`) is DETACHED, not destroyed, when
    // the user navigates away — no lifecycle hook fires. Without this the body-teleported modal
    // would survive its own tab going into the background.
    const unregisterDetach = this.tabRouteReuse.onDetach(() =>
      this.scrubAndClose(),
    );
    this.destroyRef.onDestroy(() => {
      this.destroyed = true;
      unregisterPrivacy();
      unregisterDetach();
      this.orgFeedUnlisten?.();
      this.orgFeedUnlisten = null;
      this.debounce.cancel(SEARCH_DEBOUNCE_KEY);
      this.scrub();
    });
  }

  /**
   * Install the picker-owned Shared revocation listener and turn its acknowledgement into a read
   * barrier. A missing Tauri event host is NOT permission to read Shared: local hierarchy remains
   * useful, while Shared stays absent until there is a channel capable of revoking it.
   */
  private async installOrgFeedListener(): Promise<boolean> {
    try {
      const unlisten = await this.ipc.onOrgFeedUpdated(() =>
        this.scrubAndClose(),
      );
      if (this.destroyed) {
        unlisten();
        return false;
      }
      this.orgFeedUnlisten = unlisten;
      return true;
    } catch {
      return false;
    }
  }

  /**
   * Load the bootstrap for the current anchor, and RE-load it whenever the session lock tree
   * changes. Reading `folders.tree()` registers this effect as its dependent, exactly as
   * `graph.component`'s `_refetchOnLock` does — so a relock drops what is now sealed and an unlock
   * brings it back, live. A legitimate signal-writing effect (T1): it orchestrates an async IPC
   * fetch behind a stale-result guard.
   */
  private readonly _bootstrap = effect(() => {
    const kind = this.anchorKind();
    const id = this.anchorId();
    // Establish the lock-tree dependency; the backend is the authority on visibility, we just
    // re-ask when it may have changed.
    this.folders.tree();
    // A new anchor (or a lock transition) invalidates everything already on screen BEFORE the
    // replacement read starts — never a frame of the old vault under the new title.
    this.scrub();
    const epoch = this.epoch;
    this.loading.set(true);
    void this.load(kind, id, epoch);
  });

  private async load(kind: LinkKind, id: string, epoch: number): Promise<void> {
    // The process privacy barrier before any content-bearing IPC: a read must never race ahead of
    // the invalidation listeners that would scrub it.
    const ready = await this.privacyBarrier.ensureReady();
    if (epoch !== this.epoch) {
      return;
    }
    if (!ready) {
      this.loading.set(false);
      this.error.set("Related isn’t available securely right now.");
      return;
    }
    try {
      const [bootstrap, shared] = await Promise.all([
        this.ipc.getRelatedPickerBootstrap(kind, id),
        this.loadSharedWorkspace(epoch),
      ]);
      if (epoch !== this.epoch) {
        return;
      }
      this.spaces.set(bootstrap.spaces);
      this.unclassified.set(bootstrap.unclassified);
      this.sharedWorkspace.set(shared);
      const expanded = new Set<string>();
      const anchor = bootstrap.anchor;
      if (anchor) {
        const scopeKey = anchor.containerId ?? UNCLASSIFIED_KEY;
        for (const containerId of anchor.path) {
          expanded.add(`c:${containerId}`);
        }
        if (!anchor.containerId) {
          expanded.add(UNCLASSIFIED_KEY);
        }
        const groupKey = `g:${scopeKey}:${anchor.kind}`;
        expanded.add(groupKey);
        this.anchorPath.set(anchor.path);
        this.anchorScopeKey.set(scopeKey);
        this.anchorGroupKey.set(groupKey);
        this.loaded.set(
          new Map([
            [
              `${scopeKey}|${anchor.kind}`,
              {
                offset: anchor.offset,
                items: anchor.items,
                total: anchor.total,
              },
            ],
          ]),
        );
      } else if (kind === "org") {
        const sharedAnchor = this.findSharedAnchor(id);
        if (sharedAnchor) {
          for (const key of sharedAnchor.keys) {
            expanded.add(key);
          }
          this.sharedAnchorBreadcrumb.set(sharedAnchor.breadcrumb);
        }
      }
      this.expandedKeys.set(expanded);
      this.loading.set(false);
      this.centreOnCurrent();
    } catch (e) {
      if (epoch !== this.epoch) {
        return;
      }
      this.loading.set(false);
      // The backend refuses a sealed OR unknown anchor indistinguishably. Say so once, and show
      // nothing — never a partial tree beside a refusal.
      this.error.set(
        String(e).toLowerCase().includes("locked")
          ? "This item is locked. Unlock it to browse related items."
          : "Couldn’t open the related picker.",
      );
    }
  }

  /**
   * Read the FRESH Shared forest only behind this component's acknowledged revocation listener.
   * The epoch checks cover both sides of the IPC so an event or destroy can win against a late
   * registration/read without allowing its titles to repaint.
   */
  private async loadSharedWorkspace(
    epoch: number,
  ): Promise<SharedWorkspace | null> {
    const listenerReady = await this.orgFeedListenerReady;
    if (!listenerReady || epoch !== this.epoch) {
      return null;
    }
    try {
      const shared = await this.ipc.listSharedWorkspace();
      return epoch === this.epoch ? shared : null;
    } catch {
      return null;
    }
  }

  // ── Invalidation ───────────────────────────────────────────────────────────────────────────────

  /**
   * SYNCHRONOUSLY drop every title-bearing piece of state and invalidate every in-flight reply.
   *
   * Called on relock, privacy invalidation, an org-feed event, an anchor change, a cached-tab
   * detach, a close and destroy. The epoch bump is what makes it complete: a bootstrap, page or
   * search reply already on the wire is dropped on arrival rather than repainting content that has
   * just stopped being visible.
   */
  private scrub(): void {
    this.epoch += 1;
    this.debounce.cancel(SEARCH_DEBOUNCE_KEY);
    this.spaces.set([]);
    this.unclassified.set([]);
    this.sharedWorkspace.set(null);
    this.anchorPath.set([]);
    this.anchorScopeKey.set(null);
    this.anchorGroupKey.set(null);
    this.sharedAnchorBreadcrumb.set([]);
    this.expandedKeys.set(new Set<string>());
    this.loaded.set(new Map());
    this.inFlight.set(new Set<string>());
    this.hits.set(null);
    this.hitsTotal.set(0);
    this.query.set("");
    this.pendingContainer.set(null);
    this.pendingOriginKey = null;
    this.activeKey.set(null);
    this.error.set(null);
    this.centredForKey = null;
  }

  /** Scrub, then ask the host to unmount us. */
  private scrubAndClose(): void {
    this.scrub();
    this.closed.emit();
  }

  // ── Derived view model ─────────────────────────────────────────────────────────────────────────

  /** Whether a search is active (the hierarchy is replaced by results). */
  readonly searching = computed(() => this.query().trim().length > 0);

  /** Shared roots — received Spaces plus the virtual "Shared Brains" node. */
  private readonly sharedRoots = computed<readonly SharedContainerNode[]>(
    () => {
      const shared = this.sharedWorkspace();
      return shared ? [...shared.spaces, shared.sharedBrains] : [];
    },
  );

  /** The already-related keys, resolved once per render. */
  private readonly linked = computed(() => this.linkedKeys());

  /** `${kind}:${id}` — the identity the Related panel and this picker agree on. */
  private endpointKey(kind: LinkKind, id: string): string {
    return `${kind}:${id}`;
  }

  /** Is this row the anchor itself? */
  private isAnchor(kind: LinkKind, id: string): boolean {
    return kind === this.anchorKind() && id === this.anchorId();
  }

  /**
   * The FLAT list of visible rows — the tree, or the search results.
   *
   * Flat on purpose: the keyboard model traverses "visible rows" and a flat list makes Up/Down,
   * Home/End and the roving tabindex one index arithmetic each, instead of a recursive walk per
   * keystroke. Every per-row value (glyph, status, action, target) is resolved HERE so the template
   * binds fields only.
   */
  readonly lines = computed<readonly PickerLine[]>(() => {
    if (this.searching()) {
      return this.searchLines();
    }
    const out: PickerLine[] = [];
    this.pushUnclassified(out);
    for (const space of this.spaces()) {
      this.pushContainer(out, space, null, 1);
    }
    for (const root of this.sharedRoots()) {
      this.pushShared(out, root, null, 1);
    }
    return out;
  });

  /** The search-result rows. */
  private searchLines(): readonly PickerLine[] {
    const hits = this.hits();
    if (!hits) {
      return [];
    }
    const linked = this.linked();
    return hits.map((hit) => {
      const kind = hit.kind;
      const isAnchor = this.isAnchor(kind, hit.id);
      const isLinked = linked.has(this.endpointKey(kind, hit.id));
      const isContainer = kind === "container" && hit.containerLevel !== null;
      const containsCurrent =
        isContainer && this.anchorPath().includes(hit.id);
      return {
        key: `h:${hit.kind}:${hit.id}`,
        parentKey: null,
        level: 1,
        label: hit.title,
        meta: hit.breadcrumb.join(" / "),
        glyph: hit.shared
          ? "shared-brains"
          : isContainer
            ? hit.containerLevel === "project"
              ? "spaces"
              : "folder"
            : this.leafGlyph(hit.kind as PickerItemKind),
        tone: hit.shared
          ? "shared"
          : isContainer
            ? hit.containerLevel === "project"
              ? "space"
              : "folder"
            : hit.kind,
        expandable: false,
        expanded: false,
        status: isAnchor
          ? "current"
          : isLinked
            ? "linked"
            : containsCurrent
              ? "contains"
              : null,
        action:
          isAnchor || isLinked
            ? "none"
            : isContainer
              ? "linkContainer"
              : "linkLeaf",
        target:
          isAnchor || isLinked ? null : { kind, id: hit.id, title: hit.title },
        containerLevel: hit.containerLevel,
        page: null,
        isCurrent: isAnchor,
      } satisfies PickerLine;
    });
  }

  /** The synthetic "Not classified" node + its groups. Disclosure-only: never a link target. */
  private pushUnclassified(out: PickerLine[]): void {
    const groups = this.unclassified();
    if (groups.length === 0) {
      return;
    }
    const expanded = this.expandedKeys().has(UNCLASSIFIED_KEY);
    out.push({
      key: UNCLASSIFIED_KEY,
      parentKey: null,
      level: 1,
      label: UNCLASSIFIED_LABEL,
      meta: "",
      glyph: "browse",
      tone: "unclassified",
      expandable: true,
      expanded,
      status: null,
      // A synthetic node is not a place: there is no stable id to point a relation at.
      action: "none",
      target: null,
      containerLevel: null,
      page: null,
      isCurrent: false,
    });
    if (!expanded) {
      return;
    }
    for (const group of groups) {
      this.pushGroup(out, UNCLASSIFIED_KEY, UNCLASSIFIED_KEY, group, 2);
    }
  }

  /** One local container row, its groups and its child folders. */
  private pushContainer(
    out: PickerLine[],
    node: PickerContainerNode,
    parentKey: string | null,
    level: number,
  ): void {
    const key = `c:${node.id}`;
    const sealed = node.locked && !node.unlocked;
    const hasVisibleChildren =
      node.groups.length > 0 || node.folders.length > 0;
    const isLinked = this.linked().has(this.endpointKey("container", node.id));
    const containsCurrent = this.anchorPath().includes(node.id);
    out.push({
      key,
      parentKey,
      level,
      label: node.name,
      meta: sealed ? "Unlock it in the sidebar to browse or link" : "",
      glyph: node.level === "project" ? "spaces" : "folder",
      tone: node.level === "project" ? "space" : "folder",
      // A sealed container discloses its NAME and nothing else — so it has no disclosure either.
      expandable: !sealed && hasVisibleChildren,
      expanded: this.expandedKeys().has(key),
      status: sealed
        ? "locked"
        : isLinked
          ? "linked"
          : containsCurrent
            ? "contains"
            : null,
      action: sealed || isLinked || !node.linkable ? "none" : "linkContainer",
      target:
        sealed || isLinked || !node.linkable
          ? null
          : { kind: "container", id: node.id, title: node.name },
      containerLevel: node.level,
      page: null,
      isCurrent: false,
    });
    if (sealed || !hasVisibleChildren || !this.expandedKeys().has(key)) {
      return;
    }
    for (const group of node.groups) {
      this.pushGroup(out, node.id, key, group, level + 1);
    }
    for (const child of node.folders) {
      this.pushContainer(out, child, key, level + 1);
    }
  }

  /** One per-kind group inside a scope, plus its loaded window and its paging rows. */
  private pushGroup(
    out: PickerLine[],
    scopeKey: string,
    parentKey: string | null,
    group: PickerGroup,
    level: number,
  ): void {
    const key = `g:${scopeKey}:${group.kind}`;
    const expanded = this.expandedKeys().has(key);
    out.push({
      key,
      parentKey,
      level,
      label: this.groupLabel(group.kind),
      meta: String(group.total),
      glyph: this.leafGlyph(group.kind),
      tone: group.kind,
      expandable: true,
      expanded,
      status: null,
      action: "none",
      target: null,
      containerLevel: null,
      page: null,
      isCurrent: false,
    });
    if (!expanded) {
      return;
    }
    const containerId = scopeKey === UNCLASSIFIED_KEY ? null : scopeKey;
    const window = this.loaded().get(`${scopeKey}|${group.kind}`);
    if (!window) {
      return; // the fetch is in flight; the group renders empty for one frame
    }
    if (window.offset > 0) {
      out.push(
        this.pageLine(
          `${key}:earlier`,
          key,
          level + 1,
          "Load earlier",
          containerId,
          group.kind,
          Math.max(0, window.offset - PAGE_SIZE),
        ),
      );
    }
    const linked = this.linked();
    for (const item of window.items) {
      const kind = item.kind as LinkKind;
      const isAnchor = this.isAnchor(kind, item.id);
      const isLinked = linked.has(this.endpointKey(kind, item.id));
      out.push({
        key: `i:${item.kind}:${item.id}`,
        parentKey: key,
        level: level + 1,
        label: item.title,
        meta: "",
        glyph: this.leafGlyph(item.kind),
        tone: item.kind,
        expandable: false,
        expanded: false,
        status: isAnchor ? "current" : isLinked ? "linked" : null,
        action: isAnchor || isLinked ? "none" : "linkLeaf",
        target:
          isAnchor || isLinked
            ? null
            : { kind, id: item.id, title: item.title },
        containerLevel: null,
        page: null,
        isCurrent: isAnchor,
      });
    }
    const end = window.offset + window.items.length;
    if (end < window.total) {
      out.push(
        this.pageLine(
          `${key}:more`,
          key,
          level + 1,
          "Load more",
          containerId,
          group.kind,
          end,
        ),
      );
    }
  }

  private pageLine(
    key: string,
    parentKey: string,
    level: number,
    label: string,
    containerId: string | null,
    kind: PickerItemKind,
    offset: number,
  ): PickerLine {
    return {
      key,
      parentKey,
      level,
      label,
      meta: "",
      glyph: "history",
      tone: "page",
      expandable: false,
      expanded: false,
      status: null,
      action: "page",
      target: null,
      containerLevel: null,
      page: { containerId, kind, offset },
      isCurrent: false,
    };
  }

  /**
   * One received (Shared Brain) container and its items.
   *
   * A shared CONTAINER is disclosure-only: it is somebody else's place, and the relation model has
   * no stable local id for it. A shared LEAF links by the revision-stable `${orgId}:${docId}`
   * composite — and a row that has no `docId` (an older sender's client predates the field) is
   * simply NOT linkable, rather than linked by an id that will move under it.
   */
  private pushShared(
    out: PickerLine[],
    node: SharedContainerNode,
    parentKey: string | null,
    level: number,
  ): void {
    const key = `s:${node.orgId}:${node.containerId ?? "root"}`;
    const hasVisibleChildren = node.folders.length > 0 || node.items.length > 0;
    const expanded = this.expandedKeys().has(key);
    out.push({
      key,
      parentKey,
      level,
      label: node.name,
      meta: node.orgName,
      glyph: "shared-brains",
      tone: "shared",
      expandable: hasVisibleChildren,
      expanded,
      status: null,
      action: "none",
      target: null,
      containerLevel: null,
      page: null,
      isCurrent: false,
    });
    if (!hasVisibleChildren || !expanded) {
      return;
    }
    for (const child of node.folders) {
      this.pushShared(out, child, key, level + 1);
    }
    const linked = this.linked();
    for (const item of node.items) {
      const stableId = item.docId ? `${item.orgId}:${item.docId}` : null;
      const isLinked =
        stableId !== null && linked.has(this.endpointKey("org", stableId));
      const isAnchor = stableId !== null && this.isAnchor("org", stableId);
      out.push({
        key: `si:${item.itemId}`,
        parentKey: key,
        level: level + 1,
        label: item.title,
        meta: item.authorHint,
        glyph: "shared-brains",
        tone: "shared",
        expandable: false,
        expanded: false,
        status: isAnchor ? "current" : isLinked ? "linked" : null,
        action: stableId === null || isLinked || isAnchor ? "none" : "linkLeaf",
        target:
          stableId === null || isLinked || isAnchor
            ? null
            : { kind: "org", id: stableId, title: item.title },
        containerLevel: null,
        page: null,
        isCurrent: isAnchor,
      });
    }
  }

  /** Locate an org stable link id in the fresh Shared forest and return its ancestor rows. */
  private findSharedAnchor(
    stableId: string,
  ): { keys: string[]; breadcrumb: string[] } | null {
    const walk = (
      node: SharedContainerNode,
      keys: string[],
      breadcrumb: string[],
    ): { keys: string[]; breadcrumb: string[] } | null => {
      const key = `s:${node.orgId}:${node.containerId ?? "root"}`;
      const nextKeys = [...keys, key];
      const nextBreadcrumb = [...breadcrumb, node.name];
      if (
        node.items.some(
          (item) =>
            item.docId !== null &&
            item.docId !== undefined &&
            `${item.orgId}:${item.docId}` === stableId,
        )
      ) {
        return { keys: nextKeys, breadcrumb: nextBreadcrumb };
      }
      for (const child of node.folders) {
        const found = walk(child, nextKeys, nextBreadcrumb);
        if (found) {
          return found;
        }
      }
      return null;
    };
    for (const root of this.sharedRoots()) {
      const found = walk(root, [], []);
      if (found) {
        return found;
      }
    }
    return null;
  }

  /**
   * Search the already-loaded local hierarchy for linkable, visible places.
   *
   * A match may come from the place's own name OR its full visible breadcrumb. Sealed scopes are
   * absent rather than searchable by name: the hierarchy may disclose their explicit lock row,
   * but search must not turn that metadata into a content-location oracle.
   */
  private localContainerSearchResults(query: string): PickerSearchBucket {
    const needle = query.trim().toLocaleLowerCase();
    if (!needle) {
      return { hits: [], total: 0 };
    }
    const hits: PickerSearchResult[] = [];
    let total = 0;
    const walk = (
      nodes: readonly PickerContainerNode[],
      breadcrumb: readonly string[],
    ): void => {
      for (const node of nodes) {
        const sealed = node.locked && !node.unlocked;
        if (sealed) {
          continue;
        }
        const nextBreadcrumb = [...breadcrumb, node.name];
        const fullPath = nextBreadcrumb.join(" / ").toLocaleLowerCase();
        if (
          node.linkable &&
          (node.name.toLocaleLowerCase().includes(needle) ||
            fullPath.includes(needle))
        ) {
          total += 1;
          if (hits.length < SEARCH_PAGE) {
            hits.push({
              kind: "container",
              id: node.id,
              title: node.name,
              breadcrumb: nextBreadcrumb,
              shared: false,
              containerLevel: node.level,
            });
          }
        }
        walk(node.folders, nextBreadcrumb);
      }
    };
    walk(this.spaces(), []);
    return { hits, total };
  }

  /**
   * Bounded client projection over the FRESH Shared payload; containers remain disclosure-only.
   * A visible Shared breadcrumb is searchable, but only its stable, linkable leaves become hits.
   */
  private sharedSearchResults(query: string): PickerSearchBucket {
    const needle = query.trim().toLocaleLowerCase();
    if (!needle) {
      return { hits: [], total: 0 };
    }
    const hits: PickerSearchResult[] = [];
    let total = 0;
    const walk = (
      node: SharedContainerNode,
      breadcrumb: readonly string[],
    ): void => {
      const nextBreadcrumb = [...breadcrumb, node.name];
      const fullPath = nextBreadcrumb.join(" / ").toLocaleLowerCase();
      for (const item of node.items) {
        if (
          !item.docId ||
          (!item.title.toLocaleLowerCase().includes(needle) &&
            !fullPath.includes(needle))
        ) {
          continue;
        }
        total += 1;
        if (hits.length < SEARCH_PAGE) {
          hits.push({
            kind: "org",
            id: `${item.orgId}:${item.docId}`,
            title: item.title,
            breadcrumb: nextBreadcrumb,
            shared: true,
            containerLevel: null,
          });
        }
      }
      for (const child of node.folders) {
        walk(child, nextBreadcrumb);
      }
    };
    for (const root of this.sharedRoots()) {
      walk(root, []);
    }
    return { hits, total };
  }

  private leafGlyph(kind: PickerItemKind): ShellIcon {
    return kind === "meeting"
      ? "meetings"
      : kind === "note"
        ? "notes"
        : "document";
  }

  private groupLabel(kind: PickerItemKind): string {
    return kind === "meeting"
      ? "Recordings"
      : kind === "note"
        ? "Notes"
        : "Documents";
  }

  /** The "N results" / "Opened at …" context strip. */
  readonly contextLabel = computed(() => {
    if (this.searching()) {
      const total = this.hitsTotal();
      return `${total} result${total === 1 ? "" : "s"}`;
    }
    const path = this.anchorPath();
    if (path.length === 0) {
      const sharedPath = this.sharedAnchorBreadcrumb();
      if (sharedPath.length > 0) {
        return sharedPath.join(" / ");
      }
      return this.anchorScopeKey() === UNCLASSIFIED_KEY
        ? UNCLASSIFIED_LABEL
        : "";
    }
    return this.containerNamesFor(path).join(" / ");
  });

  /** Resolve container ids to their live names using the loaded hierarchy. */
  private containerNamesFor(ids: readonly string[]): string[] {
    const names = new Map<string, string>();
    const walk = (nodes: readonly PickerContainerNode[]): void => {
      for (const node of nodes) {
        names.set(node.id, node.name);
        walk(node.folders);
      }
    };
    walk(this.spaces());
    return ids.map((id) => names.get(id) ?? "");
  }

  /** Nothing to show at all — an empty vault, or a search with no matches. */
  readonly empty = computed(
    () => !this.loading() && !this.error() && this.lines().length === 0,
  );

  // ── Interaction ────────────────────────────────────────────────────────────────────────────────

  /** Focus the search field once the modal is in the DOM (zoneless-safe: no setTimeout). */
  private readonly _focusSearch = effect(() => {
    if (this.loading() || this.error()) {
      return;
    }
    afterNextRender(
      () => this.searchRef()?.nativeElement.focus({ preventScroll: true }),
      { injector: this.injector },
    );
  });

  /**
   * Scroll the anchor's row roughly to the centre of the tree — WITHOUT stealing focus, which stays
   * on the search field. `preventScroll` on the focus call and a `scrollTop` write (rather than
   * `scrollIntoView`, which would scroll the document too) are what keep it contained.
   */
  private centreOnCurrent(): void {
    const key = this.lines().find((line) => line.isCurrent)?.key ?? null;
    if (!key || key === this.centredForKey) {
      return;
    }
    this.centredForKey = key;
    afterNextRender(
      () => {
        const tree = this.treeRef()?.nativeElement;
        const row = tree?.querySelector<HTMLElement>(
          `[data-row="${CSS.escape(key)}"]`,
        );
        if (!tree || !row) {
          return;
        }
        tree.scrollTop = Math.max(
          0,
          row.offsetTop - tree.clientHeight / 2 + row.offsetHeight / 2,
        );
        this.activeKey.set(key);
      },
      { injector: this.injector },
    );
  }

  /** Toggle a row's disclosure. This can NEVER create a relation. */
  toggle(line: PickerLine): void {
    if (!line.expandable) {
      return;
    }
    const next = new Set(this.expandedKeys());
    if (next.has(line.key)) {
      next.delete(line.key);
    } else {
      next.add(line.key);
      this.ensureGroupLoaded(line);
    }
    this.expandedKeys.set(next);
    this.activeKey.set(line.key);
  }

  /** When a GROUP row expands, pull its first page if we have not already. */
  private ensureGroupLoaded(line: PickerLine): void {
    if (!line.key.startsWith("g:")) {
      return;
    }
    const [, scopeKey, kind] = line.key.split(":");
    const mapKey = `${scopeKey}|${kind}`;
    if (this.loaded().has(mapKey) || this.inFlight().has(mapKey)) {
      return;
    }
    void this.fetchPage(
      scopeKey === UNCLASSIFIED_KEY ? null : scopeKey,
      kind as PickerItemKind,
      0,
      "replace",
    );
  }

  /** Load an earlier or further page into an already-open group. */
  loadPage(line: PickerLine): void {
    if (!line.page) {
      return;
    }
    void this.fetchPage(
      line.page.containerId,
      line.page.kind,
      line.page.offset,
      "merge",
    );
  }

  /**
   * Fetch one page and merge it into the loaded window.
   *
   * `merge` keeps ONE contiguous window per group rather than a list of disjoint pages: because the
   * bootstrap CENTRES, `Load earlier` extends the window backwards and `Load more` forwards, and a
   * single `{offset, items}` pair is the honest representation of that. The epoch guard drops a
   * reply issued before an invalidation.
   */
  private async fetchPage(
    containerId: string | null,
    kind: PickerItemKind,
    offset: number,
    mode: "replace" | "merge",
  ): Promise<void> {
    const scopeKey = containerId ?? UNCLASSIFIED_KEY;
    const mapKey = `${scopeKey}|${kind}`;
    if (this.inFlight().has(mapKey)) {
      return;
    }
    this.inFlight.update((set) => new Set(set).add(mapKey));
    const epoch = this.epoch;
    try {
      const page = await this.ipc.listRelatedPickerItems(
        this.anchorKind(),
        this.anchorId(),
        containerId,
        kind,
        offset,
        PAGE_SIZE,
      );
      if (epoch !== this.epoch) {
        return;
      }
      this.loaded.update((map) => {
        const next = new Map(map);
        const existing = mode === "merge" ? map.get(mapKey) : undefined;
        if (!existing) {
          next.set(mapKey, {
            offset: page.offset,
            items: page.items,
            total: page.total,
          });
          return next;
        }
        const merged =
          page.offset < existing.offset
            ? {
                offset: page.offset,
                items: [
                  ...page.items.slice(0, existing.offset - page.offset),
                  ...existing.items,
                ],
                total: page.total,
              }
            : {
                offset: existing.offset,
                items: [...existing.items, ...page.items],
                total: page.total,
              };
        next.set(mapKey, merged);
        return next;
      });
    } catch {
      // A refused (sealed) scope simply stays empty — the row already reads "Locked".
    } finally {
      if (epoch === this.epoch) {
        this.inFlight.update((set) => {
          const next = new Set(set);
          next.delete(mapKey);
          return next;
        });
      }
    }
  }

  /** The search field changed → debounce a gated backend search. */
  onQueryInput(event: Event): void {
    const value = (event.target as HTMLInputElement).value;
    const wasSearching = this.searching();
    this.query.set(value);
    if (!wasSearching && value.trim()) {
      // Remember where the hierarchy was, so clearing the search restores it exactly.
      this.savedScrollTop = this.treeRef()?.nativeElement.scrollTop ?? 0;
    }
    if (!value.trim()) {
      this.debounce.cancel(SEARCH_DEBOUNCE_KEY);
      this.hits.set(null);
      this.hitsTotal.set(0);
      this.activeKey.set(null);
      this.restoreTreeScroll();
      return;
    }
    const epoch = this.epoch;
    this.debounce.schedule(
      SEARCH_DEBOUNCE_KEY,
      () => void this.runSearch(value, epoch),
      SEARCH_DEBOUNCE_MS,
    );
  }

  private async runSearch(value: string, epoch: number): Promise<void> {
    if (epoch !== this.epoch) {
      return;
    }
    try {
      const page = await this.ipc.searchRelatedPicker(
        this.anchorKind(),
        this.anchorId(),
        value,
        0,
        SEARCH_PAGE,
      );
      if (epoch !== this.epoch || this.query().trim() !== value.trim()) {
        return;
      }
      const containers = this.localContainerSearchResults(value);
      const shared = this.sharedSearchResults(value);
      const local: PickerSearchResult[] = page.hits.map((hit) => ({
        ...hit,
        shared: false,
        containerLevel: null,
      }));
      // Local places are the primary navigation affordance, while a small Shared reservation keeps
      // visible Shared breadcrumb matches discoverable even when the backend returns a full local
      // leaf page. Every source is bounded and the displayed total counts ALL known matches.
      const sharedReserve = Math.min(6, shared.hits.length);
      const containerLimit = Math.min(
        containers.hits.length,
        SEARCH_PAGE - sharedReserve,
      );
      const localLimit = Math.min(
        local.length,
        SEARCH_PAGE - sharedReserve - containerLimit,
      );
      // If local places/leaves did not use the full page, Shared may consume the remainder; the
      // six-row value above is a reservation, not an artificial cap.
      const sharedLimit = Math.min(
        shared.hits.length,
        SEARCH_PAGE - containerLimit - localLimit,
      );
      this.hits.set([
        ...containers.hits.slice(0, containerLimit),
        ...local.slice(0, localLimit),
        ...shared.hits.slice(0, sharedLimit),
      ]);
      this.hitsTotal.set(page.total + containers.total + shared.total);
      const tree = this.treeRef()?.nativeElement;
      if (tree) {
        tree.scrollTop = 0;
      }
    } catch {
      if (epoch === this.epoch) {
        this.hits.set([]);
        this.hitsTotal.set(0);
      }
    }
  }

  /** Clear the search and restore the hierarchy AND the tree scroll position it had. */
  clearSearch(): void {
    this.debounce.cancel(SEARCH_DEBOUNCE_KEY);
    this.query.set("");
    this.hits.set(null);
    this.hitsTotal.set(0);
    this.activeKey.set(null);
    this.restoreTreeScroll();
    afterNextRender(
      () => this.searchRef()?.nativeElement.focus({ preventScroll: true }),
      { injector: this.injector },
    );
  }

  private restoreTreeScroll(): void {
    const top = this.savedScrollTop;
    afterNextRender(
      () => {
        const tree = this.treeRef()?.nativeElement;
        if (tree) {
          tree.scrollTop = top;
        }
      },
      { injector: this.injector },
    );
  }

  /** Activate a row's primary action. */
  activate(line: PickerLine): void {
    if (this.busy()) {
      return;
    }
    switch (line.action) {
      case "linkLeaf":
        if (line.target) {
          this.picked.emit(line.target);
        }
        break;
      case "linkContainer":
        // A tree row's body remains hierarchy navigation. A search result has no disclosure, so
        // activating its listbox option opens the SAME explicit one-edge confirmation as its
        // trailing action.
        if (this.searching()) {
          this.openContainerConfirm(line);
        } else {
          this.toggle(line);
        }
        break;
      case "page":
        this.loadPage(line);
        break;
      case "none":
        // A container/group row's BODY discloses; only its trailing button links.
        this.toggle(line);
        break;
    }
  }

  /** The trailing `Link Space` / `Link folder` button — separate from the disclosure, by design. */
  openContainerConfirm(line: PickerLine): void {
    if (!line.target || !line.containerLevel || this.busy()) {
      return;
    }
    this.pendingOriginKey = line.key;
    this.pendingContainer.set({
      id: line.target.id,
      name: line.target.title,
      level: line.containerLevel,
      breadcrumb: this.containerNamesFor(
        this.pathToContainer(line.target.id),
      ).join(" / "),
    });
    afterNextRender(() => this.confirmCancelRef()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

  /** The ancestor chain of a container id, root-first, from the loaded hierarchy. */
  private pathToContainer(id: string): string[] {
    const walk = (
      nodes: readonly PickerContainerNode[],
      trail: string[],
    ): string[] | null => {
      for (const node of nodes) {
        const next = [...trail, node.id];
        if (node.id === id) {
          return next;
        }
        const found = walk(node.folders, next);
        if (found) {
          return found;
        }
      }
      return null;
    };
    return walk(this.spaces(), []) ?? [id];
  }

  /** The confirmed container link — ONE relation to the place, never to its contents. */
  confirmContainer(): void {
    const pending = this.pendingContainer();
    if (!pending || this.busy()) {
      return;
    }
    this.pendingContainer.set(null);
    this.pendingOriginKey = null;
    this.picked.emit({
      kind: "container",
      id: pending.id,
      title: pending.name,
    });
  }

  cancelContainer(): void {
    this.pendingContainer.set(null);
    const originKey = this.pendingOriginKey;
    this.pendingOriginKey = null;
    if (originKey) {
      afterNextRender(
        () =>
          this.treeRef()
            ?.nativeElement.querySelector<HTMLElement>(
              `[data-row="${CSS.escape(originKey)}"] .rhp-action`,
            )
            ?.focus(),
        { injector: this.injector },
      );
    }
  }

  /** The noun the confirmation copy uses. */
  readonly pendingNoun = computed(() =>
    this.pendingContainer()?.level === "project" ? "Space" : "folder",
  );

  /** Backdrop click closes; a click that started inside does not. */
  onBackdropPointerDown(event: MouseEvent): void {
    if (event.target === event.currentTarget) {
      this.scrubAndClose();
    }
  }

  /** Cancel / the header ✕ / a final Escape. */
  close(): void {
    this.scrubAndClose();
  }

  // ── Keyboard ───────────────────────────────────────────────────────────────────────────────────

  /**
   * The whole modal's key handling, on the dialog host.
   *
   * Escape closes the CONFIRMATION first when one is open, then the modal — matching the approved
   * prototype, where Escape never doubled as "clear the search" (the search has its own visible ✕,
   * and stealing Escape for it costs the user the one key that reliably dismisses a dialog).
   * Tab/Shift+Tab are trapped between the first and last focusable control.
   */
  onDialogKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      if (this.pendingContainer()) {
        this.cancelContainer();
      } else {
        this.close();
      }
      return;
    }
    if (event.key === "Tab") {
      this.trapTab(event);
    }
  }

  private trapTab(event: KeyboardEvent): void {
    // While the nested confirmation is open, controls visually behind it are
    // inert from the user's perspective. Keep the focus loop inside the alertdialog.
    const root = this.pendingContainer()
      ? this.confirmDialogRef()?.nativeElement
      : this.dialogRef()?.nativeElement;
    if (!root) {
      return;
    }
    const focusable = Array.from(
      root.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])',
      ),
    ).filter((el) => el.offsetParent !== null || el === document.activeElement);
    if (focusable.length === 0) {
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const active = document.activeElement as HTMLElement | null;
    if (
      event.shiftKey &&
      (active === first || !active || !root.contains(active))
    ) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    }
  }

  /** ↓ from the search field enters the first visible row. */
  onSearchKeydown(event: KeyboardEvent): void {
    if (event.key !== "ArrowDown") {
      return;
    }
    const first = this.lines()[0];
    if (!first) {
      return;
    }
    event.preventDefault();
    this.focusRow(first.key);
  }

  /** Tree traversal on the focused row. */
  onRowKeydown(event: KeyboardEvent, line: PickerLine): void {
    const rows = this.lines();
    const index = rows.findIndex((row) => row.key === line.key);
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        this.focusRow(rows[Math.min(index + 1, rows.length - 1)]?.key);
        break;
      case "ArrowUp":
        event.preventDefault();
        if (index === 0) {
          this.searchRef()?.nativeElement.focus();
          this.activeKey.set(null);
        } else {
          this.focusRow(rows[index - 1]?.key);
        }
        break;
      case "ArrowRight":
        event.preventDefault();
        if (line.expandable && !line.expanded) {
          this.toggle(line);
        } else if (line.expandable) {
          this.focusRow(rows[index + 1]?.key);
        }
        break;
      case "ArrowLeft":
        event.preventDefault();
        if (line.expandable && line.expanded) {
          this.toggle(line);
        } else if (line.parentKey) {
          this.focusRow(line.parentKey);
        }
        break;
      case "Home":
        event.preventDefault();
        this.focusRow(rows[0]?.key);
        break;
      case "End":
        event.preventDefault();
        this.focusRow(rows[rows.length - 1]?.key);
        break;
      case "Enter":
      case " ":
        event.preventDefault();
        this.activate(line);
        break;
    }
  }

  /** Move the roving tabindex and the DOM focus to one row. */
  private focusRow(key: string | undefined): void {
    if (!key) {
      return;
    }
    this.activeKey.set(key);
    afterNextRender(
      () =>
        this.treeRef()
          ?.nativeElement.querySelector<HTMLElement>(
            `[data-row="${CSS.escape(key)}"] .rhp-row-main`,
          )
          ?.focus(),
      { injector: this.injector },
    );
  }

  /** The row that currently owns the single tab stop. */
  readonly tabbableKey = computed(
    () => this.activeKey() ?? this.lines()[0]?.key ?? null,
  );

  /** Clicking a row body focuses it, so the roving tabindex follows the pointer. */
  onRowFocus(line: PickerLine): void {
    this.activeKey.set(line.key);
  }
}
