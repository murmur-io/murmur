import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import { ReminderComposerService } from "../../reminders/reminder-composer/reminder-composer.service";
import type { ActionItem } from "../../../core/models";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";

/**
 * "Action items" — a glass panel listing the action-item checklist parsed from a
 * meeting's note (via {@link IpcService.getActionItems}). The parent owns the
 * meeting; this component owns only the action-item list and the two things it
 * can do with it — turning one item into a MURMUR reminder anchored to this
 * meeting (through {@link ReminderComposerService}, not macOS Reminders) and
 * rewriting the whole note into Obsidian Tasks format
 * ({@link IpcService.patchNoteTasks}).
 *
 * Lives in its own file so its inline styles get their own per-component
 * `anyComponentStyle` budget (the detail component's styles are near the cap),
 * mirroring {@link MeetingRecipesComponent} / {@link MeetingChatComponent}.
 *
 * Meetings with no action items render NOTHING by default, so an inline mount
 * shows no empty panel. A host that mounts this as a deliberately-opened pane
 * passes `showEmptyState` and gets an explanatory empty state instead.
 *
 * It used to also carry {@link SmartReminderCardComponent}; that card is its own
 * drawer now (2026-09-13, at the operator's request), on this surface and in the
 * note editor.
 *
 * KNOWN COST, stated rather than hidden: the card is where the fail-closed
 * "suggestions aren't available securely right now" notice surfaces, so that
 * notice is no longer visible until someone opens the drawer. An indicator on
 * the toggle is the fix if this bites; it needs the suggestion count hoisted to
 * the host, since a closed drawer does not mount the card that knows it.
 */
@Component({
  selector: "app-meeting-actions",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./meeting-actions.component.html",
  styleUrl: "./meeting-actions.component.scss",
})
export class MeetingActionsComponent {
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);
  private readonly errorCopy = inject(ErrorCopyService);
  private readonly composer = inject(ReminderComposerService);

  /** The meeting whose note's action items are listed + patched. */
  readonly meetingId = input.required<string>();
  readonly meetingTitle = input<string | null>(null);
  readonly sourceRevision = input<string | null>(null);
  /**
   * Render the panel's own close control. True on the drawer mount (the detail
   * shell owns the header toggle, this panel owns its chrome — the same split
   * as the note's reminders drawer); false for any inline mount.
   */
  readonly showClose = input(false);
  /**
   * Render the card with an explanatory empty state instead of nothing when the
   * meeting has no action items. The drawer sets it: a pane the user opened on
   * purpose must say why it is empty rather than render an empty shell.
   */
  readonly showEmptyState = input(false);
  /** Fired by the panel's close ×; the host owns the open/closed state. */
  readonly closed = output<void>();

  /** The parsed action items; empty before load (and while none exist). */
  readonly items = signal<ActionItem[]>([]);

  // --- Note-wide "Save to Obsidian Tasks" state ---------------------------
  /** True while a patchNoteTasks call is in flight. */
  readonly patching = signal(false);
  /** Drives the brief "Saved to vault" flash after a successful patch. */
  readonly patchSaved = signal(false);
  /** Inline error surfaced when the patch fails. */
  readonly patchError = signal<string | null>(null);

  /** Tracked so we can cancel the pending "Saved" reset on destroy (no leaks). */
  private patchSavedTimer: ReturnType<typeof setTimeout> | null = null;

  /**
   * Re-read the items whenever the meeting or the note revision behind them
   * changes. `ngOnInit` alone was enough while this panel was destroyed and
   * recreated on every Note/Audio tab switch; the drawer mount deliberately
   * survives those, so without this a re-summarize or a note save would leave
   * the list showing the previous note's commitments.
   */
  private readonly _reload = effect(() => {
    const id = this.meetingId();
    this.sourceRevision();
    void this.loadItems(id, ++this.loadToken);
  });

  /**
   * Monotonic id for the in-flight load. Guarding on `meetingId` alone was not
   * enough: the effect also re-runs on a `sourceRevision` change, and two loads
   * for the SAME meeting are indistinguishable by id — so a slow first response
   * could land after a newer one and win.
   */
  private loadToken = 0;

  /**
   * Load (or reload) the action items into the `items` signal (best-effort).
   * Takes the id it was started for and drops a late response once the panel
   * has moved on, so a slow fetch can never overwrite a newer meeting's items.
   */
  private async loadItems(requestedId?: string, token?: number): Promise<void> {
    const id = requestedId ?? this.meetingId();
    const mine = token ?? ++this.loadToken;
    try {
      const items = await this.ipc.getActionItems(id);
      if (mine === this.loadToken && id === this.meetingId()) {
        this.items.set(items);
      }
    } catch {
      // Leave whatever we have; an empty list simply hides the panel.
      if (mine === this.loadToken && id === this.meetingId()) {
        this.items.set([]);
      }
    }
  }

  // --- Reminders -----------------------------------------------------------

  /**
   * Turn one action item into a reminder IN MURMUR (2026-09-13, user request —
   * it used to call `add_reminder`, which hands the text to macOS Reminders via
   * osascript and leaves Murmur knowing nothing about it).
   *
   * This opens Murmur's own composer prefilled from the item: its text as the
   * title, its 📅 date as the due date, and THIS meeting as the source anchor —
   * so the reminder comes back attached to the meeting it came from, shows up in
   * the Reminders inbox, and is visibility-gated like every other one.
   *
   * The composer, not a silent create, is the right shape here: `due_at` is NOT
   * NULL in the store, and an action item frequently has no date at all. Rather
   * than invent one, the user confirms the when — with everything else already
   * filled in.
   */
  addToReminders(item: ActionItem): void {
    if (!this.reminderReady()) {
      return;
    }
    this.composer.openCreate({
      title: item.text,
      dueAt: dueDateToEpochMs(item.dueDate),
      source: {
        kind: "meeting",
        id: this.meetingId(),
        // The anchor stays OPAQUE — no parent-supplied title. Submit re-gates it
        // and the canonical list resolves a title the session may actually see.
        // Copied deliberately from `smart-reminder-card.newReminder()`; passing
        // `meetingTitle()` here would route a title around that gate.
        title: "",
      },
    });
  }

  /**
   * The composer can only be trusted once its privacy/visibility listeners are
   * registered — same gate the command bar's "New reminder" applies. Until then
   * the affordance is visible but inert, rather than opening a composer whose
   * invalidation events nobody is listening for.
   */
  readonly reminderReady = computed(
    () => this.composer.listenerState() === "ready",
  );

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
}

/**
 * An action item carries a plain `YYYY-MM-DD` (or nothing). The composer wants
 * epoch milliseconds. Resolve at LOCAL midday, not midnight: a date-only value
 * parsed as UTC midnight lands on the previous day for anyone west of Greenwich,
 * which would quietly move every due date by one.
 */
function dueDateToEpochMs(dueDate: string | null): number | null {
  if (!dueDate) {
    return null;
  }
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(dueDate.trim());
  if (!match) {
    return null;
  }
  const [, y, m, d] = match;
  const at = new Date(Number(y), Number(m) - 1, Number(d), 12, 0, 0, 0);
  return Number.isNaN(at.getTime()) ? null : at.getTime();
}
