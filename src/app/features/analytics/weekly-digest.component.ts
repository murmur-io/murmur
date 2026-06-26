import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  signal,
} from "@angular/core";
import { IpcService } from "../../core/ipc.service";
import type { DigestResult } from "../../core/models";

/** The selectable digest ranges, in days. */
const RANGES = [7, 30] as const;
type Range = (typeof RANGES)[number];

/**
 * "Weekly digest" — a one-shot synthesis of the user's recent meetings over a
 * chosen window (7 or 30 days), produced by {@link IpcService.generateDigest},
 * which also writes the digest into the vault's Digests/ folder.
 *
 * Presentational sibling of the analytics dashboard cards: the parent owns the
 * page; this component owns only the range picker + the generated result.
 *
 * Lives in its own file so its inline styles get their own per-component
 * `anyComponentStyle` budget (the analytics component's styles are near the cap).
 *
 * The returned markdown is rendered as PLAIN TEXT with `white-space: pre-wrap`
 * (no markdown lib, no innerHTML/DomSanitizer) — the model's line breaks +
 * spacing are preserved verbatim and safely.
 */
@Component({
  selector: "app-weekly-digest",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="card panel">
      <div class="panel-head">
        <h3>Weekly digest</h3>
        <span class="panel-note">Synthesise your recent meetings</span>
      </div>

      <!-- Controls: range segmented control + generate. -->
      <div class="digest-controls">
        <div class="seg" role="group" aria-label="Digest range">
          @for (r of ranges; track r) {
            <button
              type="button"
              class="seg-btn"
              [class.is-active]="range() === r"
              [attr.aria-pressed]="range() === r"
              [disabled]="pending()"
              (click)="setRange(r)"
            >
              {{ r }} days
            </button>
          }
        </div>
        <button
          type="button"
          class="btn btn-primary digest-go"
          [disabled]="pending()"
          (click)="generate()"
        >
          @if (pending()) {
            <span class="digest-spin" aria-hidden="true"></span>
            Synthesising…
          } @else {
            Generate digest
          }
        </button>
      </div>

      @if (error(); as err) {
        <div class="digest-error" role="alert">{{ err }}</div>
      }

      @if (result(); as r) {
        @if (savedPath()) {
          <p class="digest-saved">
            Saved to your vault:
            <span class="digest-path">{{ savedPath() }}</span>
          </p>
        }
        <!-- Plain-text render: markdown shown verbatim, no innerHTML. -->
        <div
          class="digest-output"
          role="region"
          aria-label="Generated digest"
          tabindex="0"
        >
          {{ r.markdown }}
        </div>
      } @else if (!pending() && !error()) {
        <p class="empty">
          Pick a range and generate a synthesis of your recent meetings.
        </p>
      }
    </div>
  `,
  styles: [
    `
      :host {
        display: block;
      }

      .panel {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
        animation: rise 420ms var(--transition) both;
      }
      .panel-head {
        display: flex;
        align-items: baseline;
        justify-content: space-between;
        gap: var(--space-3);
      }
      .panel-head h3 {
        margin: 0;
      }
      .panel-note {
        color: var(--text-muted);
        font-size: 0.8125rem;
        font-weight: 550;
        letter-spacing: 0.01em;
      }

      /* --- Controls --- */
      .digest-controls {
        display: flex;
        align-items: center;
        flex-wrap: wrap;
        gap: var(--space-3);
      }
      .seg {
        display: inline-flex;
        padding: 3px;
        gap: 3px;
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-md);
        background: var(--surface-input);
      }
      .seg-btn {
        padding: var(--space-2) var(--space-3);
        border: none;
        border-radius: var(--radius-sm);
        background: transparent;
        color: var(--text-secondary);
        font-family: inherit;
        font-size: 0.8125rem;
        font-weight: 550;
        font-variant-numeric: tabular-nums;
        line-height: 1;
        cursor: pointer;
        transition:
          background var(--transition),
          color var(--transition);
      }
      .seg-btn:hover:not(.is-active):not(:disabled) {
        background: var(--surface-hover);
        color: var(--text-primary);
      }
      .seg-btn.is-active {
        background: var(--accent-soft);
        color: var(--accent-hover);
      }
      .seg-btn:focus-visible {
        outline: none;
        box-shadow: 0 0 0 2px var(--accent-ring);
      }
      .seg-btn:disabled {
        cursor: not-allowed;
        opacity: 0.5;
      }
      .digest-go {
        flex: none;
      }
      .digest-spin {
        width: 14px;
        height: 14px;
        border-radius: 50%;
        border: 2px solid rgba(255, 255, 255, 0.35);
        border-top-color: var(--text-on-accent);
        animation: digest-spin 0.7s linear infinite;
      }

      /* --- Result --- */
      .digest-saved {
        margin: 0;
        color: var(--text-secondary);
        font-size: 0.8125rem;
      }
      .digest-path {
        color: var(--text-primary);
        font-family: var(--font-mono);
        font-size: 0.75rem;
        overflow-wrap: anywhere;
      }
      .digest-output {
        max-height: 420px;
        overflow-y: auto;
        padding: var(--space-4);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        color: var(--text-primary);
        font-size: 0.9375rem;
        line-height: 1.65;
        /* Preserve the model's line breaks + spacing as plain text. */
        white-space: pre-wrap;
        overflow-wrap: anywhere;
        overscroll-behavior: contain;
        animation: rise 320ms var(--transition) both;
      }
      .digest-output:focus-visible {
        outline: none;
        border-color: var(--accent);
        box-shadow: 0 0 0 3px var(--accent-ring);
      }

      /* --- Error --- */
      .digest-error {
        padding: var(--space-3);
        border: 1px solid rgba(255, 107, 107, 0.3);
        border-radius: var(--radius-md);
        background: var(--danger-soft);
        color: var(--text-primary);
        font-size: 0.875rem;
      }

      @keyframes digest-spin {
        to {
          transform: rotate(360deg);
        }
      }

      @media (prefers-reduced-motion: reduce) {
        .panel,
        .digest-output {
          animation: none;
        }
        .digest-spin {
          animation-duration: 0.01ms;
        }
      }
    `,
  ],
})
export class WeeklyDigestComponent {
  private readonly ipc = inject(IpcService);

  /** Selectable ranges (days). */
  protected readonly ranges = RANGES;

  /** Currently-selected range, in days. */
  readonly range = signal<Range>(7);
  /** True while {@link IpcService.generateDigest} is in flight. */
  readonly pending = signal(false);
  /** The latest generated digest; null until one is produced. */
  readonly result = signal<DigestResult | null>(null);
  /** Inline error message; null when clear. */
  readonly error = signal<string | null>(null);

  /** The vault path the digest was written to, if any. */
  readonly savedPath = computed(() => this.result()?.exportedPath ?? null);

  /** Switch the active range (ignored while a generation is in flight). */
  setRange(r: Range): void {
    if (this.pending()) {
      return;
    }
    this.range.set(r);
  }

  /**
   * Generate a digest over the selected window. Awaits the one-shot IPC call
   * (no subscribe), surfaces an inline error on failure, and replaces any prior
   * result on success.
   */
  async generate(): Promise<void> {
    if (this.pending()) {
      return;
    }
    this.pending.set(true);
    this.error.set(null);
    try {
      this.result.set(await this.ipc.generateDigest(this.range()));
    } catch (e) {
      this.error.set("Couldn’t generate the digest: " + String(e));
    } finally {
      this.pending.set(false);
    }
  }
}
