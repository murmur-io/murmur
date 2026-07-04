import {
  ChangeDetectionStrategy,
  Component,
  input,
  output,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import type { RailWhisperCard } from "../../core/meeting-conversation.store";

/**
 * Realtime Reactions — ONE dismissible "whisper" contradiction card in the record
 * screen's reactions rail (beside the proactive recall {@link ProactiveHintCardComponent}).
 * Fired on-device when a far-side utterance contradicts a fact already in the
 * user's history: it shows a neutral one-line `summary`, the EXTRACTIVE `oldQuote`
 * (a real prior value — never model-generated, so it can never fabricate an
 * accusation), and a `[[sourceMeeting]]` click-through when the source is known.
 *
 * Purely presentational — the store owns the payload, dedup, cap, dismissal, AND
 * the lock-transition purge (a card citing a just-sealed meeting must not linger;
 * see {@link MeetingConversationStore.clearRail}). Sits IN-FLOW (pinned above the
 * notes flow), NOT floating over content, so a translucent warning block is
 * correct here — the opaque `--surface-overlay` rule (trap T3) applies only to
 * overlays.
 */
@Component({
  selector: "app-whisper-card",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  template: `
    <div class="whisper-card" role="status" aria-label="Contradiction hint">
      <span class="whisper-ico" aria-hidden="true">
        <svg viewBox="0 0 24 24" width="16" height="16">
          <path
            d="M12 3.5 22 20H2L12 3.5Z"
            fill="none"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linejoin="round"
          />
          <path
            d="M12 10v4"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
          />
          <circle cx="12" cy="17" r="1" fill="currentColor" />
        </svg>
      </span>
      <span class="whisper-body">
        <span class="whisper-kind">Possible contradiction</span>
        <span class="whisper-summary" [title]="card().summary">{{
          card().summary
        }}</span>
        <span class="whisper-quote">
          Earlier: <q>{{ card().oldQuote }}</q>
          @if (card().sourceMeetingId; as mid) {
            <a class="whisper-source" [routerLink]="['/meeting', mid]">
              open source
            </a>
          }
        </span>
      </span>
      <button
        type="button"
        class="whisper-dismiss"
        (click)="dismissed.emit()"
        aria-label="Dismiss contradiction hint"
        title="Dismiss"
      >
        <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
          <path
            d="M6 6l12 12M18 6L6 18"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
          />
        </svg>
      </button>
    </div>
  `,
  styles: [
    `
      .whisper-card {
        display: flex;
        align-items: flex-start;
        gap: var(--space-3);
        flex: none;
        padding: var(--space-2) var(--space-2) var(--space-2) var(--space-3);
        border: 1px solid var(--warning);
        border-radius: var(--radius-md);
        background: var(--warning-soft);
        animation: rise 320ms var(--transition) both;
      }
      .whisper-ico {
        display: inline-flex;
        color: var(--warning);
        line-height: 1;
        flex: none;
        margin-top: 1px;
      }
      .whisper-body {
        display: flex;
        flex-direction: column;
        gap: 2px;
        flex: 1 1 auto;
        min-width: 0;
      }
      .whisper-kind {
        color: var(--warning);
        font-size: 0.68rem;
        font-weight: 600;
        letter-spacing: 0.06em;
        text-transform: uppercase;
      }
      .whisper-summary {
        color: var(--text-primary);
        font-size: 0.875rem;
        line-height: 1.4;
        overflow: hidden;
        white-space: nowrap;
        text-overflow: ellipsis;
      }
      .whisper-quote {
        color: var(--text-secondary);
        font-size: 0.8rem;
        line-height: 1.45;
      }
      .whisper-quote q {
        color: var(--text-primary);
        font-style: italic;
      }
      .whisper-source {
        margin-left: var(--space-1);
        color: var(--accent);
        font-weight: 550;
        white-space: nowrap;
      }
      .whisper-dismiss {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 28px;
        height: 28px;
        flex: none;
        border: none;
        border-radius: var(--radius-sm);
        background: transparent;
        color: var(--text-muted);
        cursor: pointer;
        transition:
          background var(--transition),
          color var(--transition);
      }
      .whisper-dismiss:hover {
        background: var(--surface-hover);
        color: var(--text-primary);
      }
      .whisper-dismiss:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
    `,
  ],
})
export class WhisperCardComponent {
  /** The contradiction card to render (the store owns the rail + lock-purge). */
  readonly card = input.required<RailWhisperCard>();
  /** The user clicked ✕ — the parent removes this card from the rail. */
  readonly dismissed = output<void>();
}
