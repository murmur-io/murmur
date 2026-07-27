import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  OnInit,
  computed,
  inject,
  input,
  signal,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import type { ActionItem } from "../../../core/models";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";

/**
 * "Action items" — a glass panel listing the action-item checklist parsed from a
 * meeting's note (via {@link IpcService.getActionItems}). A presentational
 * sibling of the analysis + recipes + chat cards: the parent owns the meeting;
 * this component owns only the action-item list and the two side-effects it can
 * trigger over it — adding a single item to macOS Reminders
 * ({@link IpcService.addReminder}) and rewriting the whole note into Obsidian
 * Tasks format ({@link IpcService.patchNoteTasks}).
 *
 * Lives in its own file so its inline styles get their own per-component
 * `anyComponentStyle` budget (the detail component's styles are near the cap),
 * mirroring {@link MeetingRecipesComponent} / {@link MeetingChatComponent}.
 *
 * Meetings with no action items render NOTHING — the host is hidden so the
 * detail view shows no empty panel.
 */
@Component({
  selector: "app-meeting-actions",
  changeDetection: ChangeDetectionStrategy.OnPush,
  // Hide the host entirely when there are no items (no empty panel).
  host: { "[hidden]": "items().length === 0" },
  templateUrl: "./meeting-actions.component.html",
  styleUrl: "./meeting-actions.component.scss",
})
export class MeetingActionsComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);
  private readonly errorCopy = inject(ErrorCopyService);

  /** The meeting whose note's action items are listed + patched. */
  readonly meetingId = input.required<string>();

  /** The parsed action items; empty before load (and while none exist). */
  readonly items = signal<ActionItem[]>([]);

  // --- Per-row Reminders state (keyed by item idx) ------------------------
  /** Item indices with an in-flight addReminder call (disables their button). */
  readonly busyIdx = signal<ReadonlySet<number>>(new Set());
  /** Item indices added to Reminders (swaps the button for "Added ✓"). */
  readonly addedIdx = signal<ReadonlySet<number>>(new Set());
  /** Per-item inline error message (e.g. permission denied). */
  readonly errorIdx = signal<ReadonlyMap<number, string>>(new Map());

  // --- Note-wide "Save to Obsidian Tasks" state ---------------------------
  /** True while a patchNoteTasks call is in flight. */
  readonly patching = signal(false);
  /** Drives the brief "Saved to vault" flash after a successful patch. */
  readonly patchSaved = signal(false);
  /** Inline error surfaced when the patch fails. */
  readonly patchError = signal<string | null>(null);

  /** Tracked so we can cancel the pending "Saved" reset on destroy (no leaks). */
  private patchSavedTimer: ReturnType<typeof setTimeout> | null = null;

  /** Convenience: whether there is anything to show (drives the host [hidden]). */
  readonly hasItems = computed(() => this.items().length > 0);

  async ngOnInit(): Promise<void> {
    await this.loadItems();
  }

  /** Load (or reload) the action items into the `items` signal (best-effort). */
  private async loadItems(): Promise<void> {
    try {
      this.items.set(await this.ipc.getActionItems(this.meetingId()));
    } catch {
      // Leave whatever we have; an empty list simply hides the panel.
      this.items.set([]);
    }
  }

  // --- Reminders -----------------------------------------------------------

  /**
   * Add a single action item to macOS Reminders. Tracks per-row busy/added/error
   * state by item idx; on a TCC permission rejection the raw error often reads
   * obscurely, so a denial is mapped to a clear, actionable message.
   */
  async addToReminders(item: ActionItem): Promise<void> {
    if (this.busyIdx().has(item.idx) || this.addedIdx().has(item.idx)) {
      return;
    }
    this.setBusy(item.idx, true);
    this.clearRowError(item.idx);
    try {
      await this.ipc.addReminder(item.text, item.dueDate);
      this.addedIdx.update((s) => new Set(s).add(item.idx));
    } catch (e) {
      this.setRowError(item.idx, this.reminderErrorMessage(e));
    } finally {
      this.setBusy(item.idx, false);
    }
  }

  /**
   * Map a Reminders failure to a clear message (permission denial → settings).
   *
   * Keyed on the `[reminders-denied]` code (`errcode::REMINDERS_DENIED`) rather than on a
   * `/permission|denied|access|authoriz|not allowed/` sweep over the raw string — that sweep also
   * matched unrelated failures, and it rendered the osascript stderr verbatim on the miss.
   */
  private reminderErrorMessage(error: unknown): string {
    if (this.errorCopy.is(error, "reminders-denied")) {
      return "Grant Reminders access in System Settings.";
    }
    return this.errorCopy.because("Couldn’t add to Reminders", error);
  }

  // --- Save to Obsidian Tasks ---------------------------------------------

  /**
   * Rewrite the note's action items into Obsidian Tasks format (with 📅 due
   * dates) and re-write the vault file, then reload the items so any normalised
   * text/dates are reflected. Flashes a brief "Saved to vault" confirmation;
   * errors surface inline.
   */
  async saveToTasks(): Promise<void> {
    if (this.patching()) {
      return;
    }
    this.patching.set(true);
    this.patchError.set(null);
    try {
      await this.ipc.patchNoteTasks(this.meetingId());
      await this.loadItems();
      this.flashPatchSaved();
    } catch (e) {
      this.patchError.set(this.errorCopy.because("Couldn’t save to your note", e));
    } finally {
      this.patching.set(false);
    }
  }

  /** Show "Saved to vault" for a moment (tracked timeout — cleared on destroy). */
  private flashPatchSaved(): void {
    this.patchSaved.set(true);
    if (this.patchSavedTimer) {
      clearTimeout(this.patchSavedTimer);
    }
    this.patchSavedTimer = setTimeout(() => this.patchSaved.set(false), 2200);
    this.destroyRef.onDestroy(() => {
      if (this.patchSavedTimer) {
        clearTimeout(this.patchSavedTimer);
      }
    });
  }

  // --- Per-row state helpers (immutable Set/Map updates) ------------------

  private setBusy(idx: number, busy: boolean): void {
    this.busyIdx.update((s) => {
      const next = new Set(s);
      if (busy) {
        next.add(idx);
      } else {
        next.delete(idx);
      }
      return next;
    });
  }

  private setRowError(idx: number, message: string): void {
    this.errorIdx.update((m) => new Map(m).set(idx, message));
  }

  private clearRowError(idx: number): void {
    this.errorIdx.update((m) => {
      if (!m.has(idx)) {
        return m;
      }
      const next = new Map(m);
      next.delete(idx);
      return next;
    });
  }
}
