import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  inject,
} from "@angular/core";
import { AssistantStore } from "../../core/assistant.store";
import type { AssistantInteraction } from "../../core/assistant.store";

/**
 * Phase H — the live "assistant actions" card on the record surface. Subscribes
 * (once, via AssistantStore.init()) to the in-meeting voice assistant's wake +
 * result event streams and renders a newest-first list of recent interactions:
 * a pending "🎙 usłyszano: {command}" row on a wake, resolved to {summary} +
 * [[Title]] citation chips with a status pill when the result arrives.
 *
 * The card is in-flow on the record page (not a floating overlay), so it uses
 * the frosted `.card` like the other record-surface panels. If it were ever
 * floated OVER content it would have to switch to `var(--surface-overlay)`
 * (trap T3) — it is intentionally NOT floated.
 */
@Component({
  selector: "app-assistant-actions",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="card assistant" role="group" aria-label="Voice assistant">
      <div class="assistant-head">
        <span class="assistant-mark" aria-hidden="true">🎙</span>
        <span class="assistant-title">Voice assistant</span>
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
                <span class="heard-ico" aria-hidden="true">🎙</span>
                @if (a.status === "nothing_heard") {
                  <span class="heard-text heard-nudge">
                    {{ statusLabel(a) }}
                  </span>
                } @else {
                  <span class="heard-text">
                    usłyszano:
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

              @if (a.status === "pending") {
                <div class="action-pending" role="status">
                  <span class="dots" aria-hidden="true">
                    <span></span><span></span><span></span>
                  </span>
                  <span class="text-muted">Thinking…</span>
                </div>
              } @else if (a.status === "nothing_heard") {
                <!-- the nudge label above is the whole message; no summary row -->
              } @else {
                @if (a.summary) {
                  <p class="action-summary">{{ a.summary }}</p>
                }
                @if (a.citations.length > 0) {
                  <div class="action-cites" aria-label="Sources">
                    @for (c of a.citations; track c) {
                      <span class="cite-chip">[[{{ c }}]]</span>
                    }
                  </div>
                }
              }
            </li>
          }
        </ul>
      } @else {
        <p class="assistant-empty text-muted">
          Say your wake phrase during a recording to ask the assistant a grounded
          question. Answers and their sources will appear here.
        </p>
      }
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
      .assistant-mark {
        font-size: 1.05rem;
        line-height: 1;
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
        margin: 0;
        color: var(--text-primary);
        font-size: 0.9rem;
        line-height: 1.55;
      }
      .action-cites {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-2);
      }
      .cite-chip {
        padding: 2px var(--space-2);
        border-radius: var(--radius-sm);
        background: var(--accent-soft);
        color: var(--accent-hover);
        font-family: var(--font-mono);
        font-size: 0.78rem;
        letter-spacing: -0.01em;
      }

      @media (prefers-reduced-motion: reduce) {
        .action-row {
          animation: none;
        }
        .dots span {
          animation: none;
          opacity: 0.7;
        }
      }
    `,
  ],
})
export class AssistantActionsComponent implements OnInit {
  protected readonly store = inject(AssistantStore);

  ngOnInit(): void {
    // Subscribe once to the wake/result event streams (idempotent). The store is
    // a root singleton, so its subscriptions outlive this component — we don't
    // unlisten on destroy here (the store owns lifetime; cf. RecorderStore).
    void this.store.init();
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
        // Not an error — the user simply didn't speak. Keep it calm: the plain
        // neutral `.pill` (muted secondary text), no alarming `is-danger`.
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
