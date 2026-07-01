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
 * ONE line of the user's notes + its anchored `@brain` THREAD (Slack-style).
 *
 *  - A PLAIN note line is the user's own jotting — a quiet "note" treatment (a
 *    subtle note glyph + token styling) so it reads clearly as the user's note,
 *    distinct from a thread/agent line.
 *  - A `@brain` THREAD renders as a Slack-style chat: a collapsible "▸ Thread (N)"
 *    toggle (collapsed → a short snippet of the question); expanded → the user's
 *    question as the FIRST right-aligned (purple) user bubble, then the brain's
 *    replies LEFT-aligned with a friendly "🧠 brain" identity, follow-up user
 *    bubbles on the right, and the thread's own Reply input. The question shows
 *    ONCE — the leading anchor turn is sliced out of the loop (no duplication).
 *
 * Propose → accept: an agent reply shows a DRAFT-NOTE card with a readable preview
 * of the proposed draft + "✓ Add to notes" / "Dismiss" ONLY when it carries a
 * `proposedNote` (the model decided the user asked it to MAKE a note); accept
 * appends THAT draft (not the whole reply) to the main notes; dismiss drops the
 * proposal (showing "Dismissed", never "✓ Added to notes"). A plain answer has no
 * proposal → no card, so the surface reads as a conversation, not a notes app with
 * a button under everything. The internal `propose_note` tool chip is filtered out
 * of the trace (it's plumbing, not a user-facing tool). The thread has its OWN
 * follow-up input so a follow-up goes to the agent WITHOUT re-typing `@brain`.
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
    <div class="note" [class.is-thread]="!!n.thread">
      <!-- ── A PERSISTED note line — the user's own jotting. It STAYS a note even
           after "✨ ask brain" attaches a thread (the thread hangs BELOW). A
           non-persisted @brain anchor instead shows its question as the thread's
           first bubble, so it has no note line. ─────────────────────────────── -->
      @if (n.persisted) {
        <div class="note-line">
          <span class="note-glyph" aria-hidden="true">
            <svg width="13" height="13" viewBox="0 0 24 24" fill="none">
              <path
                d="M5 4h11l3 3v13H5z"
                stroke="currentColor"
                stroke-width="1.8"
                stroke-linejoin="round"
              />
              <path
                d="M8 10h8M8 14h6"
                stroke="currentColor"
                stroke-width="1.8"
                stroke-linecap="round"
              />
            </svg>
          </span>
          <span class="line-text">{{ n.text }}</span>
          <!-- ✨ ask brain — only on a note that has NO thread yet (retroactively
               open a thread seeded from this note's text). Quiet; reveals on
               hover/focus of the row. A note WITH a thread shows its toggle below. -->
          @if (!n.thread) {
            <button
              type="button"
              class="ask-brain"
              (click)="store.askBrainOnNote(n.id)"
              [disabled]="!store.loaded()"
              title="Ask brain about this note"
              aria-label="Ask brain about this note"
            >
              <span class="ask-spark" aria-hidden="true">✨</span>
              <span class="ask-label">ask brain</span>
            </button>
          }
        </div>
      }

      @if (n.thread) {
        <!-- ── THREAD: a Slack-style collapsible toggle. Collapsed shows a short
             snippet (the @brain question, or the note text for an ✨ thread). ─── -->
        <button
          type="button"
          class="thread-toggle"
          (click)="store.toggleThread(n.id)"
          [attr.aria-expanded]="n.threadOpen"
        >
          <span class="caret" [class.is-open]="n.threadOpen" aria-hidden="true">
            ▸
          </span>
          <span class="toggle-label">Thread</span>
          @if (replyCount() > 0) {
            <span class="reply-count">{{ replyCount() }}</span>
          }
          @if (!n.threadOpen) {
            <span class="toggle-snippet">{{ snippet() }}</span>
          }
        </button>

        <!-- ── The nested, collapsible thread — indented under the toggle ────── -->
        @if (n.threadOpen) {
          <div class="thread" role="group" aria-label="brain thread">
            <!-- A @brain anchor shows its QUESTION as the user's first right-aligned
                 bubble (the leading turn is sliced from visibleTurns → no duplicate).
                 An ✨ thread on a persisted note already shows the text in the note
                 line above, so it does NOT repeat it as a bubble. -->
            @if (!n.persisted) {
              <div class="turn turn-user">
                <div class="turn-bubble turn-bubble-user">{{ n.text }}</div>
              </div>
            }

            @for (turn of visibleTurns(); track turn.id) {
              @if (turn.role === "user") {
                <div class="turn turn-user">
                  <div class="turn-bubble turn-bubble-user">{{ turn.text }}</div>
                </div>
              } @else {
                <div class="turn turn-agent">
                  <div class="agent-id" aria-hidden="true">
                    <span class="agent-mark">🧠</span>
                    <span class="agent-name">brain</span>
                  </div>
                  <div class="turn-bubble turn-bubble-agent">
                    @let trace = visibleTrace(turn.trace);
                    @if (trace.length > 0) {
                      <div class="trace" role="status" aria-label="Tool use">
                        @for (t of trace; track t.id) {
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
                    @if (turn.status === "pending" && trace.length === 0) {
                      <span class="thinking" role="status">
                        <span class="dots" aria-hidden="true">
                          <span></span><span></span><span></span>
                        </span>
                        <span class="thinking-label">Thinking…</span>
                      </span>
                    } @else if (turn.text) {
                      <app-markdown [markdown]="turn.text" compact />
                    } @else if (turn.status !== "pending") {
                      <span class="agent-note">{{ emptyNote(turn.status) }}</span>
                    }
                    @if (turn.citations.length > 0) {
                      <app-assistant-sources [citations]="turn.citations" />
                    }

                    <!-- Propose → accept: shown ONLY when the agent proposed a
                         NOTE (proposedNote != null). The DRAFT CONTENT is the
                         point — show it prominently so the user can review it
                         before accepting. A plain answer has no proposal. -->
                    @if (canAccept(turn)) {
                      <div class="proposal">
                        <span class="proposal-tag" aria-hidden="true">
                          📝 Draft note
                        </span>
                        <!-- The actual draft the user is accepting — rendered as a
                             readable quoted preview (not just the meta summary). -->
                        <div class="draft-preview">
                          <app-markdown [markdown]="turn.proposedNote!" compact />
                        </div>
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
                            aria-label="Dismiss this draft"
                          >
                            Dismiss
                          </button>
                        </div>
                      </div>
                    } @else if (turn.accepted) {
                      <span class="added-tag" aria-hidden="true">
                        ✓ Added to notes
                      </span>
                    } @else if (turn.dismissed) {
                      <span class="dismissed-tag" aria-hidden="true">Dismissed</span>
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
                [placeholder]="n.threadPending ? 'Thinking…' : 'Reply to brain…'"
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
                  <svg
                    width="14"
                    height="14"
                    viewBox="0 0 24 24"
                    fill="none"
                    aria-hidden="true"
                  >
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

      /* ── A plain note — the user's own jotting, quiet + clearly "a note" ──── */
      .note-line {
        display: flex;
        align-items: baseline;
        gap: var(--space-2);
        padding: var(--space-1) var(--space-2);
        border-left: 2px solid var(--border-subtle);
        border-radius: var(--radius-sm);
        background: var(--surface-input);
        font-size: 0.92rem;
        line-height: 1.55;
        color: var(--text-secondary);
      }
      .note-glyph {
        flex: 0 0 auto;
        display: inline-flex;
        align-items: center;
        color: var(--text-muted);
        transform: translateY(2px);
      }
      .line-text {
        flex: 1 1 auto;
        min-width: 0;
        white-space: pre-wrap;
        word-break: break-word;
      }

      /* ── "✨ ask brain" — a quiet affordance on a plain note (no thread yet).
         Revealed on row hover OR keyboard focus (a11y); stays clear of floating
         overlays (it's in-flow, so trap T3 N/A). var(--token) only. ──────────── */
      .ask-brain {
        flex: 0 0 auto;
        display: inline-flex;
        align-items: center;
        gap: 4px;
        align-self: center;
        padding: 2px var(--space-2);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-pill);
        background: var(--accent-soft);
        color: var(--accent-hover);
        font-size: 0.72rem;
        font-weight: 600;
        cursor: pointer;
        opacity: 0;
        transition:
          opacity var(--transition),
          background var(--transition),
          color var(--transition);
      }
      /* Reveal on hover of the whole note line, or whenever the button is focused. */
      .note-line:hover .ask-brain,
      .ask-brain:focus-visible {
        opacity: 1;
      }
      .ask-brain:hover {
        background: var(--accent);
        color: #fff;
      }
      .ask-brain:focus-visible {
        outline: none;
        box-shadow: 0 0 0 2px var(--accent-ring);
      }
      /* While notes are still hydrating (loaded() false) the affordance is inert;
         keep it muted + non-interactive even if the row is hovered. */
      .note-line:hover .ask-brain:disabled {
        opacity: 0.4;
        cursor: default;
      }
      .ask-spark {
        font-size: 0.78rem;
        line-height: 1;
      }
      /* Touch / coarse-pointer devices have no hover — keep it always visible. */
      @media (hover: none) {
        .ask-brain {
          opacity: 0.7;
        }
      }

      /* ── @brain THREAD toggle (Slack-style header): caret + "Thread (N)" + a
         short snippet of the user's question when collapsed. The question itself
         renders as the user's first RIGHT-aligned bubble once expanded. ───────── */
      .thread-toggle {
        display: flex;
        align-items: center;
        gap: 5px;
        width: 100%;
        padding: var(--space-1) var(--space-2);
        border: none;
        border-radius: var(--radius-sm);
        background: transparent;
        color: var(--text-muted);
        font: inherit;
        font-size: 0.78rem;
        font-weight: 600;
        text-align: left;
        cursor: pointer;
        transition:
          color var(--transition),
          background var(--transition);
      }
      .thread-toggle:hover {
        color: var(--accent-hover);
        background: var(--surface-hover);
      }
      .thread-toggle:focus-visible {
        outline: none;
        box-shadow: 0 0 0 2px var(--accent-ring);
      }
      .caret {
        flex: 0 0 auto;
        font-size: 0.7rem;
        transition: transform var(--transition);
      }
      .caret.is-open {
        transform: rotate(90deg);
      }
      .toggle-label {
        flex: 0 0 auto;
        letter-spacing: 0.01em;
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
      /* A muted one-line preview of the question while collapsed. */
      .toggle-snippet {
        flex: 1 1 auto;
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        font-weight: 450;
        color: var(--text-muted);
      }

      /* ── The nested thread — indented under the toggle (Slack-style) ─────── */
      .thread {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        margin-left: var(--space-3);
        padding-left: var(--space-3);
        border-left: 2px solid var(--border-subtle);
      }
      .turn {
        display: flex;
        flex-direction: column;
      }
      .turn-user {
        align-items: flex-end;
      }
      .turn-agent {
        align-items: flex-start;
      }
      .agent-id {
        display: inline-flex;
        align-items: center;
        gap: 5px;
        margin: 0 0 3px 2px;
      }
      .agent-mark {
        font-size: 0.8rem;
        line-height: 1;
      }
      .agent-name {
        color: var(--accent-hover);
        font-size: 0.72rem;
        font-weight: 700;
        letter-spacing: 0.04em;
        text-transform: lowercase;
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

      /* the clean "Thinking…" state */
      .thinking {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        color: var(--text-muted);
      }
      .thinking-label {
        font-size: 0.82rem;
      }

      /* ── propose → accept affordance (only for a NOTE proposal) ──────────── */
      .proposal {
        display: flex;
        flex-direction: column;
        gap: 4px;
        margin-top: 2px;
        padding-top: var(--space-2);
        border-top: 1px solid var(--border-subtle);
      }
      .proposal-tag {
        color: var(--text-muted);
        font-size: 0.72rem;
        font-weight: 600;
        letter-spacing: 0.02em;
      }
      /* The actual draft the user reviews before accepting — a quiet quoted card
         so the CONTENT (not just the agent's meta summary) is what they see. */
      .draft-preview {
        padding: var(--space-2) var(--space-3);
        border-left: 3px solid var(--accent);
        border-radius: var(--radius-sm);
        background: var(--accent-soft);
        color: var(--text-primary);
        font-size: 0.86rem;
        line-height: 1.5;
      }
      .turn-actions {
        display: flex;
        align-items: center;
        gap: var(--space-2);
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
        color: var(--accent-hover);
        font-size: 0.74rem;
        font-weight: 600;
        letter-spacing: 0.02em;
      }
      /* Dismiss → a muted "Dismissed" tag (NOT "✓ Added to notes" — nothing saved). */
      .dismissed-tag {
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

  /**
   * The turns rendered INSIDE the expanded thread, EXCLUDING the leading user
   * turn — that is the ANCHOR question, which the template renders explicitly as
   * the user's first right-aligned bubble (so it shows ONCE, no duplication).
   * Follow-up user turns + every agent turn still render here.
   */
  protected readonly visibleTurns = computed<ThreadTurn[]>(() => {
    const thread = this.note().thread ?? [];
    if (thread.length > 0 && thread[0].role === "user") {
      return thread.slice(1);
    }
    return thread;
  });

  /** Number of agent replies (for the collapsed "N" badge). */
  protected readonly replyCount = computed(
    () => this.note().thread?.filter((t) => t.role === "agent").length ?? 0,
  );

  /** A short one-line preview of the question shown on the collapsed toggle. */
  protected readonly snippet = computed(() => {
    const q = this.note().text.replace(/\s+/g, " ").trim();
    return q.length > 64 ? `${q.slice(0, 63)}…` : q;
  });

  /** Follow-up send is allowed with non-blank text and no in-flight thread turn. */
  protected readonly canFollow = computed(
    () => !this.note().threadPending && this.followDraft().trim().length > 0,
  );

  /**
   * Whether an agent turn can be accepted into the notes: ONLY when it carries a
   * `proposedNote` (the agent decided the user asked for a note) and has NOT been
   * accepted or dismissed. A plain answer (`proposedNote === null`) is never
   * acceptable → no "Add to notes" button.
   */
  protected canAccept(turn: ThreadTurn): boolean {
    return (
      turn.role === "agent" &&
      turn.status !== "pending" &&
      !turn.accepted &&
      !turn.dismissed &&
      turn.proposedNote !== null &&
      turn.proposedNote.trim().length > 0
    );
  }

  /**
   * The tool-trace chips to RENDER. Filters out INTERNAL mechanisms the user
   * should never see as "tools" — `propose_note` is the note-proposal plumbing,
   * not a user-facing search/lookup; the Draft-note card already conveys it (and
   * filtering it also drops the duplicated "propose_note" chips). Other tools
   * (search_meetings, web_search, …) still surface.
   */
  protected visibleTrace(trace: ThreadTurn["trace"]): ThreadTurn["trace"] {
    return trace.filter((t) => t.tool !== "propose_note");
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
    afterNextRender(() => this.followInput()?.nativeElement.focus(), {
      injector: this.injector,
    });
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
