import { DestroyRef, Injectable, effect, inject, signal } from "@angular/core";
import { type NavigationExtras, Router } from "@angular/router";
import { IpcService } from "./ipc.service";
import { NavHistoryService } from "./nav-history.service";
import { TabRouteReuseStrategy } from "./tab-route-reuse.strategy";
import { type TabKind, tabKeyFor } from "./tab-keys";

/** One open browser-style "document" tab (a meeting, a note, or an org-shared item). */
export interface Tab {
  /** Stable identity — SAME string as the `TabRouteReuseStrategy` cache key. */
  readonly id: string;
  readonly kind: TabKind;
  readonly entityId: string;
  /** The tab-strip label — a placeholder until the page's own load resolves it. */
  readonly title: string;
  readonly route: readonly string[];
}

const STORAGE_KEY = "murmur.tabs.v1";

interface PersistedTabsState {
  tabs: Tab[];
  activeTabId: string | null;
}

function isValidTab(v: unknown): v is Tab {
  if (!v || typeof v !== "object") {
    return false;
  }
  const t = v as Record<string, unknown>;
  return (
    typeof t["id"] === "string" &&
    (t["kind"] === "meeting" || t["kind"] === "note" || t["kind"] === "org-item") &&
    typeof t["entityId"] === "string" &&
    typeof t["title"] === "string" &&
    Array.isArray(t["route"]) &&
    (t["route"] as unknown[]).every((s) => typeof s === "string")
  );
}

/**
 * Single source of truth for Murmur's open document tabs (meetings + notes),
 * following the existing per-feature localStorage-service convention
 * (`theme.service.ts` / `glass.service.ts` / `chrome.service.ts` — there is no
 * shared persistence wrapper in this codebase, so this doesn't invent one).
 *
 * Pairs with `TabRouteReuseStrategy`, which keeps a closed-over tab's
 * component instance alive-but-detached so switching back is instant with no
 * lost state; `closeTab` is what actually evicts (destroys) it.
 */
@Injectable({ providedIn: "root" })
export class TabsService {
  private readonly router = inject(Router);
  private readonly tabRouteReuse = inject(TabRouteReuseStrategy);
  private readonly navHistory = inject(NavHistoryService);
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);

  private readonly _tabs = signal<Tab[]>(this.restoreTabs());
  private readonly _activeTabId = signal<string | null>(this.restoreActiveTabId());

  /** The open tabs, in open order. */
  readonly tabs = this._tabs.asReadonly();
  /** The currently-active tab's id, or `null` when no tab is open. */
  readonly activeTabId = this._activeTabId.asReadonly();

  /**
   * Persist on every change. This effect only READS signals (`_tabs`,
   * `_activeTabId`) and writes to `localStorage`, never to a signal — so
   * there is no T1 (signal-write-in-effect) concern here at all.
   */
  private readonly _persist = effect(() => {
    const state: PersistedTabsState = {
      tabs: this._tabs(),
      activeTabId: this._activeTabId(),
    };
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
    } catch {
      // Private-mode / storage-disabled — tabs simply won't survive a restart.
    }
  });

  /**
   * DELETE FAN-OUT FIX (2026-07-15): subscribed ONCE here (a root singleton
   * lives for the app's lifetime — mirrors `OrgBrainService`'s constructor
   * subscription) so a note/meeting deleted from ANY surface closes its stale
   * tab everywhere else too, instead of leaving a clickable tab that would
   * later 404. Content-free payload (id + kind only); a no-op when no
   * matching tab is open.
   */
  private feedUnlisten: (() => void) | null = null;
  private feedDestroyed = false;

  constructor() {
    this.destroyRef.onDestroy(() => {
      // A root service is never actually destroyed in practice (it outlives every
      // component), but honor the contract in case a test harness tears it down.
      this.feedDestroyed = true;
      this.feedUnlisten?.();
    });
    void this.ipc
      .onContentDeleted((p) => {
        void this.closeTab(tabKeyFor(p.kind, p.id));
      })
      .then((un) => {
        if (this.feedDestroyed) {
          un();
        } else {
          this.feedUnlisten = un;
        }
      })
      .catch(() => {
        /* best-effort: no Tauri host (e.g. plain browser) → no live fan-out */
      });
  }

  /** Open (or activate, if already open) a meeting tab and navigate to it. */
  async openMeeting(id: string, title = "Meeting"): Promise<void> {
    await this.openTab("meeting", id, title, ["/meeting", id]);
  }

  /**
   * Open (or activate) a note tab and navigate to it. `extra` forwards Router
   * navigation options — the `/notes/new` → `/notes/:id` create flow
   * (`NoteEditorComponent.createAndOpen`) passes `{ replaceUrl: true }` so the
   * placeholder URL never lingers in history.
   */
  async openNote(
    id: string,
    title = "Note",
    extra?: NavigationExtras,
  ): Promise<void> {
    await this.openTab("note", id, title, ["/notes", id], extra);
  }

  /**
   * Open (or activate) a read-only org (Shared Brain) item's tab — added
   * 2026-07-12: previously an org item had no tab at all and opened as a
   * full-page navigation, unlike its owned meeting/note counterparts.
   */
  async openOrgItem(id: string, title = "Shared note"): Promise<void> {
    await this.openTab("org-item", id, title, ["/org-item", id]);
  }

  private async openTab(
    kind: TabKind,
    entityId: string,
    title: string,
    route: readonly string[],
    extra?: NavigationExtras,
  ): Promise<void> {
    const id = tabKeyFor(kind, entityId);
    if (!this._tabs().some((t) => t.id === id)) {
      this._tabs.update((list) => [
        ...list,
        { id, kind, entityId, title, route: [...route] },
      ]);
    }
    this._activeTabId.set(id);
    await this.router.navigate([...route], extra);
  }

  /** Activate an already-open tab (a tab-strip click) and navigate to its route. */
  activate(id: string): void {
    const tab = this._tabs().find((t) => t.id === id);
    if (!tab) {
      return;
    }
    this._activeTabId.set(id);
    void this.router.navigate([...tab.route]);
  }

  /**
   * Rename a tab's label in place — called once the tab's own meeting/note
   * load resolves (mirrors how a browser tab adopts the page title). A no-op
   * if the tab isn't open (e.g. the page was reached via a routerLink that
   * doesn't go through this service) or `title` is empty.
   */
  setTitle(id: string, title: string): void {
    if (!title) {
      return;
    }
    this._tabs.update((list) =>
      list.map((t) => (t.id === id && t.title !== title ? { ...t, title } : t)),
    );
  }

  /**
   * Close a tab: drop it from the list, evict its cached detached route
   * instance (actually destroys it — see `TabRouteReuseStrategy.evict`), and,
   * if it was the active tab, activate a neighbor or fall back to the last
   * non-drill-down app route (mirrors a browser closing its last tab).
   */
  async closeTab(id: string): Promise<void> {
    const tabs = this._tabs();
    const idx = tabs.findIndex((t) => t.id === id);
    if (idx === -1) {
      return;
    }
    const wasActive = this._activeTabId() === id;
    const next = tabs.filter((t) => t.id !== id);
    this._tabs.set(next);
    this.tabRouteReuse.evict(id);
    if (!wasActive) {
      return;
    }
    if (next.length > 0) {
      const neighbor = next[Math.min(idx, next.length - 1)];
      this._activeTabId.set(neighbor.id);
      await this.router.navigate([...neighbor.route]);
    } else {
      this._activeTabId.set(null);
      await this.router.navigateByUrl(this.navHistory.lastAppRoute());
    }
  }

  /**
   * The persisted active tab's route, if any — consulted ONCE at boot
   * (`AppComponent.ngOnInit`) to restore the last session's active tab.
   */
  restoredRoute(): readonly string[] | null {
    const id = this._activeTabId();
    return this._tabs().find((t) => t.id === id)?.route ?? null;
  }

  private restoreTabs(): Tab[] {
    return this.readPersisted()?.tabs.filter(isValidTab) ?? [];
  }

  private restoreActiveTabId(): string | null {
    return this.readPersisted()?.activeTabId ?? null;
  }

  private readPersisted(): PersistedTabsState | null {
    try {
      const raw = localStorage.getItem(STORAGE_KEY);
      if (!raw) {
        return null;
      }
      const parsed = JSON.parse(raw) as Partial<PersistedTabsState>;
      if (!Array.isArray(parsed.tabs)) {
        return null;
      }
      return {
        tabs: parsed.tabs,
        activeTabId:
          typeof parsed.activeTabId === "string" ? parsed.activeTabId : null,
      };
    } catch {
      return null;
    }
  }
}
