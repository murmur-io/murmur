import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
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
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "../../core/ipc.service";
import type {
  AssistantToolPayload,
  ChatTurn,
  VaultSource,
} from "../../core/models";
import { MarkdownComponent } from "../../shared/markdown.component";
import { SourcesComponent } from "../../shared/sources.component";

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

/**
 * One tool the `ask_vault` agentic loop used while answering the IN-FLIGHT
 * question — drives the live trace chips in the typing row ("Searching
 * notes… ✓"). Tool name + a coarse count only (no PII), same visual language
 * as the record-screen thread chips. Cleared when the answer lands.
 */
interface AskTraceStep {
  /** Stable id for `@for` tracking (never key a trace chip on $index). */
  id: number;
  tool: string;
  state: "running" | "done";
  ok: boolean;
  count: number | null;
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
  imports: [RouterLink, MarkdownComponent, SourcesComponent],
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
                    @if (turn.role === "assistant") {
                      <app-markdown [markdown]="turn.content" />
                    } @else {
                      {{ turn.content }}
                    }
                  </div>

                  <!-- Source meetings the answer was grounded in (collapsible). -->
                  @if (turn.role === "assistant" && turn.sources?.length) {
                    <app-sources
                      class="ask-sources"
                      [sources]="turn.sources ?? []"
                    />
                  }
                </div>
              }

              <!-- In-flight reply: live tool-trace chips once the agentic loop
                   starts calling tools, the "Thinking…" dots until then. -->
              @if (pending()) {
                <div class="ask-row is-assistant">
                  @if (trace().length > 0) {
                    <div
                      class="ask-bubble ask-trace"
                      role="status"
                      aria-label="Tool use"
                    >
                      @for (t of trace(); track t.id) {
                        <span
                          class="ask-trace-chip"
                          [class.is-running]="t.state === 'running'"
                          [class.is-web]="t.tool === 'web_search'"
                          [class.is-failed]="!t.ok"
                        >
                          <span class="ask-trace-ico" aria-hidden="true">
                            @if (t.state === "running") {
                              <span class="ask-trace-spin"></span>
                            } @else if (!t.ok) {
                              ⚠
                            } @else {
                              ✓
                            }
                          </span>
                          {{ toolLabel(t.tool) }}
                          @if (t.state === "done" && t.count) {
                            <span class="ask-trace-count">{{ t.count }}</span>
                          }
                        </span>
                      }
                    </div>
                  } @else {
                    <div class="ask-bubble ask-typing" aria-label="Thinking">
                      <span class="ask-dot"></span>
                      <span class="ask-dot"></span>
                      <span class="ask-dot"></span>
                    </div>
                  }
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
        /* Markdown renders its own block layout — don't let pre-wrap inject blank lines. */
        white-space: normal;
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

      /* --- Live tool-trace chips (in-flight question only) --- */
      .ask-trace {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-1);
      }
      .ask-trace-chip {
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
      .ask-trace-chip.is-running {
        color: var(--text-primary);
      }
      .ask-trace-chip.is-web {
        background: color-mix(in srgb, var(--live) 16%, transparent);
        border-color: color-mix(in srgb, var(--live) 35%, transparent);
      }
      .ask-trace-chip.is-failed {
        opacity: 0.6;
      }
      .ask-trace-ico {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 11px;
        height: 11px;
        font-size: 0.66rem;
        color: var(--accent);
      }
      .ask-trace-chip.is-web .ask-trace-ico {
        color: var(--live);
      }
      .ask-trace-count {
        color: var(--text-muted);
        font-variant-numeric: tabular-nums;
      }
      .ask-trace-spin {
        width: 8px;
        height: 8px;
        border-radius: 50%;
        border: 1.5px solid var(--accent-ring);
        border-top-color: var(--accent);
        animation: ask-spin 0.7s linear infinite;
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
        .ask-send-spin,
        .ask-trace-spin {
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
  private readonly destroyRef = inject(DestroyRef);

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

  /** Live tool-trace chips for the IN-FLIGHT question (cleared when it lands). */
  readonly trace = signal<AskTraceStep[]>([]);
  /**
   * The FE-minted thread id of the in-flight question — `murmur://ask-tool`
   * payloads are routed STRICTLY by it (anything else, including unstamped
   * events, is dropped: never mis-file a chip). Plumbing only, never rendered.
   */
  private activeAskId: string | null = null;
  /** Monotonic id source for trace chips (stable `@for` keys). */
  private nextTraceId = 1;
  private unlistenAskTool: UnlistenFn | null = null;

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
    void this.listenAskTool();
    try {
      const meetings = await this.ipc.listMeetings();
      this.isEmpty.set(meetings.length === 0);
    } catch {
      this.isEmpty.set(false);
    } finally {
      this.loading.set(false);
    }
  }

  /**
   * Subscribe ONCE to the Ask page's own tool-trace stream and store the
   * unlisten so DestroyRef can release it (no leaked listener). Best-effort:
   * a failure just means no trace chips — asking still works.
   */
  private async listenAskTool(): Promise<void> {
    try {
      this.unlistenAskTool = await this.ipc.onAskTool((p) => this.onAskTool(p));
      this.destroyRef.onDestroy(() => this.unlistenAskTool?.());
    } catch {
      // Not running under Tauri (browser smoke without the mock): no chips.
    }
  }

  /**
   * Land one `murmur://ask-tool` payload as a trace chip on the in-flight
   * question. Routed STRICTLY by threadId — a payload for another turn (a
   * stale event from the previous question, or an unstamped one) is dropped.
   * A "running" event pushes a new chip; a "done" event resolves the most
   * recent matching running chip (or appends one, if "running" was missed).
   * Plain event callback writing signals — not an effect, so no NG0600.
   */
  private onAskTool(p: AssistantToolPayload): void {
    if (this.activeAskId === null || p.threadId !== this.activeAskId) {
      return;
    }
    // Internal plumbing tools are not user-facing work — never chip them
    // (mirrors the record-screen thread's visibleTrace filter).
    if (p.tool === "propose_note") {
      return;
    }
    this.trace.update((trace) => {
      if (p.state === "running") {
        return [
          ...trace,
          {
            id: this.nextTraceId++,
            tool: p.tool,
            state: "running",
            ok: true,
            count: p.count,
          },
        ];
      }
      const next = trace.slice();
      for (let i = next.length - 1; i >= 0; i--) {
        if (next[i].tool === p.tool && next[i].state === "running") {
          next[i] = { ...next[i], state: "done", ok: p.ok, count: p.count };
          return next;
        }
      }
      next.push({
        id: this.nextTraceId++,
        tool: p.tool,
        state: "done",
        ok: p.ok,
        count: p.count,
      });
      return next;
    });
    // Keep the growing chip row pinned in view like every other new message.
    this.scrollToLatest();
  }

  /** Human label for a tool-trace chip (same wording as the record screen). */
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
   *
   * Each question mints a fresh `askThreadId` that keys its live tool-trace
   * (`murmur://ask-tool` chips route strictly by it); the chips are cleared
   * when the turn lands — the answer's source chips remain the durable record.
   * We ship the FULL conversation as history: the backend caps it at the last
   * 12 messages itself, so no FE-side truncation.
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

    const askThreadId = crypto.randomUUID();
    this.activeAskId = askThreadId;
    this.trace.set([]);
    this.error.set(null);
    this.draft.set("");
    this.conversation.update((turns) => [
      ...turns,
      { role: "user", content: question },
    ]);
    this.pending.set(true);
    this.scrollToLatest();

    try {
      const result = await this.ipc.askVault(
        question,
        priorHistory,
        askThreadId,
      );
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
      // Retire this turn's trace: late tool events for it are dropped.
      this.activeAskId = null;
      this.trace.set([]);
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
