import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
} from "@angular/core";
import type { Posture } from "../../../../core/models";
import { SettingsStore } from "../../settings.store";

/**
 * AI & Models → Brain Posture block: the 3-way preset selector (Cloud /
 * Hybrid / Fully local), the installed-base retirement nudge, and the
 * contextual "right now" state area (idle state line + model pills, or
 * pending-download progress + Cancel).
 *
 * Moved from AiDefaultsBlockComponent so it forms its own card above
 * "What Murmur uses". The standalone "Enable Murmur Brain Live" card is
 * intentionally NOT included — posture-switching is now the only entry
 * point. Consumes Task-1 store signals.
 */
@Component({
  selector: "app-brain-posture-block",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [],
  template: `
    <div class="card posture-card">
      <!-- Intro header — frames the section before the posture chooser. -->
      <div class="posture-head-copy">
        <h3>What Murmur uses</h3>
        <p class="text-secondary posture-intro">
          Pick how much runs on this Mac. The map below shows exactly what runs where — tune it under Advanced.
        </p>
      </div>

      <!--
        Installed-base migration nudge: the persisted on-device model was
        retired for licensing → offer the Apache-licensed replacement.
        In-flow alert, so the frosted .banner is correct (not opaque overlay).
      -->
      @if (retirementNudge(); as nudge) {
        <div class="banner is-warning retirement-banner">
          <span class="retirement-copy">
            ⚠ Your on-device brain model was retired for licensing — switch to
            {{ nudge.replacementName }}.
            <span class="text-muted retirement-reason">{{ nudge.reason }}</span>
          </span>
          <button
            type="button"
            class="btn btn-primary"
            (click)="applyRetirement()"
            [disabled]="applyingRetirement()"
          >
            @if (applyingRetirement()) {
              <span class="spin-ring" aria-hidden="true"></span>
              Switching…
            } @else {
              Switch to {{ nudge.replacementName }}
            }
          </button>
        </div>
      }

      <!--
        Murmur Brain posture: a 3-way preset selector. "custom" is a
        display-only state (no preset forced) — its own pill shows and no
        segment button is active.
      -->
      <div class="posture">
        <div class="posture-head">
          <span class="use-group-label text-muted">Murmur Brain posture</span>
          @if (posture() === "custom") {
            <span class="pill posture-custom-pill">
              <span class="pill-dot"></span>
              Custom
            </span>
          }
        </div>
        <div class="posture-seg" role="group" aria-label="Murmur Brain posture">
          <button
            type="button"
            class="posture-opt"
            [class.is-active]="posture() === 'cloud'"
            [disabled]="postureBusy()"
            (click)="setPosture('cloud')"
          >
            <span class="posture-opt-title">Cloud</span>
            <span class="posture-opt-sub text-muted">
              Your Default engine does everything
            </span>
          </button>
          <button
            type="button"
            class="posture-opt"
            [class.is-active]="posture() === 'hybrid'"
            [disabled]="postureBusy()"
            (click)="setPosture('hybrid')"
          >
            <span class="posture-opt-title">Hybrid ⭐</span>
            <span class="posture-opt-sub text-muted">
              Cloud notes + realtime reactions on this Mac
            </span>
          </button>
          <button
            type="button"
            class="posture-opt"
            [class.is-active]="posture() === 'fully_local'"
            [disabled]="postureBusy()"
            (click)="setPosture('fully_local')"
          >
            <span class="posture-opt-title">Fully local</span>
            <span class="posture-opt-sub text-muted">
              Nothing leaves this Mac
            </span>
          </button>
        </div>

        <!--
          Contextual state area — replaces the old static field-help and the
          Brain Live enablement card. Shows either the pending-download
          progress (Cancel available) or the committed-posture summary.

          NOTE: during a pending download the committed posture (posture())
          is still the OLD value; neededModels() would return [] for "cloud".
          We therefore look up the downloading model by brainDownloadingId()
          in brainModels() for the progress label, rather than iterating
          neededModels() in the pending branch.
        -->
        <div class="posture-state">
          @if (pendingPosture(); as pend) {
            <p class="text-secondary">
              {{ pendingLabel(pend) }} — downloading on-device models…
            </p>
            @if (brainDownloadingId()) {
              <div class="semantic-progress" role="status">
                <div class="semantic-progress-track" aria-hidden="true">
                  <div
                    class="semantic-progress-fill"
                    [style.width.%]="brainDownloadFrac() * 100"
                  ></div>
                </div>
                <span class="semantic-progress-label text-muted">
                  {{ downloadingModelName() }} · {{ brainPct() }}
                </span>
              </div>
            }
            <button
              type="button"
              class="btn btn-ghost"
              (click)="cancelPostureDownload()"
            >
              Cancel
            </button>
            <span class="field-help text-muted">
              Staying on your current setup until it's ready.
            </span>
          } @else {
            <p class="text-secondary posture-now">{{ postureStateLine() }}</p>
            @for (n of neededModels(); track n.role) {
              <span class="pill" [class.is-success]="n.model?.downloaded">
                <span class="pill-dot"></span>{{ n.role === "notes" ? "Notes & Ask" : "Reactions" }}: {{ n.model?.name ?? "—" }}{{ n.model?.downloaded ? " ✓" : "" }}
              </span>
            }
            @if (!brainLiveRamOk()) {
              <span class="brain-live-ram-warn">
                ⚠ Your Mac may not have enough RAM to run this smoothly
                alongside recording.
              </span>
            }
          }
          @if (postureError(); as perr) {
            <p class="text-danger brain-error">{{ perr }}</p>
          }
        </div>
      </div>
    </div>
  `,
  styles: [
    `
      :host {
        display: contents;
      }

      .posture-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }

      /* Retirement nudge — banner copy + action, stacked on narrow widths. */
      .retirement-banner {
        flex-direction: column;
        align-items: flex-start;
        gap: var(--space-3);
      }
      .retirement-copy {
        display: flex;
        flex-direction: column;
        gap: 2px;
        line-height: 1.5;
      }
      .retirement-reason {
        font-size: 0.8125rem;
        line-height: 1.5;
      }

      /* ── Section intro header ── */
      .posture-head-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .posture-head-copy h3 {
        margin: 0;
      }
      .posture-intro {
        margin: 0;
        font-size: 0.875rem;
        line-height: 1.55;
      }

      /* ── Posture group heading ── */
      .use-group-label {
        font-size: 0.8125rem;
        font-weight: 550;
        letter-spacing: 0.01em;
        text-transform: uppercase;
      }

      /* ── Murmur Brain posture — segmented preset selector ── */
      .posture {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .posture-head {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-2);
      }
      .posture-custom-pill {
        flex: none;
      }
      .posture-seg {
        display: flex;
        gap: var(--space-2);
        flex-wrap: wrap;
      }
      .posture-opt {
        display: flex;
        flex-direction: column;
        gap: 2px;
        flex: 1 1 140px;
        min-width: 0;
        text-align: left;
        padding: var(--space-2) var(--space-3);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        color: var(--text-primary);
        cursor: pointer;
        transition:
          border-color var(--transition),
          background var(--transition);
      }
      .posture-opt:hover:not(:disabled) {
        border-color: var(--border-strong);
      }
      .posture-opt.is-active {
        border-color: var(--accent);
        background: var(--accent-soft);
      }
      .posture-opt:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .posture-opt:disabled {
        opacity: 0.6;
        cursor: default;
      }
      .posture-opt-title {
        font-size: 0.9rem;
        font-weight: 600;
      }
      .posture-opt-sub {
        font-size: 0.78rem;
        line-height: 1.4;
      }

      /* ── Contextual state area ── */
      .posture-state {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .posture-now {
        margin: 0;
        font-size: 0.875rem;
        line-height: 1.55;
      }
      .brain-live-ram-warn {
        font-size: 0.8125rem;
        line-height: 1.5;
        color: var(--warning);
      }
      .brain-error {
        margin: 0;
        font-size: 0.85rem;
      }
      .field-help {
        font-size: 0.8125rem;
        line-height: 1.5;
      }

      /* ── Download progress bar (reused from the embed model section) ── */
      .semantic-progress {
        display: flex;
        flex-direction: column;
        gap: 3px;
        min-width: 180px;
        flex: 1 1 auto;
      }
      .semantic-progress-track {
        height: 6px;
        border-radius: 3px;
        background: var(--surface-raised);
        overflow: hidden;
      }
      .semantic-progress-fill {
        height: 100%;
        background: var(--accent);
        border-radius: 3px;
        transition: width var(--transition);
      }
      .semantic-progress-label {
        font-size: 0.75rem;
      }

      /* Inline spinner on the "Switch to …" retirement button. */
      .spin-ring {
        width: 15px;
        height: 15px;
        border-radius: 50%;
        border: 2px solid rgba(255, 255, 255, 0.35);
        border-top-color: var(--text-on-accent);
        animation: spin 0.8s linear infinite;
        margin-right: var(--space-2);
        vertical-align: -2px;
        display: inline-block;
      }
      @keyframes spin {
        to {
          transform: rotate(360deg);
        }
      }
      @media (prefers-reduced-motion: reduce) {
        .spin-ring {
          animation: none;
        }
      }
    `,
  ],
})
export class BrainPostureBlockComponent {
  private readonly store = inject(SettingsStore);

  // ── Store signal wires ────────────────────────────────────────────────────
  readonly posture = this.store.posture;
  readonly postureBusy = this.store.postureBusy;
  readonly postureError = this.store.postureError;
  readonly pendingPosture = this.store.pendingPosture;
  readonly postureStateLine = this.store.postureStateLine;
  readonly neededModels = this.store.neededModels;
  readonly brainDownloadingId = this.store.brainDownloadingId;
  readonly brainDownloadFrac = this.store.brainDownloadFrac;
  readonly brainPct = this.store.brainPct;
  readonly brainLiveRamOk = this.store.brainLiveRamOk;
  readonly retirementNudge = this.store.retirementNudge;
  readonly applyingRetirement = this.store.applyingRetirement;

  /** Full model list — used to resolve the downloading model's display name. */
  private readonly brainModels = this.store.brainModels;

  /**
   * Display name of the model currently downloading, looked up from
   * brainModels() by brainDownloadingId(). Falls back to the raw id so the
   * progress label is never blank.
   *
   * Separate from neededModels() because during a setPosture download the
   * committed posture is still the OLD value → neededModelsFor(oldPosture)
   * returns [] → iterating neededModels() in the pending block would produce
   * no progress bar at all.
   */
  readonly downloadingModelName = computed((): string => {
    const id = this.brainDownloadingId();
    if (!id) return "";
    return this.brainModels().find((m) => m.id === id)?.name ?? id;
  });

  // ── Actions ───────────────────────────────────────────────────────────────

  /** Apply a Murmur Brain posture preset (`cloud` / `hybrid` / `fully_local`). */
  setPosture(p: Posture): void {
    void this.store.setPosture(p);
  }

  /** Abort an in-flight posture download; the committed posture is unchanged. */
  cancelPostureDownload(): void {
    this.store.cancelPostureDownload();
  }

  /** Download + select the Apache-licensed replacement for the retired model. */
  applyRetirement(): void {
    void this.store.applyRetirementReplacement();
  }

  /** Human-readable label for the pending posture (used in the progress copy). */
  pendingLabel(p: Posture): string {
    if (p === "hybrid") return "Hybrid";
    if (p === "fully_local") return "Fully local";
    return p;
  }
}
