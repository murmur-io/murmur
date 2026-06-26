import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  inject,
  input,
  signal,
  viewChild,
} from "@angular/core";
import { IpcService } from "../../core/ipc.service";
import type { ChatTurn } from "../../core/models";
import { MarkdownComponent } from "../../shared/markdown.component";

/** The starter prompts shown in the empty state — tap to ask immediately. */
const STARTERS: readonly string[] = [
  "Summarize the key decisions",
  "What are my action items?",
  "What questions were left open?",
];

/**
 * "Chat with this meeting" — a grounded Q&A panel over a single meeting's
 * transcript. It is a presentational sibling of the timeline + analysis cards:
 * the parent owns the meeting; this component owns only the conversation it
 * builds via {@link IpcService.chatMeeting}, which answers strictly from that
 * meeting's transcript.
 *
 * Lives in its own file so its inline styles get their own per-component
 * `anyComponentStyle` budget (the detail component's styles are near the cap).
 *
 * Assistant replies are rendered as PLAIN TEXT with `white-space: pre-wrap`
 * (no markdown lib, no innerHTML/DomSanitizer) — line breaks + spacing from the
 * model are preserved verbatim and safely.
 */
@Component({
  selector: "app-meeting-chat",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MarkdownComponent],
  template: `
    <div class="chat card">
      <div class="chat-head">
        <div class="chat-head-text">
          <h3 class="chat-title">Ask about this meeting</h3>
          <span class="chat-sub"
            >Answers are grounded only in this transcript</span
          >
        </div>
        @if (conversation().length) {
          <button
            type="button"
            class="btn btn-ghost chat-clear"
            (click)="clear()"
            [disabled]="pending()"
          >
            Clear
          </button>
        }
      </div>

      <!-- Message list (also the scroll region) -->
      <div
        #scroller
        class="chat-log"
        role="log"
        aria-live="polite"
        aria-label="Conversation"
      >
        @if (conversation().length === 0 && !pending()) {
          <!-- Empty state: a friendly prompt + tappable starter chips. -->
          <div class="chat-empty">
            <span class="chat-empty-mark" aria-hidden="true"></span>
            <p class="chat-empty-title">Chat with this meeting</p>
            <p class="chat-empty-copy">
              Ask anything about what was said — decisions, owners, follow-ups.
            </p>
            <div
              class="chat-starters"
              role="group"
              aria-label="Suggested questions"
            >
              @for (s of starters; track s) {
                <button
                  type="button"
                  class="chat-chip"
                  [style.--i]="$index"
                  (click)="ask(s)"
                >
                  {{ s }}
                </button>
              }
            </div>
          </div>
        } @else {
          @for (turn of conversation(); track $index) {
            <div
              class="chat-row"
              [class.is-user]="turn.role === 'user'"
              [class.is-assistant]="turn.role === 'assistant'"
            >
              <div
                class="chat-bubble"
                [attr.aria-label]="
                  (turn.role === 'user' ? 'You' : 'Assistant') + ' said'
                "
              >
                @if (turn.role === "assistant") {
                  <app-markdown [markdown]="turn.content" compact />
                } @else {
                  {{ turn.content }}
                }
              </div>
            </div>
          }

          <!-- "Thinking…" typing indicator while a reply is in flight. -->
          @if (pending()) {
            <div class="chat-row is-assistant">
              <div class="chat-bubble chat-typing" aria-label="Thinking">
                <span class="chat-dot"></span>
                <span class="chat-dot"></span>
                <span class="chat-dot"></span>
              </div>
            </div>
          }
        }

        <!-- Inline error with a retry affordance (keeps the question intact). -->
        @if (error(); as err) {
          <div class="chat-error" role="alert">
            <span class="chat-error-text">{{ err }}</span>
            <button
              type="button"
              class="btn btn-ghost chat-retry"
              (click)="retry()"
              [disabled]="pending()"
            >
              Retry
            </button>
          </div>
        }
      </div>

      <!-- Composer: textarea (Enter sends · Shift+Enter newline) + Send. -->
      <form class="chat-composer" (submit)="onSubmit($event)">
        <textarea
          #input
          class="chat-input"
          rows="1"
          autocapitalize="sentences"
          autocomplete="off"
          spellcheck="true"
          aria-label="Your question"
          placeholder="Ask about this meeting…"
          [value]="draft()"
          [disabled]="pending()"
          (input)="onDraftInput($event)"
          (keydown)="onKeydown($event)"
        ></textarea>
        <button
          type="submit"
          class="btn btn-primary chat-send"
          [disabled]="!canSend()"
          [attr.aria-label]="pending() ? 'Sending' : 'Send'"
        >
          @if (pending()) {
            <span class="chat-send-spin" aria-hidden="true"></span>
          } @else {
            <span class="chat-send-arrow" aria-hidden="true"></span>
          }
        </button>
      </form>
    </div>
  `,
  styles: [
    `
      :host {
        display: block;
      }

      .chat {
        position: relative;
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
        padding: var(--space-5);
        overflow: hidden;
        animation: rise 420ms var(--transition) both;
      }
      /* A faint aurora wash to lift the glass above the page surface. */
      .chat::before {
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
      .chat > * {
        position: relative;
        z-index: 1;
      }

      /* --- Head --- */
      .chat-head {
        display: flex;
        align-items: flex-start;
        justify-content: space-between;
        gap: var(--space-3);
      }
      .chat-head-text {
        display: flex;
        flex-direction: column;
        gap: 2px;
        min-width: 0;
      }
      .chat-title {
        margin: 0;
      }
      .chat-sub {
        color: var(--text-muted);
        font-size: 0.8125rem;
      }
      .chat-clear {
        flex: none;
        height: 32px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
      }

      /* --- Message log (scroll region) --- */
      .chat-log {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        max-height: 440px;
        min-height: 132px;
        overflow-y: auto;
        padding: var(--space-1) var(--space-1) var(--space-2);
        scroll-behavior: smooth;
        overscroll-behavior: contain;
      }

      /* --- Empty state --- */
      .chat-empty {
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: var(--space-2);
        margin: auto;
        padding: var(--space-5) var(--space-4);
        text-align: center;
      }
      .chat-empty-mark {
        width: 40px;
        height: 40px;
        margin-bottom: var(--space-1);
        border-radius: var(--radius-pill);
        background: var(--accent-soft);
        border: 1px solid var(--glass-border);
        box-shadow: var(--glass-highlight);
      }
      .chat-empty-title {
        margin: 0;
        color: var(--text-primary);
        font-weight: 600;
      }
      .chat-empty-copy {
        margin: 0;
        max-width: 42ch;
        color: var(--text-muted);
        font-size: 0.875rem;
      }
      .chat-starters {
        display: flex;
        flex-wrap: wrap;
        justify-content: center;
        gap: var(--space-2);
        margin-top: var(--space-2);
      }
      .chat-chip {
        padding: var(--space-2) var(--space-3);
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-pill);
        background: rgba(255, 255, 255, 0.05);
        color: var(--text-secondary);
        font-family: inherit;
        font-size: 0.8125rem;
        font-weight: 550;
        line-height: 1.2;
        cursor: pointer;
        box-shadow: var(--glass-highlight);
        animation: rise 360ms var(--transition) both;
        animation-delay: calc(var(--i, 0) * 60ms + 80ms);
        transition:
          background var(--transition),
          border-color var(--transition),
          color var(--transition),
          transform var(--transition-fast);
      }
      .chat-chip:hover {
        background: var(--surface-hover);
        border-color: var(--border-strong);
        color: var(--text-primary);
      }
      .chat-chip:active {
        transform: translateY(1px);
      }
      .chat-chip:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }

      /* --- Message rows + bubbles --- */
      .chat-row {
        display: flex;
        max-width: 100%;
        animation: bubble-in 320ms var(--ease-spring) both;
      }
      .chat-row.is-user {
        justify-content: flex-end;
      }
      .chat-row.is-assistant {
        justify-content: flex-start;
      }
      .chat-bubble {
        max-width: 82%;
        padding: var(--space-3) var(--space-4);
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-lg);
        font-size: 0.9375rem;
        line-height: 1.6;
        /* Preserve the model's line breaks + spacing as plain text. */
        white-space: pre-wrap;
        overflow-wrap: anywhere;
      }
      .is-user .chat-bubble {
        background: var(--accent-gradient);
        border-color: transparent;
        color: var(--text-on-accent);
        border-bottom-right-radius: var(--radius-sm);
        box-shadow: var(--shadow-accent), var(--glass-highlight);
      }
      .is-assistant .chat-bubble {
        background: var(--surface-raised);
        -webkit-backdrop-filter: blur(var(--glass-blur))
          saturate(var(--glass-saturate));
        backdrop-filter: blur(var(--glass-blur)) saturate(var(--glass-saturate));
        color: var(--text-primary);
        border-bottom-left-radius: var(--radius-sm);
        box-shadow: var(--glass-highlight);
        /* Markdown renders its own block layout — don't let pre-wrap inject blank lines. */
        white-space: normal;
      }

      /* --- Typing indicator --- */
      .chat-typing {
        display: inline-flex;
        align-items: center;
        gap: 5px;
        padding: var(--space-3) var(--space-4);
      }
      .chat-dot {
        width: 7px;
        height: 7px;
        border-radius: 50%;
        background: var(--text-muted);
        animation: typing 1.2s ease-in-out infinite;
      }
      .chat-dot:nth-child(2) {
        animation-delay: 0.18s;
      }
      .chat-dot:nth-child(3) {
        animation-delay: 0.36s;
      }

      /* --- Inline error + retry --- */
      .chat-error {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-2) var(--space-3);
        border: 1px solid rgba(255, 107, 107, 0.3);
        border-radius: var(--radius-md);
        background: var(--danger-soft);
        animation: rise 240ms var(--transition) both;
      }
      .chat-error-text {
        flex: 1 1 auto;
        min-width: 0;
        color: var(--text-primary);
        font-size: 0.875rem;
      }
      .chat-retry {
        flex: none;
        height: 30px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
      }

      /* --- Composer --- */
      .chat-composer {
        display: flex;
        align-items: flex-end;
        gap: var(--space-2);
      }
      .chat-input {
        flex: 1 1 auto;
        min-width: 0;
        min-height: 44px;
        max-height: 168px;
        padding: var(--space-3) var(--space-4);
        line-height: 1.5;
        resize: none;
      }
      .chat-send {
        flex: none;
        width: 44px;
        height: 44px;
        padding: 0;
        border-radius: var(--radius-md);
      }
      /* Pure-CSS send glyph (no icon dependency). */
      .chat-send-arrow {
        width: 16px;
        height: 16px;
        background: currentColor;
        -webkit-mask: var(--send-mask) center / contain no-repeat;
        mask: var(--send-mask) center / contain no-repeat;
        --send-mask: url("data:image/svg+xml;charset=utf-8,%3Csvg xmlns='http://www.w3.org/2000/svg' width='24' height='24' viewBox='0 0 24 24' fill='none'%3E%3Cpath d='M4 12L20 4l-4 16-4-7-8-1z' fill='black'/%3E%3C/svg%3E");
      }
      .chat-send-spin {
        width: 16px;
        height: 16px;
        border-radius: 50%;
        border: 2px solid rgba(255, 255, 255, 0.35);
        border-top-color: var(--text-on-accent);
        animation: chat-spin 0.7s linear infinite;
      }

      @keyframes bubble-in {
        from {
          opacity: 0;
          transform: translateY(8px) scale(0.98);
        }
        to {
          opacity: 1;
          transform: translateY(0) scale(1);
        }
      }
      @keyframes typing {
        0%,
        60%,
        100% {
          opacity: 0.35;
          transform: translateY(0);
        }
        30% {
          opacity: 1;
          transform: translateY(-3px);
        }
      }
      @keyframes chat-spin {
        to {
          transform: rotate(360deg);
        }
      }

      @media (prefers-reduced-motion: reduce) {
        .chat,
        .chat-row,
        .chat-chip,
        .chat-error {
          animation: none;
        }
        .chat-dot,
        .chat-send-spin {
          animation-duration: 0.01ms;
        }
        .chat-log {
          scroll-behavior: auto;
        }
      }
    `,
  ],
})
export class MeetingChatComponent {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);

  /** The meeting whose transcript grounds every answer. */
  readonly meetingId = input.required<string>();

  /** The running conversation (optimistic user turns + grounded replies). */
  readonly conversation = signal<ChatTurn[]>([]);
  /** True while a {@link IpcService.chatMeeting} call is in flight. */
  readonly pending = signal(false);
  /** Inline error message (with a Retry affordance); null when clear. */
  readonly error = signal<string | null>(null);
  /** Working copy of the composer text (textarea (input) → signal). */
  readonly draft = signal("");

  /** Starter prompts for the empty state. */
  protected readonly starters = STARTERS;

  /** A submit is allowed only with non-empty text and no in-flight request. */
  readonly canSend = computed(
    () => !this.pending() && this.draft().trim().length > 0,
  );

  /** Mirror the textarea value into the `draft` signal. */
  onDraftInput(event: Event): void {
    this.draft.set((event.target as HTMLTextAreaElement).value);
  }

  /** Enter sends; Shift+Enter inserts a newline (textarea default). */
  onKeydown(event: KeyboardEvent): void {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      void this.send();
    }
  }

  /** Composer form submit (Send button / Enter). */
  onSubmit(event: Event): void {
    event.preventDefault();
    void this.send();
  }

  /** Fill + send a starter chip's question. */
  ask(question: string): void {
    if (this.pending()) {
      return;
    }
    this.draft.set(question);
    void this.send();
  }

  /** Re-send the last user question after an error (it's still in the log). */
  retry(): void {
    if (this.pending()) {
      return;
    }
    const turns = this.conversation();
    const last = turns[turns.length - 1];
    if (last?.role !== "user") {
      this.error.set(null);
      return;
    }
    // Pop the dangling user turn back into the composer, then re-run send()
    // so it captures the correct prior history.
    this.conversation.set(turns.slice(0, -1));
    this.draft.set(last.content);
    void this.send();
  }

  /** Clear the whole conversation back to the empty state. */
  clear(): void {
    if (this.pending()) {
      return;
    }
    this.conversation.set([]);
    this.error.set(null);
  }

  /**
   * Ask the current question. Captures the question + the PRIOR history
   * (the conversation before this turn), optimistically appends the user turn,
   * awaits the grounded reply, then appends the assistant turn. On failure the
   * user's question is kept (an inline Retry re-runs it).
   */
  async send(): Promise<void> {
    const question = this.draft().trim();
    if (!question || this.pending()) {
      return;
    }

    // History as seen by the model = everything BEFORE this turn.
    const priorHistory = this.conversation();

    this.error.set(null);
    this.draft.set("");
    this.conversation.set([
      ...priorHistory,
      { role: "user", content: question },
    ]);
    this.pending.set(true);
    this.scrollToLatest();

    try {
      const answer = await this.ipc.chatMeeting(
        this.meetingId(),
        question,
        priorHistory,
      );
      this.conversation.update((turns) => [
        ...turns,
        { role: "assistant", content: answer },
      ]);
    } catch (e) {
      // Keep the user's question in the log so Retry can re-send it.
      this.error.set("Couldn’t get an answer: " + String(e));
    } finally {
      this.pending.set(false);
      this.scrollToLatest();
    }
  }

  // --- Auto-scroll ---------------------------------------------------------

  /** The scrollable message log. */
  private readonly scroller = viewChild<ElementRef<HTMLDivElement>>("scroller");

  /**
   * Pin the log to the newest message. Runs after the next render so the new
   * row/typing indicator is laid out before we measure scrollHeight — zoneless
   * safe, no setTimeout. afterNextRender registered with this component's
   * injector is a one-shot and is auto-torn-down when the component is
   * destroyed, so there is nothing to clean up manually.
   */
  private scrollToLatest(): void {
    afterNextRender(
      () => {
        const el = this.scroller()?.nativeElement;
        if (el) {
          el.scrollTop = el.scrollHeight;
        }
      },
      { injector: this.injector },
    );
  }
}
