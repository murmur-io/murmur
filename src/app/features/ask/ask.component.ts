import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  OnInit,
  afterNextRender,
  computed,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import { IpcService } from "../../core/ipc.service";
import type { ChatTurn, VaultSource } from "../../core/models";

/**
 * A conversation turn as rendered on the Ask page. It mirrors {@link ChatTurn}
 * but assistant turns also carry the source meetings the answer was grounded in
 * (rendered as chips that deep-link into each meeting).
 */
interface AskTurn {
  role: "user" | "assistant";
  content: string;
  /** Present on assistant turns only — the meetings that grounded the answer. */
  sources?: VaultSource[];
}

/** The starter prompts shown in the empty state — tap to ask immediately. */
const STARTERS: readonly string[] = [
  "What did we decide about …?",
  "What are my open action items across all meetings?",
  "Summarize my last week",
];

/**
 * "Ask your meetings" — a premium full-page chat that answers grounded in the
 * WHOLE vault (every past meeting), via {@link IpcService.askVault}. It is a
 * page-level sibling of meeting-chat (which is scoped to one meeting): the same
 * conversation/composer/auto-scroll/retry language, but answers span all
 * meetings and each reply lists its source meetings as deep-link chips.
 *
 * Lives in its own file so its inline styles get their own per-component
 * `anyComponentStyle` budget.
 *
 * Assistant replies are rendered as PLAIN TEXT with `white-space: pre-wrap`
 * (no markdown lib, no innerHTML/DomSanitizer) — line breaks + spacing from the
 * model are preserved verbatim and safely.
 */
@Component({
  selector: "app-ask",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  template: `
    <section class="ask">
      <header class="ask-head">
        <div class="ask-head-text">
          <h2 class="ask-title">Ask your meetings</h2>
          <p class="ask-intro">
            Ask a question and get an answer grounded across every meeting in
            your vault — with links to the meetings it came from.
          </p>
        </div>
        @if (conversation().length) {
          <button
            type="button"
            class="btn btn-ghost ask-clear"
            (click)="clear()"
            [disabled]="pending()"
          >
            Clear
          </button>
        }
      </header>

      @if (loading()) {
        <div class="card state-card">
          <p class="empty">Loading…</p>
        </div>
      } @else if (isEmpty()) {
        <!-- No meetings in the vault: nothing to ask about yet. -->
        <div class="card empty-state">
          <span class="empty-mark" aria-hidden="true"></span>
          <p class="empty-title">No meetings to ask about yet</p>
          <p class="empty">
            Record your first meeting and you can chat across your whole vault
            here.
          </p>
        </div>
      } @else {
        <div class="ask-panel card">
          <!-- Message list (also the scroll region) -->
          <div
            #scroller
            class="ask-log"
            role="log"
            aria-live="polite"
            aria-label="Conversation"
          >
            @if (conversation().length === 0 && !pending()) {
              <!-- Empty state: a friendly prompt + tappable starter chips. -->
              <div class="ask-empty">
                <span class="ask-empty-mark" aria-hidden="true"></span>
                <p class="ask-empty-title">Chat across all your meetings</p>
                <p class="ask-empty-copy">
                  Ask about decisions, owners, and follow-ups spanning your
                  entire history — answers cite the meetings they came from.
                </p>
                <div
                  class="ask-starters"
                  role="group"
                  aria-label="Suggested questions"
                >
                  @for (s of starters; track s) {
                    <button
                      type="button"
                      class="ask-chip"
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
                  class="ask-row"
                  [class.is-user]="turn.role === 'user'"
                  [class.is-assistant]="turn.role === 'assistant'"
                >
                  <div
                    class="ask-bubble"
                    [attr.aria-label]="
                      (turn.role === 'user' ? 'You' : 'Assistant') + ' said'
                    "
                  >
                    {{ turn.content }}
                  </div>

                  <!-- Source meetings the answer was grounded in (chips). -->
                  @if (turn.role === "assistant" && turn.sources?.length) {
                    <div
                      class="ask-sources"
                      role="group"
                      aria-label="Source meetings"
                    >
                      <span class="ask-sources-label">Sources</span>
                      @for (src of turn.sources; track src.meetingId) {
                        <a
                          class="ask-source"
                          [routerLink]="['/meeting', src.meetingId]"
                        >
                          <span class="ask-source-title">{{
                            src.title || "(untitled)"
                          }}</span>
                          <span class="ask-source-date">{{
                            formatDate(src.startedAt)
                          }}</span>
                        </a>
                      }
                    </div>
                  }
                </div>
              }

              <!-- "Thinking…" typing indicator while a reply is in flight. -->
              @if (pending()) {
                <div class="ask-row is-assistant">
                  <div class="ask-bubble ask-typing" aria-label="Thinking">
                    <span class="ask-dot"></span>
                    <span class="ask-dot"></span>
                    <span class="ask-dot"></span>
                  </div>
                </div>
              }
            }

            <!-- Inline error with a retry affordance (keeps the question). -->
            @if (error(); as err) {
              <div class="ask-error" role="alert">
                <span class="ask-error-text">{{ err }}</span>
                <button
                  type="button"
                  class="btn btn-ghost ask-retry"
                  (click)="retry()"
                  [disabled]="pending()"
                >
                  Retry
                </button>
              </div>
            }
          </div>

          <!-- Composer: textarea (Enter sends · Shift+Enter newline) + Send. -->
          <form class="ask-composer" (submit)="onSubmit($event)">
            <textarea
              #input
              class="ask-input"
              rows="1"
              autocapitalize="sentences"
              autocomplete="off"
              spellcheck="true"
              aria-label="Your question"
              placeholder="Ask anything about your meetings…"
              [value]="draft()"
              [disabled]="pending()"
              (input)="onDraftInput($event)"
              (keydown)="onKeydown($event)"
            ></textarea>
            <button
              type="submit"
              class="btn btn-primary ask-send"
              [disabled]="!canSend()"
              [attr.aria-label]="pending() ? 'Sending' : 'Send'"
            >
              @if (pending()) {
                <span class="ask-send-spin" aria-hidden="true"></span>
              } @else {
                <span class="ask-send-arrow" aria-hidden="true"></span>
              }
            </button>
          </form>
        </div>
      }
    </section>
  `,
  styles: [
    `
      :host {
        display: block;
      }

      .ask {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
      }

      /* --- Head --- */
      .ask-head {
        display: flex;
        align-items: flex-start;
        justify-content: space-between;
        gap: var(--space-4);
      }
      .ask-head-text {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        min-width: 0;
      }
      .ask-title {
        margin: 0;
      }
      .ask-intro {
        margin: 0;
        max-width: 60ch;
        color: var(--text-secondary);
        font-size: 0.9375rem;
        line-height: 1.55;
      }
      .ask-clear {
        flex: none;
        height: 32px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
      }

      /* --- Conversation panel --- */
      .ask-panel {
        position: relative;
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
        padding: var(--space-5);
        overflow: hidden;
        animation: rise 420ms var(--transition) both;
      }
      /* A faint aurora wash to lift the glass above the page surface. */
      .ask-panel::before {
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
      .ask-panel > * {
        position: relative;
        z-index: 1;
      }

      /* --- Message log (scroll region) --- */
      .ask-log {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
        max-height: 56vh;
        min-height: 220px;
        overflow-y: auto;
        padding: var(--space-1) var(--space-1) var(--space-2);
        scroll-behavior: smooth;
        overscroll-behavior: contain;
      }

      /* --- Empty state --- */
      .ask-empty {
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: var(--space-2);
        margin: auto;
        padding: var(--space-6) var(--space-4);
        text-align: center;
      }
      .ask-empty-mark {
        width: 44px;
        height: 44px;
        margin-bottom: var(--space-1);
        border-radius: var(--radius-pill);
        background: var(--accent-soft);
        border: 1px solid var(--glass-border);
        box-shadow: var(--glass-highlight);
      }
      .ask-empty-title {
        margin: 0;
        color: var(--text-primary);
        font-weight: 600;
      }
      .ask-empty-copy {
        margin: 0;
        max-width: 48ch;
        color: var(--text-muted);
        font-size: 0.875rem;
      }
      .ask-starters {
        display: flex;
        flex-wrap: wrap;
        justify-content: center;
        gap: var(--space-2);
        margin-top: var(--space-2);
      }
      .ask-chip {
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
      .ask-chip:hover {
        background: var(--surface-hover);
        border-color: var(--border-strong);
        color: var(--text-primary);
      }
      .ask-chip:active {
        transform: translateY(1px);
      }
      .ask-chip:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }

      /* --- Message rows + bubbles --- */
      .ask-row {
        display: flex;
        flex-direction: column;
        max-width: 100%;
        gap: var(--space-2);
        animation: bubble-in 320ms var(--ease-spring) both;
      }
      .ask-row.is-user {
        align-items: flex-end;
      }
      .ask-row.is-assistant {
        align-items: flex-start;
      }
      .ask-bubble {
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
      .is-user .ask-bubble {
        background: var(--accent-gradient);
        border-color: transparent;
        color: var(--text-on-accent);
        border-bottom-right-radius: var(--radius-sm);
        box-shadow: var(--shadow-accent), var(--glass-highlight);
      }
      .is-assistant .ask-bubble {
        background: var(--surface-raised);
        -webkit-backdrop-filter: blur(var(--glass-blur))
          saturate(var(--glass-saturate));
        backdrop-filter: blur(var(--glass-blur)) saturate(var(--glass-saturate));
        color: var(--text-primary);
        border-bottom-left-radius: var(--radius-sm);
        box-shadow: var(--glass-highlight);
      }

      /* --- Source chips (under an assistant answer) --- */
      .ask-sources {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-2);
        max-width: 100%;
        padding-left: var(--space-1);
      }
      .ask-sources-label {
        color: var(--text-muted);
        font-size: 0.6875rem;
        font-weight: 600;
        letter-spacing: 0.04em;
        text-transform: uppercase;
      }
      .ask-source {
        display: inline-flex;
        align-items: baseline;
        gap: var(--space-2);
        max-width: 100%;
        padding: var(--space-1) var(--space-3);
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        color: var(--text-secondary);
        font-size: 0.8125rem;
        line-height: 1.3;
        text-decoration: none;
        transition:
          background var(--transition),
          border-color var(--transition),
          color var(--transition),
          transform var(--transition-fast);
      }
      .ask-source:hover {
        background: var(--accent-soft);
        border-color: transparent;
        color: var(--accent-hover);
      }
      .ask-source:active {
        transform: translateY(1px);
      }
      .ask-source:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .ask-source-title {
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        max-width: 30ch;
        font-weight: 600;
      }
      .ask-source-date {
        flex: none;
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-variant-numeric: tabular-nums;
        font-size: 0.6875rem;
      }

      /* --- Typing indicator --- */
      .ask-typing {
        display: inline-flex;
        align-items: center;
        gap: 5px;
        padding: var(--space-3) var(--space-4);
      }
      .ask-dot {
        width: 7px;
        height: 7px;
        border-radius: 50%;
        background: var(--text-muted);
        animation: typing 1.2s ease-in-out infinite;
      }
      .ask-dot:nth-child(2) {
        animation-delay: 0.18s;
      }
      .ask-dot:nth-child(3) {
        animation-delay: 0.36s;
      }

      /* --- Inline error + retry --- */
      .ask-error {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-2) var(--space-3);
        border: 1px solid rgba(255, 107, 107, 0.3);
        border-radius: var(--radius-md);
        background: var(--danger-soft);
        animation: rise 240ms var(--transition) both;
      }
      .ask-error-text {
        flex: 1 1 auto;
        min-width: 0;
        color: var(--text-primary);
        font-size: 0.875rem;
      }
      .ask-retry {
        flex: none;
        height: 30px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
      }

      /* --- Composer --- */
      .ask-composer {
        display: flex;
        align-items: flex-end;
        gap: var(--space-2);
      }
      .ask-input {
        flex: 1 1 auto;
        min-width: 0;
        min-height: 44px;
        max-height: 168px;
        padding: var(--space-3) var(--space-4);
        line-height: 1.5;
        resize: none;
      }
      .ask-send {
        flex: none;
        width: 44px;
        height: 44px;
        padding: 0;
        border-radius: var(--radius-md);
      }
      /* Pure-CSS send glyph (no icon dependency). */
      .ask-send-arrow {
        width: 16px;
        height: 16px;
        background: currentColor;
        -webkit-mask: var(--send-mask) center / contain no-repeat;
        mask: var(--send-mask) center / contain no-repeat;
        --send-mask: url("data:image/svg+xml;charset=utf-8,%3Csvg xmlns='http://www.w3.org/2000/svg' width='24' height='24' viewBox='0 0 24 24' fill='none'%3E%3Cpath d='M4 12L20 4l-4 16-4-7-8-1z' fill='black'/%3E%3C/svg%3E");
      }
      .ask-send-spin {
        width: 16px;
        height: 16px;
        border-radius: 50%;
        border: 2px solid rgba(255, 255, 255, 0.35);
        border-top-color: var(--text-on-accent);
        animation: ask-spin 0.7s linear infinite;
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
      @keyframes ask-spin {
        to {
          transform: rotate(360deg);
        }
      }

      @media (prefers-reduced-motion: reduce) {
        .ask-panel,
        .ask-row,
        .ask-chip,
        .ask-error {
          animation: none;
        }
        .ask-dot,
        .ask-send-spin {
          animation-duration: 0.01ms;
        }
        .ask-log {
          scroll-behavior: auto;
        }
      }
    `,
  ],
})
export class AskComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);

  /** True while the initial "any meetings?" probe is in flight. */
  readonly loading = signal(true);
  /** True when the vault has no meetings — the page shows an empty state. */
  readonly isEmpty = signal(false);

  /** The running conversation (optimistic user turns + grounded replies). */
  readonly conversation = signal<AskTurn[]>([]);
  /** True while an {@link IpcService.askVault} call is in flight. */
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

  /**
   * Probe whether there are any meetings to ask about. A failure here is not
   * fatal — we still let the user try (the ask itself surfaces its own error),
   * so we only flip to the empty state on a confirmed empty list.
   */
  async ngOnInit(): Promise<void> {
    try {
      const meetings = await this.ipc.listMeetings();
      this.isEmpty.set(meetings.length === 0);
    } catch {
      this.isEmpty.set(false);
    } finally {
      this.loading.set(false);
    }
  }

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
   * Ask the current question across the whole vault. Captures the question +
   * the PRIOR history (the conversation before this turn, as plain
   * {@link ChatTurn}s the backend expects), optimistically appends the user
   * turn, awaits the grounded reply, then appends the assistant turn with its
   * source meetings. On failure the user's question is kept (Retry re-runs it).
   */
  async send(): Promise<void> {
    const question = this.draft().trim();
    if (!question || this.pending()) {
      return;
    }

    // History as seen by the model = everything BEFORE this turn, reduced to
    // the {role, content} shape the IPC contract takes (drop source metadata).
    const priorHistory: ChatTurn[] = this.conversation().map((t) => ({
      role: t.role,
      content: t.content,
    }));

    this.error.set(null);
    this.draft.set("");
    this.conversation.update((turns) => [
      ...turns,
      { role: "user", content: question },
    ]);
    this.pending.set(true);
    this.scrollToLatest();

    try {
      const result = await this.ipc.askVault(question, priorHistory);
      this.conversation.update((turns) => [
        ...turns,
        {
          role: "assistant",
          content: result.answer,
          sources: result.sources,
        },
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
   * injector is a one-shot torn down with the component, so there is nothing to
   * clean up manually.
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

  /** Presentational only: render a source timestamp as a friendly local date. */
  formatDate(startedAt: string): string {
    const d = new Date(startedAt);
    if (Number.isNaN(d.getTime())) {
      return startedAt;
    }
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  }
}
