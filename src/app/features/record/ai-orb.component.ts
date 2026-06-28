import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
} from "@angular/core";
import type { OrbState } from "../../core/assistant.store";

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
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <span
      class="orb"
      [class]="'is-' + state()"
      [style.--level]="clampedLevel()"
      aria-hidden="true"
    >
      <span class="orb-core"></span>
      <span class="orb-ring"></span>
    </span>
  `,
  styles: [
    `
      :host {
        display: inline-flex;
        flex: none;
        line-height: 0;
      }
      .orb {
        position: relative;
        display: inline-block;
        width: var(--orb-size, 56px);
        height: var(--orb-size, 56px);
      }

      /* The living gradient core — its motion/color is the whole state language. */
      .orb-core {
        position: absolute;
        inset: 0;
        border-radius: 50%;
        background: var(--accent-gradient);
        box-shadow: 0 0 18px rgba(110, 118, 255, 0.55);
        /* blur softens the gradient into a liquid orb (Siri-orb recipe). */
        filter: saturate(1.15);
        transform: scale(1);
        transition:
          transform 90ms linear,
          box-shadow var(--transition),
          filter var(--transition);
      }

      /* The overlaid ring — repurposed per state (reactive halo / conic spinner). */
      .orb-ring {
        position: absolute;
        inset: -4px;
        border-radius: 50%;
        pointer-events: none;
        opacity: 0;
        transition:
          opacity var(--transition),
          transform 90ms linear;
      }

      /* ── IDLE — slow breathe ──────────────────────────────────────────── */
      .orb.is-idle .orb-core {
        animation: orb-breathe 3s ease-in-out infinite;
      }
      .orb.is-idle .orb-ring {
        border: 1.5px solid var(--accent);
        opacity: 0.35;
        animation: orb-halo 3s ease-in-out infinite;
      }
      @keyframes orb-breathe {
        0%,
        100% {
          transform: scale(0.94);
        }
        50% {
          transform: scale(1);
        }
      }
      @keyframes orb-halo {
        0%,
        100% {
          transform: scale(1);
          opacity: 0.35;
        }
        50% {
          transform: scale(1.18);
          opacity: 0;
        }
      }

      /* ── LISTENING — audio-reactive scale + reactive ring ─────────────── */
      .orb.is-listening .orb-core {
        /* value-driven: grows with the live mic level (90ms CSS smoothing). */
        transform: scale(calc(1 + var(--level, 0) * 0.18));
        box-shadow: 0 0 26px rgba(110, 118, 255, 0.8);
      }
      .orb.is-listening .orb-ring {
        border: 2px solid var(--accent-hover);
        opacity: calc(0.25 + var(--level, 0) * 0.6);
        transform: scale(calc(1.05 + var(--level, 0) * 0.22));
      }

      /* ── PROCESSING — conic-gradient ring, element-spin (no @property) ── */
      .orb.is-processing .orb-core {
        transform: scale(0.9);
        animation: orb-breathe 1.6s ease-in-out infinite;
      }
      .orb.is-processing .orb-ring {
        inset: -3px;
        opacity: 1;
        border: none;
        background: conic-gradient(
          from 0deg,
          transparent 0deg,
          var(--accent) 120deg,
          var(--accent-hover) 240deg,
          transparent 360deg
        );
        /* carve the disc into a ring */
        -webkit-mask: radial-gradient(
          farthest-side,
          transparent calc(100% - 4px),
          #000 calc(100% - 4px)
        );
        mask: radial-gradient(
          farthest-side,
          transparent calc(100% - 4px),
          #000 calc(100% - 4px)
        );
        animation: orb-spin 0.9s linear infinite;
      }
      @keyframes orb-spin {
        to {
          transform: rotate(360deg);
        }
      }

      /* ── ANSWER — steady gentle glow + one-shot entry "pop" ───────────── */
      .orb.is-answer .orb-core {
        animation:
          orb-pop 360ms var(--transition) both,
          orb-breathe 3.4s ease-in-out 360ms infinite;
        box-shadow: 0 0 22px rgba(110, 118, 255, 0.7);
      }
      .orb.is-answer .orb-ring {
        border: 1.5px solid var(--accent);
        opacity: 0.3;
      }
      @keyframes orb-pop {
        0% {
          transform: scale(0.7);
        }
        55% {
          transform: scale(1.12);
        }
        100% {
          transform: scale(1);
        }
      }

      /* EXPLICIT reduced-motion guard — neutralise BOTH the animations AND the
         value-driven listening scale (mirrors record.component.ts:751). */
      @media (prefers-reduced-motion: reduce) {
        .orb-core,
        .orb-ring {
          animation: none !important;
          transition: none;
        }
        .orb.is-listening .orb-core,
        .orb.is-processing .orb-core,
        .orb.is-answer .orb-core {
          transform: scale(1);
        }
        .orb.is-listening .orb-ring {
          transform: scale(1.05);
          opacity: 0.4;
        }
        /* keep the processing ring visible but static (no spin). */
        .orb.is-processing .orb-ring {
          opacity: 1;
        }
      }
    `,
  ],
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
