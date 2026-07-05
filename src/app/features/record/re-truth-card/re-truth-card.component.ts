import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  input,
  signal,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import { FoldersService } from "../../../services/folders.service";
import type { ApplyResult, SupersessionDto } from "../../../core/models";

/**
 * Re-Truth — "the vault heals itself" (post-note surface, Tier-4-style reveal).
 * After a note lands, this card surfaces the facts the just-finished meeting
 * SUPERSEDES from earlier notes: your vault "moved on" — an entity's predicate
 * changed, and the note that first recorded the old value is now stale. One tap
 * ("Heal my vault") APPENDS an Obsidian `[!superseded]` callout to each stale
 * source note — append-only and fully reversible via Undo. We never edit the
 * user's own prose line; the callout is an additive stamp.
 *
 * HONEST: no card when the preview returns zero supersessions (like
 * {@link BrainRevealCardComponent}) — the vault only announces a heal it can
 * actually offer. The preview / apply / undo all land in SIGNALS via a tracked
 * `effect` with a stale-result guard (mirrors `entity-detail`'s `_load`) — never
 * a `.then()`/`.subscribe()` into a field.
 *
 * Sits IN-FLOW in the record screen's post-note region (not floating over
 * content), so the translucent `.card` surface + `--accent-soft`/`--success-soft`
 * accent blocks are correct here; the opaque `--surface-overlay` rule (trap T3)
 * applies only to overlays — same rationale as {@link BrainRevealCardComponent}.
 *
 * PRIVACY (lock-model): a supersession whose SOURCE note is sealed is skipped by
 * the backend (`ApplyResult.skippedSealed`), surfaced honestly in the healed
 * banner; `sourceNotePath` is backend-only and never rendered.
 */
@Component({
  selector: "app-re-truth-card",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./re-truth-card.component.html",
  styleUrl: "./re-truth-card.component.scss",
})
export class ReTruthCardComponent {
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);

  /**
   * Fires the preview only when the post-note surface is live — the record
   * screen binds `!!store.lastNote()` (set after `stopRecording()` resolves).
   * While false, the effect skips the fetch entirely, so a sealed meeting never
   * triggers a read behind the lock.
   */
  readonly active = input(false);
  /** The just-finalized meeting whose supersessions we preview. */
  readonly meetingId = input<string | null>(null);

  private readonly _items = signal<SupersessionDto[]>([]);
  private readonly _loaded = signal(false);
  /** Per-supersession selection (all selected by default); keyed by row id. */
  private readonly _selected = signal<Record<string, boolean>>({});
  /** The healed result (null = not yet healed → still offering the heal). */
  private readonly _healed = signal<ApplyResult | null>(null);
  /** Ids stamped by the last heal — the exact set Undo restores. */
  private readonly _appliedIds = signal<string[]>([]);
  /** True while an apply/undo IPC call is in flight. */
  private readonly _busy = signal(false);
  private readonly _error = signal<string | null>(null);

  readonly items = this._items.asReadonly();
  readonly healed = this._healed.asReadonly();
  readonly busy = this._busy.asReadonly();
  readonly error = this._error.asReadonly();

  /**
   * Reveal only when active, the preview has resolved, and something actually
   * moved on — honest: no card on an empty preview, and hidden until the fetch
   * resolves (no flash of an empty heal offer).
   */
  readonly show = computed(
    () => this.active() && this._loaded() && this._items().length > 0,
  );

  readonly count = computed(() => this._items().length);

  readonly headline = computed(() => {
    const n = this.count();
    return n === 1
      ? "This meeting changed 1 thing you'd written before."
      : `This meeting changed ${n} things you'd written before.`;
  });

  /** Ids currently checked, in list order — what "Heal my vault" applies. */
  readonly selectedIds = computed(() => {
    const sel = this._selected();
    return this._items()
      .filter((it) => sel[it.id])
      .map((it) => it.id);
  });
  readonly selectedCount = computed(() => this.selectedIds().length);

  readonly stampedLabel = computed(() => {
    const n = this._healed()?.applied ?? 0;
    return n === 1 ? "1 note" : `${n} notes`;
  });

  readonly skippedLabel = computed(() => {
    const n = this._healed()?.skippedSealed ?? 0;
    return n === 1 ? "1 sealed note" : `${n} sealed notes`;
  });

  /**
   * Preview the supersessions when the surface goes live, and re-preview whenever
   * the folder lock-state changes (a session unlock/relock shifts what is sealed)
   * — mirrors BrainRevealCardComponent / GraphComponent. `fetch()` writes the
   * item/loaded signals (async), so this tracked effect is allowed to write
   * (NG0600 guard). The `active`/`meetingId` guard runs FIRST, so no preview is
   * dispatched behind a lock or before a note.
   */
  private readonly _load = effect(
    () => {
      const id = this.meetingId();
      if (!this.active() || !id) {
        return;
      }
      this.folders.tree();
      void this.fetch(id);
    },
    { allowSignalWrites: true },
  );

  private async fetch(id: string): Promise<void> {
    try {
      const rows = await this.ipc.previewSupersessions(id);
      // Drop a response that resolved after the meeting moved on (stale guard).
      if (this.meetingId() !== id) {
        return;
      }
      this._items.set(rows);
      // Fresh preview → select all by default and reset any prior heal state.
      const sel: Record<string, boolean> = {};
      for (const r of rows) {
        sel[r.id] = true;
      }
      this._selected.set(sel);
      this._healed.set(null);
      this._appliedIds.set([]);
      this._error.set(null);
    } catch {
      if (this.meetingId() !== id) {
        return;
      }
      this._items.set([]);
    } finally {
      if (this.meetingId() === id) {
        this._loaded.set(true);
      }
    }
  }

  /** Is this supersession row checked? (pure lookup — reactive on `_selected`). */
  isSelected(id: string): boolean {
    return this._selected()[id] ?? false;
  }

  /** Toggle one row's inclusion in the heal. */
  toggle(id: string): void {
    this._selected.update((m) => ({ ...m, [id]: !(m[id] ?? false) }));
  }

  /** Where the new value came from — the superseding note, or "this meeting" when sealed. */
  supersedingLabel(it: SupersessionDto): string {
    return it.supersedingNoteTitle ?? "this meeting";
  }

  /**
   * Heal the vault: stamp the selected stale notes with an append-only
   * `[!superseded]` callout. On success we keep the applied ids so the button
   * swaps to Undo. All state lands in signals.
   */
  async heal(): Promise<void> {
    const ids = this.selectedIds();
    if (ids.length === 0 || this._busy()) {
      return;
    }
    this._busy.set(true);
    this._error.set(null);
    try {
      const res = await this.ipc.applySupersessions(ids);
      this._appliedIds.set(ids);
      this._healed.set(res);
    } catch (e) {
      this._error.set(String(e));
    } finally {
      this._busy.set(false);
    }
  }

  /** Undo the heal — remove the appended callouts, restoring the originals. */
  async undo(): Promise<void> {
    const ids = this._appliedIds();
    if (ids.length === 0 || this._busy()) {
      return;
    }
    this._busy.set(true);
    this._error.set(null);
    try {
      await this.ipc.undoSupersessions(ids);
      this._healed.set(null);
      this._appliedIds.set([]);
    } catch (e) {
      this._error.set(String(e));
    } finally {
      this._busy.set(false);
    }
  }
}
