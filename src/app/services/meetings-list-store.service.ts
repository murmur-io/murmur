import { DestroyRef, Injectable, inject, signal } from "@angular/core";
import { IpcService } from "../core/ipc.service";
import type { Meeting } from "../core/models";

/**
 * Root-persisted backing signals for {@link LibraryComponent}'s no-query
 * meetings list — split out from the component itself so the DATA survives a
 * destroy+recreate (e.g. leaving `/library` to open a meeting, then coming
 * back): a component-local `signal<Meeting[]>([])` is wiped to empty on every
 * remount, forcing a full reload-from-blank flash. A root service instance
 * outlives the component, so the list renders with the LAST-KNOWN rows
 * INSTANTLY on return while `LibraryComponent.ngOnInit`'s existing reload
 * (unchanged — still a real refetch every visit, not a "skip if ever loaded"
 * cache) quietly replaces it underneath.
 *
 * Deliberately a thin signal holder, NOT a service with its own load()/CRUD
 * methods: `LibraryComponent` owns the orchestration (folder/tag filtering,
 * drag-drop patches, delete pruning, the tree-reactive reload effect) — that
 * logic is unchanged, it now just reads/writes THESE signals instead of
 * component-local ones. See the pattern note in `angular-zoneless.md` §9.
 *
 * DELETE FAN-OUT FIX (2026-07-15) SAFETY NET: the ONE exception to "no methods
 * of its own" is this passive constructor subscription (mirrors
 * `OrgBrainService`) — a meeting deleted from a DIFFERENT surface than
 * `LibraryComponent` (e.g. its own detail tab) still prunes THIS root-persisted
 * list, since only a root singleton outlives that component's destroy+recreate
 * cycle. It is pure pruning, not orchestration — `LibraryComponent`'s own
 * `confirmDelete` pruning is unchanged and layered on top of this, not replaced.
 */
@Injectable({ providedIn: "root" })
export class MeetingsListStore {
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);

  readonly meetings = signal<Meeting[]>([]);
  readonly loading = signal(true);
  readonly tags = signal<string[]>([]);

  private feedUnlisten: (() => void) | null = null;
  private feedDestroyed = false;

  constructor() {
    this.destroyRef.onDestroy(() => {
      this.feedDestroyed = true;
      this.feedUnlisten?.();
    });
    void this.ipc
      .onContentDeleted((p) => {
        if (p.kind !== "meeting") {
          return;
        }
        this.meetings.update((list) => list.filter((m) => m.id !== p.id));
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
}
