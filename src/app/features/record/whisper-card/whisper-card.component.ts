import {
  ChangeDetectionStrategy,
  Component,
  input,
  output,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import type { RailWhisperCard } from "../../../core/meeting-conversation.store";

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
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  templateUrl: "./whisper-card.component.html",
  styleUrl: "./whisper-card.component.scss",
})
export class WhisperCardComponent {
  /** The contradiction card to render (the store owns the rail + lock-purge). */
  readonly card = input.required<RailWhisperCard>();
  /** The user clicked ✕ — the parent removes this card from the rail. */
  readonly dismissed = output<void>();
}
