import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  computed,
  inject,
  signal,
} from "@angular/core";
import { AssistantStore } from "../../core/assistant.store";
import type { AssistantInteraction } from "../../core/assistant.store";
import { MarkdownComponent } from "../../shared/markdown.component";
import { AssistantSourcesComponent } from "../../shared/assistant-sources.component";
import { AiOrbComponent } from "./ai-orb.component";

/**
 * The live "assistant" card on the record surface — the home of the in-meeting
 * BRAIN. Subscribes (once, via AssistantStore.init()) to the wake + result + live
 * tool-trace streams and renders a newest-first list of interactions: a pending
 * row (🎙 spoken / ⌨ typed) that shows the brain's LIVE tool trace as it works
 * ("Searching notes… ✓", "Checking the web…"), resolved to a SANITIZED-markdown
 * answer (`app-markdown`) + a deduped "🔗 Źródła" block (`app-assistant-sources`).
 *
 * It also carries the TEXT COMPOSER at its foot — the twin of the voice trigger:
 * the user can TYPE a question that funnels through the exact same gated brain
 * (`IpcService.askAssistantText` → `run_assistant_turn`), so speech and text share
 * one timeline, one orb, one trace.
 *
 * The card is in-flow on the record page (not a floating overlay), so it uses the
 * frosted `.card`. If it were ever floated OVER content it would have to switch to
 * `var(--surface-overlay)` (trap T3) — it is intentionally NOT floated.
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

      @if (store.hasAny()) {
        <ul class="actions-list">
          @for (a of store.interactions(); track a.id) {
            <li class="action-row" [class.is-pending]="a.status === 'pending'">
              <div class="action-heard">
                <span class="heard-ico" aria-hidden="true">{{
                  a.source === "text" ? "⌨" : "🎙"
                }}</span>
                @if (a.status === "nothing_heard") {
                  <span class="heard-text heard-nudge">{{ statusLabel(a) }}</span>
                } @else {
                  <span class="heard-text">
                    {{ a.source === "text" ? "you asked:" : "usłyszano:" }}
                    <strong>{{ a.command || "…" }}</strong>
                  </span>
                  @if (a.status !== "pending") {
                    <span class="pill" [class]="statusPillClass(a)">
                      <span class="pill-dot"></span>
                      {{ statusLabel(a) }}
                    </span>
                  }
                }
              </div>

              @if (a.trace.length > 0) {
                <div class="trace" role="status" aria-label="Tool use">
                  @for (t of a.trace; track t.id) {
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

              @if (a.status === "pending") {
                @if (a.trace.length === 0) {
                  <div class="action-pending" role="status">
                    <span class="dots" aria-hidden="true">
                      <span></span><span></span><span></span>
                    </span>
                    <span class="text-muted">Thinking…</span>
                  </div>
                }
              } @else if (a.status === "nothing_heard") {
                <!-- the nudge label above is the whole message; no summary row -->
              } @else {
                @if (a.summary) {
                  <app-markdown class="action-summary" [markdown]="a.summary" compact />
                }
                @if (a.citations.length > 0) {
                  <app-assistant-sources [citations]="a.citations" />
                }
              }
            </li>
          }
        </ul>
      } @else {
        <p class="assistant-empty text-muted">
          Ask the assistant a grounded question — type below, or say your wake
          phrase. Answers and their sources appear here.
        </p>
      }

      <form class="composer" (submit)="submit($event)">
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
      .assistant-empty {
        margin: 0;
        font-size: 0.875rem;
        line-height: 1.55;
      }

      .actions-list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .action-row {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        padding: var(--space-3);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
        animation: rise 260ms var(--transition) both;
      }
      .action-row.is-pending {
        border-color: var(--accent-soft);
      }
      .action-heard {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex-wrap: wrap;
      }
      .heard-ico {
        font-size: 0.85rem;
        line-height: 1;
      }
      .heard-text {
        color: var(--text-secondary);
        font-size: 0.875rem;
      }
      .heard-text strong {
        color: var(--text-primary);
        font-weight: 600;
      }
      .heard-nudge {
        color: var(--text-primary);
        font-weight: 550;
      }
      .action-heard .pill {
        margin-left: auto;
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

      .action-pending {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        font-size: 0.85rem;
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

      .action-summary {
        display: block;
        font-size: 0.9rem;
      }

      /* ── text composer ────────────────────────────────────────────── */
      .composer {
        display: flex;
        align-items: flex-end;
        gap: var(--space-2);
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
        .action-row {
          animation: none;
        }
        .dots span,
        .trace-spin,
        .composer-spin {
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

  /** The text composer draft (signal-backed — zoneless). */
  protected readonly draft = signal("");

  /** Send is allowed when there's non-blank text and no turn is in flight. */
  protected readonly canSend = computed(
    () => !this.store.processing() && this.draft().trim().length > 0,
  );

  ngOnInit(): void {
    // Subscribe once to the wake/result/tool streams (idempotent). The store is a
    // root singleton, so its subscriptions outlive this component — we don't
    // unlisten on destroy here (the store owns lifetime; cf. RecorderStore).
    void this.store.init();
  }

  /** Submit the typed question through the shared gated brain. */
  protected submit(event: Event): void {
    event.preventDefault();
    const text = this.draft().trim();
    if (!text || this.store.processing()) return;
    this.draft.set("");
    void this.store.askText(text).catch(() => {
      /* the store surfaces the error on the optimistic row */
    });
  }

  /** Enter sends; Shift+Enter inserts a newline. */
  protected onEnter(event: Event): void {
    const ke = event as KeyboardEvent;
    if (ke.shiftKey) return; // allow a newline
    this.submit(event);
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

  /** Map a resolved status to a global `.pill` variant. */
  protected statusPillClass(a: AssistantInteraction): string {
    switch (a.status) {
      case "ok":
        return "is-success";
      case "needs_consent":
        return "is-warning";
      case "unavailable":
      case "unrecognized":
        return "is-accent";
      case "nothing_heard":
        return "";
      default:
        return "is-danger";
    }
  }

  /** Short human label for the status pill / nudge line. */
  protected statusLabel(a: AssistantInteraction): string {
    switch (a.status) {
      case "ok":
        return "Done";
      case "needs_consent":
        return "Needs consent";
      case "unavailable":
        return "Unavailable";
      case "unrecognized":
        return "Not recognized";
      case "nothing_heard":
        return "Nie usłyszałem — spróbuj jeszcze raz";
      case "error":
        return "Error";
      default:
        return "";
    }
  }
}
