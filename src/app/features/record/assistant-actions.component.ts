import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  OnInit,
  afterNextRender,
  computed,
  effect,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { AssistantStore } from "../../core/assistant.store";
import type { ChatMessage } from "../../core/assistant.store";
import { MarkdownComponent } from "../../shared/markdown.component";
import { AssistantSourcesComponent } from "../../shared/assistant-sources.component";
import { AiOrbComponent } from "./ai-orb.component";

/**
 * The unified in-meeting assistant surface — the single home of the in-meeting
 * BRAIN. One chronological conversation thread fed by BOTH voice and text, with
 * multi-turn memory, cleared on each new recording (by the record screen).
 *
 * Subscribes (once, via {@link AssistantStore.init}) to the wake + result + live
 * tool-trace streams. Renders a scrollable thread (oldest → newest) of user /
 * assistant bubbles, each assistant bubble showing its LIVE tool trace as the
 * brain works ("Searching notes… ✓", "Checking the web…") then a SANITIZED
 * markdown answer (`app-markdown`) + a deduped "🔗 Źródła" block
 * (`app-assistant-sources`). The INPUT is pinned at the foot: the voice mic
 * (askNow/endAsk) + the text composer (send) side by side — speech and text
 * share one thread, one orb, one trace.
 *
 * The surface is IN-FLOW on the record page (not a floating overlay), so the
 * frosted `.card` is correct here (trap T3 applies only to floating popovers —
 * this is intentionally NOT floated, unlike the deleted slide-out chat panel).
 */
@Component({
  selector: "app-assistant-actions",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [AiOrbComponent, MarkdownComponent, AssistantSourcesComponent],
  template: `
    <div class="card assistant" role="group" aria-label="In-meeting assistant">
      <div class="assistant-head">
        <app-ai-orb class="head-orb" [state]="store.orbState()" />
        <span class="assistant-title">Assistant</span>
        <span class="pill is-live assistant-live" aria-hidden="true">
          <span class="pill-dot"></span>
          LIVE
        </span>
      </div>

      <div class="thread" #thread>
        @if (!store.hasMessages()) {
          <p class="thread-empty text-muted">
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
                } @else if (m.status !== "pending") {
                  <span class="bubble-note">{{ emptyNote(m.status) }}</span>
                }
                @if (m.citations.length > 0) {
                  <app-assistant-sources [citations]="m.citations" />
                }
              </div>
            </div>
          }
        }
      </div>

      <form class="composer" (submit)="submit($event)">
        <button
          type="button"
          class="mic-btn"
          [class.is-listening]="store.listening()"
          [disabled]="store.processing()"
          (click)="toggleAsk()"
          [attr.aria-pressed]="store.listening()"
          [attr.aria-label]="
            store.listening() ? 'Stop listening and ask' : 'Ask by voice'
          "
          [title]="store.listening() ? 'Stop & ask' : 'Ask by voice'"
        >
          <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true">
            <path
              d="M12 3l1.6 4.4L18 9l-4.4 1.6L12 15l-1.6-4.4L6 9l4.4-1.6L12 3z"
              fill="currentColor"
            />
            <path
              d="M18.5 14l.8 2.2 2.2.8-2.2.8-.8 2.2-.8-2.2-2.2-.8 2.2-.8.8-2.2z"
              fill="currentColor"
              opacity="0.85"
            />
          </svg>
        </button>
        <textarea
          class="composer-input"
          rows="1"
          autocomplete="off"
          spellcheck="false"
          [placeholder]="store.processing() ? 'Working…' : 'Ask the assistant…'"
          [value]="draft()"
          (input)="draft.set($any($event.target).value)"
          (keydown.enter)="onEnter($event)"
        ></textarea>
        <button
          type="submit"
          class="btn btn-primary composer-send"
          [disabled]="!canSend()"
          aria-label="Send question"
          title="Send (Enter)"
        >
          @if (store.processing()) {
            <span class="composer-spin" aria-hidden="true"></span>
          } @else {
            <svg
              width="16"
              height="16"
              viewBox="0 0 24 24"
              fill="none"
              aria-hidden="true"
            >
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
    </div>
  `,
  styles: [
    `
      .assistant {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .assistant-head {
        display: flex;
        align-items: center;
        gap: var(--space-2);
      }
      .head-orb {
        --orb-size: 22px;
      }
      .assistant-title {
        color: var(--text-primary);
        font-weight: 600;
        font-size: 0.95rem;
      }
      .assistant-live {
        margin-left: auto;
      }

      /* ── The scrollable conversation thread (oldest → newest) ─────────── */
      .thread {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        max-height: 420px;
        overflow-y: auto;
        padding-right: var(--space-1);
      }
      .thread-empty {
        margin: 0;
        font-size: 0.875rem;
        line-height: 1.55;
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
        animation: rise 220ms var(--transition) both;
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
      .bubble-note {
        color: var(--text-secondary);
        font-size: 0.875rem;
      }

      /* ── live tool trace ──────────────────────────────────────────── */
      .trace {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-2);
      }
      .trace-chip {
        display: inline-flex;
        align-items: center;
        gap: 6px;
        padding: 3px var(--space-2);
        border-radius: var(--radius-pill);
        background: var(--accent-soft);
        border: 1px solid var(--border-subtle);
        color: var(--text-secondary);
        font-size: 0.78rem;
        line-height: 1.2;
        transition: opacity var(--transition);
      }
      .trace-chip.is-running {
        color: var(--text-primary);
      }
      .trace-chip.is-web {
        /* the loud "off-device" tint — web is the one egressing tool */
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
        width: 12px;
        height: 12px;
        font-size: 0.7rem;
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
        width: 9px;
        height: 9px;
        border-radius: 50%;
        border: 1.5px solid var(--accent-ring);
        border-top-color: var(--accent);
        animation: trace-spin 0.7s linear infinite;
      }
      @keyframes trace-spin {
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

      /* ── input row: voice mic + text composer, pinned at the foot ─────── */
      .composer {
        display: flex;
        align-items: flex-end;
        gap: var(--space-2);
      }
      .mic-btn {
        flex: 0 0 auto;
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 40px;
        height: 40px;
        border: 1px solid var(--accent-ring);
        border-radius: 50%;
        color: var(--accent-hover);
        background: var(--accent-soft);
        cursor: pointer;
        transition:
          transform var(--transition-fast),
          background var(--transition),
          box-shadow var(--transition),
          color var(--transition);
      }
      .mic-btn:hover:not(:disabled) {
        background: var(--accent);
        color: #fff;
        transform: scale(1.05);
      }
      .mic-btn:active:not(:disabled) {
        transform: scale(0.96);
      }
      .mic-btn:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .mic-btn.is-listening {
        background: var(--accent-gradient);
        color: #fff;
        border-color: transparent;
        animation: mic-pulse 1.5s ease-in-out infinite;
      }
      .mic-btn:disabled {
        opacity: 0.55;
        cursor: default;
      }
      @keyframes mic-pulse {
        0%,
        100% {
          box-shadow: 0 0 0 0 var(--accent-ring);
        }
        50% {
          box-shadow: 0 0 0 8px rgba(110, 118, 255, 0);
        }
      }
      .composer-input {
        flex: 1;
        min-height: 40px;
        max-height: 140px;
        padding: var(--space-2) var(--space-3);
        resize: none;
        line-height: 1.45;
        font-size: 0.9rem;
      }
      .composer-send {
        flex: 0 0 auto;
        width: 40px;
        height: 40px;
        padding: 0;
        justify-content: center;
      }
      .composer-send:disabled {
        opacity: 0.5;
        cursor: default;
      }
      .composer-spin {
        width: 15px;
        height: 15px;
        border-radius: 50%;
        border: 2px solid rgba(255, 255, 255, 0.35);
        border-top-color: var(--text-on-accent);
        animation: trace-spin 0.7s linear infinite;
      }

      @media (prefers-reduced-motion: reduce) {
        .bubble {
          animation: none;
        }
        .dots span,
        .trace-spin,
        .composer-spin,
        .mic-btn.is-listening {
          animation: none;
        }
        .dots span {
          opacity: 0.7;
        }
      }
    `,
  ],
})
export class AssistantActionsComponent implements OnInit {
  protected readonly store = inject(AssistantStore);
  private readonly injector = inject(Injector);
  private readonly thread = viewChild<ElementRef<HTMLElement>>("thread");

  /** The text composer draft (signal-backed — zoneless). */
  protected readonly draft = signal("");

  /** Send is allowed when there's non-blank text and no turn is in flight. */
  protected readonly canSend = computed(
    () => !this.store.processing() && this.draft().trim().length > 0,
  );

  constructor() {
    // Auto-scroll the thread to the newest bubble whenever the conversation
    // changes. Tracks the messages signal in the effect, schedules the DOM work
    // via afterNextRender (zoneless-safe; no signal writes → no NG0600).
    effect(() => {
      this.store.messages();
      afterNextRender(
        () => {
          const el = this.thread()?.nativeElement;
          if (el) el.scrollTop = el.scrollHeight;
        },
        { injector: this.injector },
      );
    });
  }

  ngOnInit(): void {
    // Subscribe once to the wake/result/tool streams (idempotent). The store is a
    // root singleton, so its subscriptions outlive this component — we don't
    // unlisten on destroy here (the store owns lifetime; cf. RecorderStore).
    void this.store.init();
  }

  /** Submit the typed question through the shared multi-turn brain (memory). */
  protected submit(event: Event): void {
    event.preventDefault();
    const text = this.draft().trim();
    if (!text || this.store.processing()) return;
    this.draft.set("");
    void this.store.send(text).catch(() => {
      /* the store surfaces the error on the assistant bubble */
    });
  }

  /** Enter sends; Shift+Enter inserts a newline. */
  protected onEnter(event: Event): void {
    const ke = event as KeyboardEvent;
    if (ke.shiftKey) return; // allow a newline
    this.submit(event);
  }

  /**
   * CLICK-TO-STOP voice trigger: while listening, stop so the full utterance is
   * dispatched; otherwise open the listener. Swallow rejections — the store
   * resets its listening/processing/in-flight state on error.
   */
  protected toggleAsk(): void {
    if (this.store.listening()) {
      void this.store.endAsk().catch(() => {
        /* stop failed — store cleared processing/in-flight */
      });
    } else {
      void this.store.askNow().catch(() => {
        /* listener unavailable — store resets the listening/in-flight state */
      });
    }
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

  /** Short message for a resolved assistant bubble that carries no answer text. */
  protected emptyNote(status: ChatMessage["status"]): string {
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
