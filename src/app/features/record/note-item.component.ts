import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  inject,
  input,
  output,
  signal,
  viewChild,
} from "@angular/core";
import { MeetingConversationStore } from "../../core/meeting-conversation.store";
import type { NoteItem, ThreadTurn } from "../../core/meeting-conversation.store";
import { MarkdownComponent } from "../../shared/markdown.component";
import { AssistantSourcesComponent } from "../../shared/assistant-sources.component";

/**
 * ONE line of the user's notes + its anchored `@brain` THREAD (Slack-style). The
 * note line is the main-flow content; when the line opened a `@brain` thread, the
 * nested, collapsible Q&A renders INDENTED below it (a connector + offset).
 *
 * Each AGENT turn in the thread carries the propose-accept affordance —
 * "✓ Add to notes" / dismiss — wired to {@link MeetingConversationStore}: accept
 * appends the agent's text as a NEW persisted note line (the only path content
 * enters the notes); dismiss discards it. The thread has its OWN small follow-up
 * input so a follow-up goes to the agent WITHOUT re-typing `@brain` (it ships the
 * thread's own history → multi-turn).
 *
 * Presentational — all state lives in the store; this component holds only its
 * follow-up draft. A note line holds TURNS, not notes, so there is NO mutual
 * recursion (no `forwardRef` / trap T2 needed). The thread is IN-FLOW (not a
 * floating overlay), so the in-flow surface tokens are correct (trap T3 N/A).
 */
@Component({
  selector: "app-note-item",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MarkdownComponent, AssistantSourcesComponent],
  template: `
    @let n = note();
    <div class="note">
      <!-- ── The note line — the main-flow content. A thread anchor shows the
           @brain glyph + a collapse toggle; a plain note shows a quiet bullet. ── -->
      <div class="note-line" [class.is-anchor]="!!n.thread">
        @if (n.thread) {
          <button
            type="button"
            class="line-toggle"
            (click)="store.toggleThread(n.id)"
            [attr.aria-expanded]="n.threadOpen"
            [attr.aria-label]="n.threadOpen ? 'Collapse thread' : 'Expand thread'"
          >
            <span class="brain-glyph" aria-hidden="true">🧠</span>
            <span class="line-text">{{ n.text }}</span>
            <span
              class="caret"
              [class.is-open]="n.threadOpen"
              aria-hidden="true"
            >
              ▸
            </span>
            @if (!n.threadOpen && replyCount() > 0) {
              <span class="reply-count">{{ replyCount() }}</span>
            }
          </button>
        } @else {
          <span class="bullet" aria-hidden="true"></span>
          <span class="line-text">{{ n.text }}</span>
        }
      </div>

      <!-- ── The nested, collapsible thread — indented under the line ───────── -->
      @if (n.thread && n.threadOpen) {
        <div class="thread" role="group" aria-label="@brain thread">
          @for (turn of n.thread; track turn.id) {
            @if (turn.role === "user") {
              <div class="turn turn-user">
                <div class="turn-bubble turn-bubble-user">{{ turn.text }}</div>
              </div>
            } @else {
              <div class="turn turn-agent">
                <div class="turn-bubble turn-bubble-agent">
                  @if (turn.trace.length > 0) {
                    <div class="trace" role="status" aria-label="Tool use">
                      @for (t of turn.trace; track t.id) {
                        <span
                          class="trace-chip"
                          [class.is-running]="t.state === 'running'"
                          [class.is-web]="t.tool === 'web_search'"
                          [class.is-failed]="!t.ok"
                        >
                          <span class="trace-ico" aria-hidden="true">
                            @if (t.state === "running") {
                              <span class="trace-spin"></span>
                            } @else if (!t.ok) {
                              ⚠
                            } @else {
                              ✓
                            }
                          </span>
                          {{ toolLabel(t.tool) }}
                          @if (t.state === "done" && t.count) {
                            <span class="trace-count">{{ t.count }}</span>
                          }
                        </span>
                      }
                    </div>
                  }
                  @if (turn.status === "pending" && turn.trace.length === 0) {
                    <span class="dots" aria-hidden="true">
                      <span></span><span></span><span></span>
                    </span>
                  } @else if (turn.text) {
                    <app-markdown [markdown]="turn.text" compact />
                  } @else if (turn.status !== "pending") {
                    <span class="agent-note">{{ emptyNote(turn.status) }}</span>
                  }
                  @if (turn.citations.length > 0) {
                    <app-assistant-sources [citations]="turn.citations" />
                  }

                  <!-- Propose → accept: the agent NEVER auto-writes; "Add to
                       notes" is the only path content enters the main notes. -->
                  @if (canAccept(turn)) {
                    <div class="turn-actions">
                      <button
                        type="button"
                        class="accept-btn"
                        (click)="store.acceptIntoNotes(n.id, turn.id)"
                      >
                        <span aria-hidden="true">✓</span> Add to notes
                      </button>
                      <button
                        type="button"
                        class="dismiss-btn"
                        (click)="store.dismissTurn(n.id, turn.id)"
                        aria-label="Dismiss this reply"
                      >
                        Dismiss
                      </button>
                    </div>
                  } @else if (turn.accepted && turn.text) {
                    <span class="added-tag" aria-hidden="true">✓ Added to notes</span>
                  }
                </div>
              </div>
            }
          }

          <!-- The thread's OWN follow-up input — no @brain needed here. -->
          <form class="follow" (submit)="submitFollow($event)">
            <input
              #fin
              type="text"
              class="follow-input"
              autocomplete="off"
              [value]="followDraft()"
              [disabled]="n.threadPending"
              [placeholder]="n.threadPending ? 'Thinking…' : 'Reply to @brain…'"
              (input)="onFollowInput($event)"
              aria-label="Reply in this thread"
            />
            <button
              type="submit"
              class="follow-send"
              [disabled]="!canFollow()"
              aria-label="Send follow-up"
            >
              @if (n.threadPending) {
                <span class="follow-spin" aria-hidden="true"></span>
              } @else {
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                  <path
                    d="M5 12h14M13 6l6 6-6 6"
                    stroke="currentColor"
                    stroke-width="2.4"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                  />
                </svg>
              }
            </button>
          </form>
        </div>
      }
    </div>
  `,
  styles: [
    `
      :host {
        display: block;
      }
      .note {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }

      /* ── The note line — the user's main flow ──────────────────────────── */
      .note-line {
        display: flex;
        align-items: baseline;
        gap: var(--space-2);
        font-size: 0.95rem;
        line-height: 1.55;
        color: var(--text-primary);
      }
      .bullet {
        flex: 0 0 auto;
        width: 5px;
        height: 5px;
        margin-top: 0.55em;
        border-radius: 50%;
        background: var(--text-muted);
      }
      .line-text {
        flex: 1 1 auto;
        min-width: 0;
        white-space: pre-wrap;
        word-break: break-word;
      }

      /* A @brain anchor line: the whole line toggles the thread. */
      .line-toggle {
        display: flex;
        align-items: baseline;
        gap: var(--space-2);
        width: 100%;
        padding: var(--space-1) var(--space-2);
        margin: 0;
        border: none;
        border-radius: var(--radius-sm);
        background: transparent;
        color: var(--text-primary);
        font: inherit;
        text-align: left;
        cursor: pointer;
        transition: background var(--transition);
      }
      .line-toggle:hover {
        background: var(--surface-hover);
      }
      .line-toggle:focus-visible {
        outline: none;
        box-shadow: 0 0 0 2px var(--accent-ring);
      }
      .brain-glyph {
        flex: 0 0 auto;
        font-size: 0.95rem;
        line-height: 1.4;
      }
      .caret {
        flex: 0 0 auto;
        color: var(--text-muted);
        font-size: 0.7rem;
        transition: transform var(--transition);
      }
      .caret.is-open {
        transform: rotate(90deg);
      }
      .reply-count {
        flex: 0 0 auto;
        padding: 0 6px;
        border-radius: var(--radius-pill);
        background: var(--accent-soft);
        color: var(--accent-hover);
        font-size: 0.7rem;
        font-weight: 600;
        font-variant-numeric: tabular-nums;
      }

      /* ── The nested thread — indented under the line (Slack-style) ─────── */
      .thread {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        margin-left: var(--space-4);
        padding-left: var(--space-3);
        border-left: 2px solid var(--border-subtle);
      }
      .turn {
        display: flex;
      }
      .turn-user {
        justify-content: flex-end;
      }
      .turn-agent {
        justify-content: flex-start;
      }
      .turn-bubble {
        max-width: 92%;
        padding: var(--space-2) var(--space-3);
        border-radius: var(--radius-md);
        font-size: 0.875rem;
        line-height: 1.5;
        animation: rise 200ms var(--transition) both;
      }
      .turn-bubble-user {
        background: var(--accent-gradient);
        color: var(--text-on-accent);
        border-bottom-right-radius: var(--radius-sm);
        white-space: pre-wrap;
      }
      .turn-bubble-agent {
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
        color: var(--text-primary);
        border-bottom-left-radius: var(--radius-sm);
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .agent-note {
        color: var(--text-secondary);
      }

      /* ── propose → accept affordance ───────────────────────────────────── */
      .turn-actions {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        margin-top: 2px;
      }
      .accept-btn {
        display: inline-flex;
        align-items: center;
        gap: 5px;
        padding: 3px var(--space-3);
        border: 1px solid var(--accent-ring);
        border-radius: var(--radius-pill);
        background: var(--accent-soft);
        color: var(--accent-hover);
        font-size: 0.78rem;
        font-weight: 600;
        cursor: pointer;
        transition:
          background var(--transition),
          color var(--transition);
      }
      .accept-btn:hover {
        background: var(--accent);
        color: #fff;
      }
      .accept-btn:focus-visible {
        outline: none;
        box-shadow: 0 0 0 2px var(--accent-ring);
      }
      .dismiss-btn {
        padding: 3px var(--space-2);
        border: none;
        border-radius: var(--radius-pill);
        background: transparent;
        color: var(--text-muted);
        font-size: 0.78rem;
        cursor: pointer;
        transition: color var(--transition);
      }
      .dismiss-btn:hover {
        color: var(--text-secondary);
      }
      .added-tag {
        margin-top: 2px;
        color: var(--text-muted);
        font-size: 0.74rem;
        font-weight: 600;
        letter-spacing: 0.02em;
      }

      /* ── the thread's own follow-up input ──────────────────────────────── */
      .follow {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        margin-top: 2px;
      }
      .follow-input {
        flex: 1 1 auto;
        min-width: 0;
        height: 34px;
        padding: 0 var(--space-3);
        font-size: 0.85rem;
      }
      .follow-send {
        flex: 0 0 auto;
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 34px;
        height: 34px;
        border: 1px solid var(--accent-ring);
        border-radius: 50%;
        background: var(--accent-soft);
        color: var(--accent-hover);
        cursor: pointer;
        transition:
          background var(--transition),
          color var(--transition);
      }
      .follow-send:hover:not(:disabled) {
        background: var(--accent);
        color: #fff;
      }
      .follow-send:focus-visible {
        outline: none;
        box-shadow: 0 0 0 2px var(--accent-ring);
      }
      .follow-send:disabled {
        opacity: 0.5;
        cursor: default;
      }
      .follow-spin {
        width: 13px;
        height: 13px;
        border-radius: 50%;
        border: 2px solid var(--accent-ring);
        border-top-color: var(--accent);
        animation: spin 0.7s linear infinite;
      }

      /* ── live tool trace ──────────────────────────────────────────────── */
      .trace {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-1);
      }
      .trace-chip {
        display: inline-flex;
        align-items: center;
        gap: 5px;
        padding: 2px var(--space-2);
        border-radius: var(--radius-pill);
        background: var(--accent-soft);
        border: 1px solid var(--border-subtle);
        color: var(--text-secondary);
        font-size: 0.74rem;
        line-height: 1.2;
      }
      .trace-chip.is-running {
        color: var(--text-primary);
      }
      .trace-chip.is-web {
        background: color-mix(in srgb, var(--live) 16%, transparent);
        border-color: color-mix(in srgb, var(--live) 35%, transparent);
      }
      .trace-chip.is-failed {
        opacity: 0.6;
      }
      .trace-ico {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 11px;
        height: 11px;
        font-size: 0.66rem;
        color: var(--accent);
      }
      .trace-chip.is-web .trace-ico {
        color: var(--live);
      }
      .trace-count {
        color: var(--text-muted);
        font-variant-numeric: tabular-nums;
      }
      .trace-spin {
        width: 8px;
        height: 8px;
        border-radius: 50%;
        border: 1.5px solid var(--accent-ring);
        border-top-color: var(--accent);
        animation: spin 0.7s linear infinite;
      }
      @keyframes spin {
        to {
          transform: rotate(360deg);
        }
      }

      .dots {
        display: inline-flex;
        gap: 3px;
      }
      .dots span {
        width: 5px;
        height: 5px;
        border-radius: 50%;
        background: var(--accent);
        animation: blink 1.2s ease-in-out infinite both;
      }
      .dots span:nth-child(2) {
        animation-delay: 0.2s;
      }
      .dots span:nth-child(3) {
        animation-delay: 0.4s;
      }
      @keyframes blink {
        0%,
        80%,
        100% {
          opacity: 0.3;
        }
        40% {
          opacity: 1;
        }
      }

      @media (prefers-reduced-motion: reduce) {
        .turn-bubble,
        .dots span,
        .trace-spin,
        .follow-spin {
          animation: none;
        }
        .dots span {
          opacity: 0.7;
        }
      }
    `,
  ],
})
export class NoteItemComponent {
  protected readonly store = inject(MeetingConversationStore);
  private readonly injector = inject(Injector);

  /** The note line (with its thread, if any) — the single source of truth. */
  readonly note = input.required<NoteItem>();
  /** Fired when this item's follow-up form is submitted (parent scrolls/etc.). */
  readonly followed = output<void>();

  private readonly followInput = viewChild<ElementRef<HTMLInputElement>>("fin");

  /** This thread's follow-up draft (signal-backed — zoneless). */
  protected readonly followDraft = signal("");

  /** Number of agent replies (for the collapsed "N" badge). */
  protected readonly replyCount = computed(
    () => this.note().thread?.filter((t) => t.role === "agent").length ?? 0,
  );

  /** Follow-up send is allowed with non-blank text and no in-flight thread turn. */
  protected readonly canFollow = computed(
    () => !this.note().threadPending && this.followDraft().trim().length > 0,
  );

  /** Whether an agent turn can still be accepted into the notes. */
  protected canAccept(turn: ThreadTurn): boolean {
    return (
      turn.role === "agent" &&
      turn.status !== "pending" &&
      !turn.accepted &&
      turn.text.trim().length > 0
    );
  }

  protected onFollowInput(event: Event): void {
    this.followDraft.set((event.target as HTMLInputElement).value);
  }

  /** Submit a follow-up into THIS thread (no @brain needed) → the agent. */
  protected submitFollow(event: Event): void {
    event.preventDefault();
    const text = this.followDraft().trim();
    if (!text || this.note().threadPending) return;
    this.followDraft.set("");
    void this.store.followUp(this.note().id, text).catch(() => {
      /* the store resolves the agent turn with an error in the thread */
    });
    this.followed.emit();
    // Keep the focus in the thread input for a fast back-and-forth.
    afterNextRender(
      () => this.followInput()?.nativeElement.focus(),
      { injector: this.injector },
    );
  }

  /** Human label for a tool-trace chip. */
  protected toolLabel(tool: string): string {
    switch (tool) {
      case "search_meetings":
        return "Searching notes";
      case "search_semantic":
        return "Searching by meaning";
      case "get_meeting":
        return "Reading a meeting";
      case "list_recent_meetings":
        return "Listing meetings";
      case "get_open_commitments":
        return "Checking action items";
      case "get_entity_dossier":
        return "Looking up an entity";
      case "web_search":
        return "Searching the web";
      case "calendar_lookup":
        return "Checking the calendar";
      default:
        return tool;
    }
  }

  /** Short message for a resolved agent turn that carries no answer text. */
  protected emptyNote(status: ThreadTurn["status"]): string {
    switch (status) {
      case "nothing_heard":
        return "Nie usłyszałem — spróbuj jeszcze raz";
      case "needs_consent":
        return "Needs consent to answer.";
      case "unavailable":
        return "That isn't available yet.";
      case "unrecognized":
        return "I didn't catch a question there.";
      case "error":
        return "Something went wrong.";
      default:
        return "(no answer)";
    }
  }
}
