import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  input,
  signal,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import { IpcService } from "../../core/ipc.service";
import { FoldersService } from "../../services/folders.service";

/** localStorage key for the one-time "brain reveal" dismissal. */
const SEEN_KEY = "murmur-brain-reveal-seen";

/**
 * Post-first-note "Brain Reveal" card (Tier 4a) — after a note lands, reveal the
 * already-alive shallow brain: "Murmur mapped N people and M open commitments
 * from your meetings", with one-tap nav to /people and /graph. Entity extraction
 * runs unconditionally after every summary on defaults, so this simply makes that
 * existing, on-defaults work VISIBLE on day one — it flips no flag, adds no
 * egress, and needs no model.
 *
 * NO new read path: the counts derive from {@link IpcService.listPeople}, which
 * is already visibility-gated server-side (visible-only people + visible-only
 * counts). The card shows AGGREGATE COUNTS only — no names, no note/segment/audio
 * content. Owner-attributed honesty: the commitment count is Σ openCommitmentCount
 * across known people, so ownerless open items are not counted here.
 *
 * Sits IN-FLOW in the record screen's post-note region (not floating over
 * content), so the translucent `--accent-soft` accent block is correct here; the
 * opaque `--surface-overlay` rule (trap T3) applies only to overlays — same
 * rationale as {@link ProactiveHintCardComponent}.
 */
@Component({
  selector: "app-brain-reveal-card",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  template: `
    @if (show()) {
      <div class="reveal" role="status" aria-live="polite">
        <span class="reveal-ico" aria-hidden="true">🧠</span>
        <div class="reveal-body">
          <span class="reveal-kicker">Your brain is already working</span>
          <p class="reveal-line">
            Murmur mapped {{ peopleLabel() }} and {{ commitmentLabel() }} from
            your meetings.
          </p>
        </div>
        <div class="reveal-actions">
          <a class="btn btn-primary btn-sm" routerLink="/people">See people</a>
          <a class="btn btn-ghost btn-sm" routerLink="/graph">Open graph</a>
        </div>
        <button
          type="button"
          class="reveal-dismiss"
          (click)="dismiss()"
          aria-label="Dismiss"
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
    }
  `,
  styles: [
    `
      .reveal {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-3) var(--space-4);
        border: 1px solid var(--accent-ring);
        border-radius: var(--radius-md);
        background: var(--accent-soft);
      }
      .reveal-ico {
        font-size: 1.15rem;
        line-height: 1;
        flex: none;
      }
      .reveal-body {
        display: flex;
        flex-direction: column;
        gap: 1px;
        flex: 1 1 auto;
        min-width: 0;
      }
      .reveal-kicker {
        color: var(--text-muted);
        font-size: 0.68rem;
        font-weight: 600;
        letter-spacing: 0.06em;
        text-transform: uppercase;
      }
      .reveal-line {
        margin: 0;
        color: var(--text-primary);
        font-size: 0.9rem;
        line-height: 1.4;
      }
      .reveal-actions {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex: none;
      }
      .reveal .btn-sm {
        height: 32px;
        padding: 0 var(--space-3);
        font-size: 0.85rem;
      }
      .reveal-dismiss {
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
      .reveal-dismiss:hover {
        background: var(--surface-hover);
        color: var(--text-primary);
      }
      .reveal-dismiss:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      @media (max-width: 620px) {
        .reveal {
          flex-wrap: wrap;
        }
      }
    `,
  ],
})
export class BrainRevealCardComponent {
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);

  /**
   * Fires the reveal only when the post-note surface is live (record screen:
   * `!!store.lastNote()` — set after `stopRecording()` resolves, post-extraction;
   * detail screen: `!locked()`). While false, the effect skips the fetch entirely,
   * so a sealed meeting never triggers a read behind the lock.
   */
  readonly active = input(false);

  private readonly _peopleCount = signal(0);
  private readonly _commitmentCount = signal(0);
  private readonly _loaded = signal(false);
  private readonly _dismissed = signal(this.readSeen());

  readonly peopleCount = this._peopleCount.asReadonly();
  readonly commitmentCount = this._commitmentCount.asReadonly();

  /**
   * Reveal only when active, not-yet-dismissed, the counts have loaded, and the
   * brain actually found something — honest: no reveal on an empty brain, and
   * hidden until the fetch resolves (closes the day-one extraction-lag window).
   */
  readonly show = computed(
    () =>
      this.active() &&
      !this._dismissed() &&
      this._loaded() &&
      this._peopleCount() + this._commitmentCount() > 0,
  );

  readonly peopleLabel = computed(() => {
    const n = this._peopleCount();
    return n === 1 ? "1 person" : `${n} people`;
  });

  readonly commitmentLabel = computed(() => {
    const n = this._commitmentCount();
    return n === 1 ? "1 open commitment" : `${n} open commitments`;
  });

  /**
   * Load the counts when the surface goes live, and re-load whenever the folder
   * lock-state changes (a session unlock/relock shifts visibility) — mirrors
   * PeopleComponent / GraphComponent. `fetch()` writes the count/loaded signals,
   * so this tracked effect must be allowed to write (NG0600 guard). The `active`
   * guard runs FIRST so no fetch is dispatched behind a lock / before a note.
   */
  private readonly _load = effect(
    () => {
      if (!this.active()) {
        return;
      }
      this.folders.tree();
      void this.fetch();
    },
    { allowSignalWrites: true },
  );

  private async fetch(): Promise<void> {
    try {
      const rows = await this.ipc.listPeople();
      this._peopleCount.set(rows.length);
      this._commitmentCount.set(
        rows.reduce((n, p) => n + p.openCommitmentCount, 0),
      );
    } catch {
      this._peopleCount.set(0);
      this._commitmentCount.set(0);
    } finally {
      this._loaded.set(true);
    }
  }

  /** One-time dismiss: hide now + remember it so the reveal never nags again. */
  dismiss(): void {
    this._dismissed.set(true);
    try {
      localStorage.setItem(SEEN_KEY, "1");
    } catch {
      /* private-mode / storage-disabled — session-only dismissal is fine. */
    }
  }

  private readSeen(): boolean {
    try {
      return localStorage.getItem(SEEN_KEY) === "1";
    } catch {
      return false;
    }
  }
}
