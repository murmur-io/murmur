import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import { IpcService } from "../../../core/ipc.service";
import type { UserMemory, UserMemoryFact } from "../../../core/models";
import { FoldersService } from "../../../services/folders.service";
import { ToastService } from "../../../services/toast.service";
import { MemoryImportComponent } from "../memory-import/memory-import.component";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";

/**
 * "What the brain knows about you" — the user-memory audit section on the Brain
 * page. Lists the persisted user-memory FACTS (subject/predicate/object +
 * provenance) that the brain injects into grounding, each with a per-fact
 * "Forget", plus a "Clear all". Data + mutations go through the Phase-3 memory
 * commands ({@link IpcService.getUserMemory} / `forgetUserFact` / `clearUserMemory`).
 *
 * Signals-first + OnPush. GATED server-side: `get_user_memory` only returns facts
 * whose SOURCE meeting is visible under the live unlocked snapshot, so a
 * lock-state change (`folders.tree()`) re-fetches — a sealed-not-unlocked
 * meeting's memory disappears from this list live (mirrors the overview/graph
 * refetch shape in `BrainComponent`). Forget / Clear are bitemporal INVALIDATEs
 * in the backend (history preserved) — the copy says "Forget", not "Delete".
 */
@Component({
  selector: "app-brain-memory",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink, MemoryImportComponent],
  templateUrl: "./brain-memory.component.html",
  styleUrl: "./brain-memory.component.scss",
})
export class BrainMemoryComponent {
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);
  private readonly toast = inject(ToastService);
  private readonly errorCopy = inject(ErrorCopyService);

  /** Collapsed by default — the Brain page leads with sources, not memory. */
  readonly open = signal(false);

  private readonly memory = signal<UserMemory | null>(null);
  readonly loading = signal(true);
  readonly error = signal<string | null>(null);

  readonly facts = computed<UserMemoryFact[]>(
    () => this.memory()?.facts ?? [],
  );

  /**
   * TRUE when cross-meeting memory is turned OFF entirely (`get_user_memory`
   * returns the `disabled` marker with empty facts/brief). Renders a distinct
   * "memory is off" affordance rather than an "empty memory" one — mirrors the
   * backend, which suppresses ALL injection in this state.
   */
  readonly disabled = computed<boolean>(() => this.memory()?.disabled ?? false);

  /** The id of the fact currently being forgotten (disables just that row). */
  readonly forgettingId = signal<string | null>(null);
  /** Clear-all confirm gate + in-flight flag (inline confirm, no floating menu). */
  readonly confirmingClear = signal(false);
  readonly clearing = signal(false);

  constructor() {
    // (Re)load memory whenever the folder lock-state changes — a session
    // unlock/relock shifts which facts are visible (gated by source-meeting
    // visibility server-side). Reading `tree()` registers the dependency; the
    // fetch writes signals synchronously before its first await, so writes must
    // be allowed (NG0600 guard — mirrors BrainComponent's overview effect).
    effect(
      () => {
        this.folders.tree();
        void this.fetch();
      },
    );
  }

  /** Refetch after a memory import added new facts (L2.3 — the child emits `imported`). */
  onImported(): void {
    void this.fetch();
  }

  /** Render one fact as a plain sentence: "<subject> <predicate> <object>". */
  factLine(f: UserMemoryFact): string {
    return [f.subject, f.predicate, f.object]
      .map((s) => s.trim())
      .filter((s) => s.length > 0)
      .join(" ");
  }

  private async fetch(): Promise<void> {
    this.loading.set(true);
    this.error.set(null);
    try {
      this.memory.set(await this.ipc.getUserMemory());
    } catch (e) {
      this.memory.set(null);
      this.error.set(this.errorCopy.humanize(e));
    } finally {
      this.loading.set(false);
    }
  }

  async forget(id: string): Promise<void> {
    if (this.forgettingId()) return;
    this.forgettingId.set(id);
    try {
      await this.ipc.forgetUserFact(id);
      await this.fetch();
      this.toast.info("Forgotten.");
    } catch (e) {
      this.toast.danger(this.errorCopy.humanize(e));
    } finally {
      this.forgettingId.set(null);
    }
  }

  async clearAll(): Promise<void> {
    if (this.clearing()) return;
    this.clearing.set(true);
    try {
      await this.ipc.clearUserMemory();
      await this.fetch();
      this.confirmingClear.set(false);
      this.toast.info("Memory cleared.");
    } catch (e) {
      this.toast.danger(this.errorCopy.humanize(e));
    } finally {
      this.clearing.set(false);
    }
  }
}
