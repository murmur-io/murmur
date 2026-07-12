import { Injectable, signal } from "@angular/core";
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
 */
@Injectable({ providedIn: "root" })
export class MeetingsListStore {
  readonly meetings = signal<Meeting[]>([]);
  readonly loading = signal(true);
  readonly tags = signal<string[]>([]);
}
