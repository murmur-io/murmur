import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
  output,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import type { ProactiveHintPayload } from "../../core/models";

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
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  template: `
    <div class="hint-card" role="status" aria-label="Brain hint">
      <span class="hint-ico" aria-hidden="true">{{ icon() }}</span>
      <span class="hint-body">
        <span class="hint-kind">{{ kindLabel() }}</span>
        <span class="hint-title" [title]="hint().title">{{
          hint().title
        }}</span>
      </span>
      @if (hint().meetingId; as mid) {
        <a class="btn btn-ghost hint-open" [routerLink]="['/meeting', mid]">
          Open
        </a>
      }
      <button
        type="button"
        class="hint-dismiss"
        (click)="dismissed.emit()"
        aria-label="Dismiss hint"
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
      .hint-card {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex: none;
        padding: var(--space-2) var(--space-2) var(--space-2) var(--space-3);
        border: 1px solid var(--accent-ring);
        border-radius: var(--radius-md);
        background: var(--accent-soft);
        animation: rise 320ms var(--transition) both;
      }
      .hint-ico {
        font-size: 1.05rem;
        line-height: 1;
        flex: none;
      }
      .hint-body {
        display: flex;
        flex-direction: column;
        gap: 1px;
        flex: 1 1 auto;
        min-width: 0;
      }
      .hint-kind {
        color: var(--text-muted);
        font-size: 0.68rem;
        font-weight: 600;
        letter-spacing: 0.06em;
        text-transform: uppercase;
      }
      .hint-title {
        color: var(--text-primary);
        font-size: 0.875rem;
        line-height: 1.4;
        overflow: hidden;
        white-space: nowrap;
        text-overflow: ellipsis;
      }
      .hint-open {
        flex: none;
        height: 30px;
        padding: 0 var(--space-3);
        font-size: 0.82rem;
      }
      .hint-dismiss {
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
      .hint-dismiss:hover {
        background: var(--surface-hover);
        color: var(--text-primary);
      }
      .hint-dismiss:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
    `,
  ],
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
