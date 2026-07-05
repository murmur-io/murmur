import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
  output,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import type { ProactiveHintPayload } from "../../../core/models";

/**
 * Proactive brain (P2) — ONE dismissible, system-style recall card in the
 * record screen's conversation surface: "the brain volunteers what you'd have
 * asked for" (a related past meeting / an open commitment / a known fact
 * matched against the live transcript, all from LOCAL reads). Purely
 * presentational: the parent owns visibility (the store's `hint` signal +
 * the `proactiveHintsEnabled` mute) and dismissal; this renders the payload's
 * IDs + title only — it never fetches content.
 *
 * Sits IN-FLOW inside the conversation surface (pinned above the notes flow),
 * NOT floating over content — so a translucent accent block is correct here;
 * the opaque `--surface-overlay` rule (trap T3) applies only to overlays.
 * "Open" navigates to the source meeting's detail view (`/meeting/:id`) when
 * the hint carries one; a hint without a meeting id offers no navigation.
 */
@Component({
  selector: "app-proactive-hint-card",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  templateUrl: "./proactive-hint-card.component.html",
  styleUrl: "./proactive-hint-card.component.scss",
})
export class ProactiveHintCardComponent {
  /** The hint to render (the parent gates on null / the global mute). */
  readonly hint = input.required<ProactiveHintPayload>();
  /** The user clicked ✕ — the parent hides + session-dedups the hint. */
  readonly dismissed = output<void>();

  /** Kind glyph — matches the house `@brain` emoji style (mention popover). */
  readonly icon = computed(() => {
    switch (this.hint().kind) {
      case "open_commitment":
        return "📌";
      case "fact":
        return "ℹ️";
      default:
        return "🧠";
    }
  });

  /** The subtle "why am I seeing this" label above the title. */
  readonly kindLabel = computed(() => {
    switch (this.hint().kind) {
      case "open_commitment":
        return "Open commitment";
      case "fact":
        return "Known fact";
      default:
        return "Related meeting";
    }
  });
}
