import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  effect,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { MeetingChatStore } from "../../core/meeting-chat.store";
import { MarkdownComponent } from "../../shared/markdown.component";
import { AssistantSourcesComponent } from "../../shared/assistant-sources.component";

/**
 * The dedicated in-meeting CHAT panel — a MULTI-TURN conversation with the brain,
 * slid in from the right during recording (separate from the quick-Q&A assistant
 * card). Follow-ups remember the conversation (the {@link MeetingChatStore} ships
 * the full history each turn). Each assistant bubble shows its live tool-trace
 * ("Searching notes… ✓") then the sanitized-markdown answer + grounding sources.
 *
 * It FLOATS over the record content, so per trap T3 it uses the OPAQUE
 * `var(--surface-overlay)` (never the translucent `.card`) — otherwise the meeting
 * UI would bleed through the conversation.
 */
@Component({
  selector: "app-meeting-chat-panel",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MarkdownComponent, AssistantSourcesComponent],
  template: `
    @if (!store.open()) {
      <button
        class="chat-fab"
        type="button"
        (click)="store.openPanel()"
        aria-label="Open meeting chat"
      >
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <path
            d="M21 11.5a8.38 8.38 0 0 1-8.5 8.5 8.5 8.5 0 0 1-3.9-.9L3 21l1.9-5.6a8.5 8.5 0 0 1-.9-3.9A8.38 8.38 0 0 1 12.5 3 8.38 8.38 0 0 1 21 11.5Z"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
        <span class="chat-fab-label">Chat</span>
        @if (store.hasMessages()) {
          <span class="chat-fab-dot" aria-hidden="true"></span>
        }
      </button>
    }

    @if (store.open()) {
      <aside class="chat-panel" role="dialog" aria-label="Meeting chat">
        <header class="chat-head">
          <span class="chat-title">Chat</span>
          <span class="chat-sub text-muted">grounded in this meeting + your vault</span>
          <span class="chat-head-actions">
            @if (store.hasMessages()) {
              <button class="btn btn-ghost chat-clear" type="button" (click)="store.clear()">
                Clear
              </button>
            }
            <button class="chat-close" type="button" aria-label="Close chat" (click)="store.closePanel()">
              ✕
            </button>
          </span>
        </header>

        <div class="chat-thread" #thread>
          @if (!store.hasMessages()) {
            <p class="chat-empty text-muted">
              Ask anything about this meeting or your past notes. Follow-ups
              remember the conversation.
            </p>
          }
          @for (m of store.messages(); track m.id) {
            @if (m.role === "user") {
              <div class="msg msg-user">
                <div class="bubble bubble-user">{{ m.text }}</div>
              </div>
            } @else {
              <div class="msg msg-bot">
                <div class="bubble bubble-bot">
                  @if (m.trace.length > 0) {
                    <div class="trace" role="status" aria-label="Tool use">
                      @for (t of m.trace; track t.id) {
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
                  @if (m.status === "pending" && m.trace.length === 0) {
                    <span class="dots" aria-hidden="true">
                      <span></span><span></span><span></span>
                    </span>
                  } @else if (m.text) {
                    <app-markdown [markdown]="m.text" compact />
                  }
                  @if (m.citations.length > 0) {
                    <app-assistant-sources [citations]="m.citations" />
                  }
                </div>
              </div>
            }
          }
        </div>

        <form class="chat-composer" (submit)="submit($event)">
          <textarea
            class="chat-input"
            rows="1"
            autocomplete="off"
            [value]="draft()"
            (input)="draft.set($any($event.target).value)"
            (keydown.enter)="onEnter($event)"
            [placeholder]="store.pending() ? 'Working…' : 'Message the assistant…'"
          ></textarea>
          <button
            type="submit"
            class="btn btn-primary chat-send"
            [disabled]="!canSend()"
            aria-label="Send message"
          >
            @if (store.pending()) {
              <span class="chat-spin" aria-hidden="true"></span>
            } @else {
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                <path
                  d="M5 12h14M13 6l6 6-6 6"
                  stroke="currentColor"
                  stroke-width="2.2"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                />
              </svg>
            }
          </button>
        </form>
      </aside>
    }
  `,
  styles: [
    `
      .chat-fab {
        position: fixed;
        right: var(--space-5);
        bottom: var(--space-5);
        z-index: 49;
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        height: 44px;
        padding: 0 var(--space-4);
        border-radius: var(--radius-pill);
        border: 1px solid transparent;
        background: var(--accent-gradient);
        color: var(--text-on-accent);
        font-size: 0.9rem;
        font-weight: 600;
        cursor: pointer;
        box-shadow: var(--shadow-accent), var(--shadow-lg);
        transition: transform var(--transition-fast), box-shadow var(--transition);
        animation: chat-in 220ms var(--transition) both;
      }
      .chat-fab:hover {
        transform: translateY(-1px);
      }
      .chat-fab-dot {
        width: 7px;
        height: 7px;
        border-radius: 50%;
        background: var(--text-on-accent);
        opacity: 0.85;
      }
      .chat-panel {
        position: fixed;
        top: 0;
        right: 0;
        bottom: 0;
        width: min(420px, 100vw);
        display: flex;
        flex-direction: column;
        background: var(--surface-overlay);
        border-left: 1px solid var(--border-strong);
        box-shadow: var(--shadow-lg);
        z-index: 50;
        animation: chat-in 220ms var(--transition) both;
      }
      @keyframes chat-in {
        from {
          transform: translateX(16px);
          opacity: 0;
        }
      }
      .chat-head {
        display: flex;
        align-items: baseline;
        gap: var(--space-2);
        padding: var(--space-4) var(--space-4) var(--space-3);
        border-bottom: 1px solid var(--border-subtle);
      }
      .chat-title {
        color: var(--text-primary);
        font-weight: 650;
        font-size: 1rem;
      }
      .chat-sub {
        font-size: 0.75rem;
      }
      .chat-head-actions {
        margin-left: auto;
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
      }
      .chat-clear {
        height: 30px;
        padding: 0 var(--space-3);
        font-size: 0.8rem;
      }
      .chat-close {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 30px;
        height: 30px;
        border-radius: var(--radius-md);
        border: 1px solid var(--glass-border);
        background: transparent;
        color: var(--text-secondary);
        cursor: pointer;
        transition: background var(--transition), color var(--transition);
      }
      .chat-close:hover {
        background: rgba(255, 255, 255, 0.06);
        color: var(--text-primary);
      }

      .chat-thread {
        flex: 1;
        overflow-y: auto;
        padding: var(--space-4);
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .chat-empty {
        margin: auto 0;
        text-align: center;
        font-size: 0.875rem;
        line-height: 1.55;
        padding: 0 var(--space-3);
      }
      .msg {
        display: flex;
      }
      .msg-user {
        justify-content: flex-end;
      }
      .msg-bot {
        justify-content: flex-start;
      }
      .bubble {
        max-width: 86%;
        padding: var(--space-3);
        border-radius: var(--radius-lg);
        font-size: 0.9rem;
        line-height: 1.5;
      }
      .bubble-user {
        background: var(--accent-gradient);
        color: var(--text-on-accent);
        border-bottom-right-radius: var(--radius-sm);
        white-space: pre-wrap;
      }
      .bubble-bot {
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
        color: var(--text-primary);
        border-bottom-left-radius: var(--radius-sm);
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }

      .trace {
        display: flex;
        flex-wrap: wrap;
        gap: 6px;
      }
      .trace-chip {
        display: inline-flex;
        align-items: center;
        gap: 5px;
        padding: 2px 8px;
        border-radius: var(--radius-pill);
        background: var(--accent-soft);
        border: 1px solid var(--border-subtle);
        color: var(--text-secondary);
        font-size: 0.72rem;
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
        animation: chat-spin 0.7s linear infinite;
      }
      @keyframes chat-spin {
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
        animation: chat-blink 1.2s ease-in-out infinite both;
      }
      .dots span:nth-child(2) {
        animation-delay: 0.2s;
      }
      .dots span:nth-child(3) {
        animation-delay: 0.4s;
      }
      @keyframes chat-blink {
        0%,
        80%,
        100% {
          opacity: 0.3;
        }
        40% {
          opacity: 1;
        }
      }

      .chat-composer {
        display: flex;
        align-items: flex-end;
        gap: var(--space-2);
        padding: var(--space-3) var(--space-4) var(--space-4);
        border-top: 1px solid var(--border-subtle);
      }
      .chat-input {
        flex: 1;
        min-height: 40px;
        max-height: 140px;
        padding: var(--space-2) var(--space-3);
        resize: none;
        line-height: 1.45;
        font-size: 0.9rem;
      }
      .chat-send {
        flex: 0 0 auto;
        width: 40px;
        height: 40px;
        padding: 0;
        justify-content: center;
      }
      .chat-send:disabled {
        opacity: 0.5;
        cursor: default;
      }
      .chat-spin {
        width: 15px;
        height: 15px;
        border-radius: 50%;
        border: 2px solid rgba(255, 255, 255, 0.35);
        border-top-color: var(--text-on-accent);
        animation: chat-spin 0.7s linear infinite;
      }

      @media (prefers-reduced-motion: reduce) {
        .chat-panel,
        .trace-spin,
        .chat-spin,
        .dots span {
          animation: none;
        }
        .dots span {
          opacity: 0.7;
        }
      }
    `,
  ],
})
export class MeetingChatPanelComponent {
  protected readonly store = inject(MeetingChatStore);
  private readonly injector = inject(Injector);
  private readonly thread = viewChild<ElementRef<HTMLElement>>("thread");

  protected readonly draft = signal("");
  protected readonly canSend = computed(
    () => !this.store.pending() && this.draft().trim().length > 0,
  );

  constructor() {
    // Auto-scroll the thread to the newest message whenever the conversation
    // changes or the panel opens. Tracks signals in the effect, schedules the DOM
    // work via afterNextRender (zoneless-safe; no signal writes → no NG0600).
    effect(() => {
      this.store.messages();
      this.store.open();
      afterNextRender(
        () => {
          const el = this.thread()?.nativeElement;
          if (el) el.scrollTop = el.scrollHeight;
        },
        { injector: this.injector },
      );
    });
  }

  protected submit(event: Event): void {
    event.preventDefault();
    const text = this.draft().trim();
    if (!text || this.store.pending()) return;
    this.draft.set("");
    void this.store.send(text).catch(() => {
      /* the store surfaces the error on the assistant bubble */
    });
  }

  protected onEnter(event: Event): void {
    const ke = event as KeyboardEvent;
    if (ke.shiftKey) return;
    this.submit(event);
  }

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
}
