import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
} from "@angular/core";
import type { OrbState } from "../../../core/meeting-conversation.store";

/**
 * The in-meeting AI-assistant ORB — a single morphing gradient orb whose STATE is
 * expressed by motion + color (the convergent ChatGPT/Siri/Gemini pattern), NOT
 * by swapping widgets (research: 2026-06-28-ai-assistant-orb-ui.md, Option B):
 *
 *   IDLE       — calm `--accent-gradient` core, slow `transform: scale` breathe.
 *   LISTENING  — audio-reactive scale driven by `level()` via `[style.--level]`
 *                (mirrors the recorder's `[style.--level]="store.level()"` wave)
 *                + a reactive ring whose scale/opacity track the level.
 *   PROCESSING — a rotating conic-gradient ring carved with a radial-gradient
 *                `mask`, spun with `transform: rotate()` (the element-spin variant
 *                — NOT `@property --angle` — so it sidesteps the at-rule scoping
 *                caveat through Angular's CSS pipeline; mirrors the existing
 *                `.orb.proc` spinner in record.component.ts).
 *   ANSWER     — settle to a steady gentle glow + a ONE-SHOT entry "pop" keyed on
 *                the class change (a CSS entry-animation → no component timer,
 *                rule §5). Re-keys per result since `[class]` re-applies.
 *
 * Pure presentational: `state` + `level` are `input()`s, everything else is a
 * `computed`. The orb itself is `aria-hidden` (decorative); the spoken state is
 * announced by a paired `role="status" aria-live` line OWNED BY THE PARENT (the
 * recorder bar / the card) so this component stays a single focused visual unit.
 *
 * prefers-reduced-motion: an EXPLICIT guard kills the value-driven scale + the
 * conic spin + the breathe/pop (the global rule zeroes `animation` duration, but
 * the `transform: scale(calc(1 + var(--level)…))` is value-driven, not an
 * animation — it MUST be neutralised here; mirrors record.component.ts:751).
 */
@Component({
  selector: "app-ai-orb",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./ai-orb.component.html",
  styleUrl: "./ai-orb.component.scss",
})
export class AiOrbComponent {
  /** The 4-state orb model (idle | listening | processing | answer). */
  readonly state = input<OrbState>("idle");

  /** Live mic level 0..1 (RecorderStore.level()); only used in LISTENING. */
  readonly level = input<number>(0);

  /** Defensive clamp — a malformed level never blows up the scale calc. */
  protected readonly clampedLevel = computed(() =>
    Math.max(0, Math.min(1, this.level() || 0)),
  );
}
