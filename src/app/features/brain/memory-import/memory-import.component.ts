import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  output,
  signal,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import { ToastService } from "../../../services/toast.service";

/**
 * Brain v2 L2.3 — "Import memories from another assistant": paste a ChatGPT /
 * Claude memory export, one Import button. The backend extracts STRICTLY on
 * the on-device light reasoner (local-or-stub, never cloud — the pasted export
 * never leaves the device), dedups against the existing memory (reconcile →
 * NoOps on a re-import), and anchors the new facts to a synthetic "Memory
 * Import" meeting — so the whole import is undoable by deleting that meeting.
 * No local brain model ⇒ 0 imported (the empty-result hint mentions it). Emits
 * `imported` with the added count so the parent audit list can refetch.
 *
 * Signals-first + OnPush; ZERO egress by construction.
 */
@Component({
  selector: "app-memory-import",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./memory-import.component.html",
  styleUrl: "./memory-import.component.scss",
})
export class MemoryImportComponent {
  private readonly ipc = inject(IpcService);
  private readonly toast = inject(ToastService);

  /** Fires after a successful import with the number of NEW facts added. */
  readonly imported = output<number>();

  /** Collapsed by default — an occasional migration affordance, not a headline. */
  readonly open = signal(false);
  readonly text = signal("");
  readonly importing = signal(false);
  /** The last import's added count (`null` until an import ran). */
  readonly lastCount = signal<number | null>(null);

  readonly canImport = computed(
    () => !this.importing() && this.text().trim().length > 0,
  );

  onInput(event: Event): void {
    this.text.set((event.target as HTMLTextAreaElement).value);
  }

  async runImport(): Promise<void> {
    if (!this.canImport()) return;
    this.importing.set(true);
    this.lastCount.set(null);
    try {
      const n = await this.ipc.importMemories(this.text());
      this.lastCount.set(n);
      if (n > 0) {
        this.text.set("");
        this.toast.info(
          n === 1 ? "Imported 1 memory." : `Imported ${n} memories.`,
        );
        this.imported.emit(n);
      }
    } catch (e) {
      this.toast.danger(String(e));
    } finally {
      this.importing.set(false);
    }
  }
}
