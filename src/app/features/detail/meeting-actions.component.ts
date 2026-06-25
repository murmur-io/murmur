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
import { IpcService } from "../../core/ipc.service";
import type { ActionItem } from "../../core/models";

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
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  // Hide the host entirely when there are no items (no empty panel).
  host: { "[hidden]": "items().length === 0" },
  template: `
    @if (items().length) {
      <div class="act card">
        <div class="act-head">
          <div class="act-head-text">
            <h3 class="act-title">Action items</h3>
            <span class="act-sub"
              >Send them to Reminders, or write them into your note</span
            >
          </div>
          <button
            type="button"
            class="btn btn-ghost act-patch"
            title="Writes 📅 due-dates into your note as Tasks-plugin checkboxes."
            [disabled]="patching()"
            (click)="saveToTasks()"
          >
            @if (patchSaved()) {
              <span class="act-patch-check" aria-hidden="true"></span>
              Saved to vault
            } @else {
              {{ patching() ? "Saving…" : "Save to Obsidian Tasks" }}
            }
          </button>
        </div>

        <!-- Note-wide patch error (separate from per-row Reminder errors). -->
        @if (patchError(); as err) {
          <p class="act-patch-error" role="alert">{{ err }}</p>
        }

        <ul class="act-list">
          @for (it of items(); track it.idx) {
            <li class="act-row" [class.is-done]="it.done" [style.--i]="$index">
              <span
                class="act-mark"
                [class.is-done]="it.done"
                role="img"
                [attr.aria-label]="it.done ? 'Done' : 'Not done'"
              ></span>

              <div class="act-body">
                <span class="act-text">{{ it.text }}</span>
                <div class="act-tags">
                  @if (it.owner; as owner) {
                    <span class="act-owner">{{ owner }}</span>
                  }
                  @if (it.dueDate; as due) {
                    <span class="act-due">
                      <span class="act-due-cal" aria-hidden="true">📅</span>
                      <span class="act-due-date">{{ due }}</span>
                    </span>
                  }
                </div>
              </div>

              <!-- Reminders affordance: only for not-yet-done items. -->
              @if (!it.done) {
                <div class="act-row-action">
                  @if (addedIdx().has(it.idx)) {
                    <span class="act-added" role="status">Added ✓</span>
                  } @else {
                    <button
                      type="button"
                      class="btn btn-ghost act-add"
                      [disabled]="busyIdx().has(it.idx)"
                      [attr.aria-label]="'Add to Reminders: ' + it.text"
                      (click)="addToReminders(it)"
                    >
                      {{
                        busyIdx().has(it.idx) ? "Adding…" : "Add to Reminders"
                      }}
                    </button>
                  }
                </div>
              }
            </li>

            @if (errorIdx().get(it.idx); as err) {
              <li class="act-row-error" role="alert">{{ err }}</li>
            }
          }
        </ul>
      </div>
    }
  `,
  styles: [
    `
      :host {
        display: block;
      }
      :host([hidden]) {
        display: none;
      }

      .act {
        position: relative;
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
        padding: var(--space-5);
        overflow: hidden;
        animation: rise 420ms var(--transition) both;
      }
      /* A faint aurora wash to lift the glass above the page surface. */
      .act::before {
        content: "";
        position: absolute;
        inset: 0;
        pointer-events: none;
        background: radial-gradient(
          120% 90% at 88% -10%,
          rgba(157, 123, 255, 0.1),
          transparent 60%
        );
      }
      .act > * {
        position: relative;
        z-index: 1;
      }

      /* --- Head --- */
      .act-head {
        display: flex;
        flex-wrap: wrap;
        align-items: flex-start;
        justify-content: space-between;
        gap: var(--space-3);
      }
      .act-head-text {
        display: flex;
        flex-direction: column;
        gap: 2px;
        min-width: 0;
      }
      .act-title {
        margin: 0;
      }
      .act-sub {
        color: var(--text-muted);
        font-size: 0.8125rem;
      }
      .act-patch {
        flex: none;
        height: 34px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
        font-weight: 600;
      }
      .act-patch-check {
        position: relative;
        width: 13px;
        height: 13px;
        flex: none;
        color: var(--success);
      }
      .act-patch-check::after {
        content: "";
        position: absolute;
        left: 4px;
        top: 0;
        width: 4px;
        height: 9px;
        border: solid currentColor;
        border-width: 0 2px 2px 0;
        transform: rotate(45deg);
      }
      .act-patch-error {
        margin: 0;
        padding: var(--space-2) var(--space-3);
        border: 1px solid rgba(255, 107, 107, 0.3);
        border-radius: var(--radius-md);
        background: var(--danger-soft);
        color: var(--text-primary);
        font-size: 0.85rem;
      }

      /* --- List --- */
      .act-list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .act-row {
        display: flex;
        align-items: flex-start;
        gap: var(--space-3);
        padding: var(--space-3);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        animation: rise 360ms var(--transition) both;
        animation-delay: calc(var(--i, 0) * 45ms + 60ms);
        transition:
          border-color var(--transition),
          background var(--transition);
      }
      .act-row:hover {
        border-color: var(--border-strong);
      }

      /* Read-only done/not-done mark (mirrors the analysis checklist). */
      .act-mark {
        flex: none;
        position: relative;
        width: 20px;
        height: 20px;
        margin-top: 0.1em;
        border: 1px solid var(--border-strong);
        border-radius: var(--radius-sm);
        background: var(--surface-input);
      }
      .act-mark.is-done {
        background: var(--accent-gradient);
        border-color: transparent;
      }
      .act-mark.is-done::after {
        content: "";
        position: absolute;
        left: 6px;
        top: 2px;
        width: 5px;
        height: 10px;
        border: solid var(--text-on-accent);
        border-width: 0 2px 2px 0;
        transform: rotate(45deg);
      }

      .act-body {
        flex: 1 1 auto;
        min-width: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .act-text {
        color: var(--text-secondary);
        line-height: 1.5;
        overflow-wrap: anywhere;
      }
      .act-row.is-done .act-text {
        color: var(--text-muted);
        text-decoration: line-through;
        text-decoration-color: var(--text-muted);
      }
      .act-tags {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-2);
      }

      /* Subtle owner pill. */
      .act-owner {
        display: inline-flex;
        align-items: center;
        height: 22px;
        padding: 0 var(--space-3);
        border-radius: var(--radius-pill);
        background: var(--accent-soft);
        color: var(--accent-hover);
        font-size: 0.75rem;
        font-weight: 600;
      }

      /* Due-date chip — mono + tabular figures. */
      .act-due {
        display: inline-flex;
        align-items: center;
        gap: var(--space-1);
        height: 22px;
        padding: 0 var(--space-2);
        border-radius: var(--radius-pill);
        background: var(--surface-hover);
        border: 1px solid var(--border-subtle);
        color: var(--text-secondary);
      }
      .act-due-cal {
        font-size: 0.75rem;
        line-height: 1;
      }
      .act-due-date {
        font-family: var(--font-mono);
        font-size: 0.75rem;
        font-variant-numeric: tabular-nums;
        letter-spacing: -0.01em;
      }

      /* --- Per-row Reminders affordance --- */
      .act-row-action {
        flex: none;
        align-self: center;
      }
      .act-add {
        height: 30px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
        white-space: nowrap;
      }
      .act-added {
        display: inline-flex;
        align-items: center;
        height: 30px;
        padding: 0 var(--space-3);
        border-radius: var(--radius-pill);
        background: var(--success-soft);
        color: var(--success);
        font-size: 0.8125rem;
        font-weight: 600;
        white-space: nowrap;
        animation: rise 240ms var(--transition) both;
      }

      /* Per-row Reminder error (e.g. permission denied). */
      .act-row-error {
        margin: calc(var(--space-1) * -1) 0 0;
        padding: var(--space-2) var(--space-3);
        border: 1px solid rgba(255, 107, 107, 0.3);
        border-radius: var(--radius-md);
        background: var(--danger-soft);
        color: var(--text-primary);
        font-size: 0.8125rem;
        animation: rise 200ms var(--transition) both;
      }

      @media (max-width: 720px) {
        .act-row {
          flex-wrap: wrap;
        }
        .act-row-action {
          width: 100%;
          align-self: stretch;
        }
        .act-add,
        .act-added {
          width: 100%;
          justify-content: center;
        }
      }

      @media (prefers-reduced-motion: reduce) {
        .act,
        .act-row,
        .act-added,
        .act-row-error {
          animation: none;
        }
      }
    `,
  ],
})
export class MeetingActionsComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);

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

  /** Map a Reminders failure to a clear message (permission denial → settings). */
  private reminderErrorMessage(error: unknown): string {
    const raw = String(error);
    if (/permission|denied|access|authoriz|not allowed/i.test(raw)) {
      return "Grant Reminders access in System Settings.";
    }
    return "Couldn’t add to Reminders: " + raw;
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
      this.patchError.set("Couldn’t save to your note: " + String(e));
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
