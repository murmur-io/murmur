import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../../settings.store";

/**
 * AI & Models → "On-device intelligence" block (Task 5).
 *
 * Extracted verbatim from AiDefaultsBlockComponent as a standalone card.
 * Owns the always-on-device honesty badges (Embeddings / Name redaction /
 * Transcription), the semantic-search toggle, the embedding-model download
 * flow, and the re-index controls.
 *
 * All work is on-device — no cloud calls, no consent requirement.
 */
@Component({
  selector: "app-on-device-intelligence-block",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  template: `
    <div class="card ondevice-card" [formGroup]="form">
      <!-- ── On-device intelligence ──────────────────────────────────── -->
      <div class="use-group">
        <span class="use-group-label text-muted">On-device intelligence</span>

        <!--
          Fixed "always on-device" badges — these stages are NOT routable to
          any provider (they activate on local model presence), so they stay
          out of every picker above. Honesty line, not controls.
        -->
        <div class="ondevice-badges">
          <span class="pill">
            <span class="pill-dot"></span>
            Embeddings
          </span>
          <span class="pill">
            <span class="pill-dot"></span>
            Name redaction
          </span>
          <span class="pill">
            <span class="pill-dot"></span>
            Transcription
          </span>
          <span class="text-muted ondevice-note">
            Always run on this Mac — never sent to any provider.
          </span>
        </div>

        <!-- brain2 RAG — semantic search over your notes (embedding model + reindex) -->
        <div class="semantic">
          <label class="toggle-row">
            <span class="toggle-copy">
              <span class="toggle-title">Semantic search (multilingual)</span>
              <span class="text-secondary toggle-sub">
                Finds notes by meaning + across languages — needs the embedding
                model.
              </span>
            </span>
            <input class="switch" type="checkbox" formControlName="semanticSearchEnabled" />
          </label>

          <!-- Embedding model: present pill, or a download control with progress -->
          <div class="semantic-model-row">
            @if (embedModelPresent() === true) {
              <span class="pill is-success">
                <span class="pill-dot"></span>
                Embedding model ready ✓
              </span>
              <span class="text-muted semantic-note">
                Stored on this Mac — used to index + search your notes.
              </span>
            } @else if (embedModelPresent() === false) {
              @if (downloadingEmbedModel()) {
                <div class="semantic-progress" role="status">
                  <div class="semantic-progress-track" aria-hidden="true">
                    <div
                      class="semantic-progress-fill"
                      [style.width.%]="embedDownloadFrac() * 100"
                    ></div>
                  </div>
                  <span class="semantic-progress-label text-muted">
                    Downloading embedding model… {{ embedPct() }}
                  </span>
                </div>
              } @else {
                <button
                  type="button"
                  class="btn btn-primary btn-sm"
                  (click)="downloadEmbedModel()"
                >
                  Download embedding model (~470 MB)
                </button>
                <span class="text-muted semantic-note">
                  One time, on-device — required before semantic search can
                  index.
                </span>
              }
            } @else {
              <span class="pill">
                <span class="pill-dot"></span>
                Checking…
              </span>
            }
          </div>
          @if (embedDownloadError(); as eerr) {
            <p class="text-danger brain-error">{{ eerr }}</p>
          }

          <!-- Re-index notes: backfill the semantic vector index over all notes -->
          <div class="semantic-reindex">
            <button
              type="button"
              class="btn btn-sm"
              (click)="reindexEmbeddings()"
              [disabled]="reindexing()"
            >
              @if (reindexing()) {
                <span class="spin-ring" aria-hidden="true"></span>
                Re-indexing…
              } @else {
                Re-index notes
              }
            </button>
            <span class="text-muted semantic-note">
              Builds the semantic index over your notes — run it after turning
              this on, or after downloading the model.
            </span>
          </div>
          @if (reindexing()) {
            <div class="semantic-progress" role="status">
              <div class="semantic-progress-track" aria-hidden="true">
                <div
                  class="semantic-progress-fill"
                  [style.width.%]="reindexFrac() * 100"
                ></div>
              </div>
              <span class="semantic-progress-label text-muted">
                Indexing notes… {{ reindexPct() }}
              </span>
            </div>
          }
          @if (reindexResult(); as rr) {
            @if (rr.status === "model_missing") {
              <p class="semantic-nudge text-secondary">
                Download the embedding model above first — semantic search
                can't index without it.
              </p>
            } @else {
              <span class="pill is-success semantic-done-pill">
                <span class="pill-dot"></span>
                Indexed {{ rr.indexed }} of {{ rr.total }} notes
              </span>
            }
          }
          @if (reindexError(); as rerr) {
            <p class="text-danger brain-error">{{ rerr }}</p>
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

      .ondevice-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }

      /* Light regrouping heading. */
      .use-group {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .use-group-label {
        font-size: 0.8125rem;
        font-weight: 550;
        letter-spacing: 0.01em;
        text-transform: uppercase;
      }

      /* Fixed always-on-device badges (not controls). */
      .ondevice-badges {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex-wrap: wrap;
      }
      .ondevice-note {
        font-size: 0.8125rem;
        line-height: 1.5;
      }
      .btn-sm {
        height: 32px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
      }

      /* --- brain2 RAG — semantic-search subsection --- */
      .semantic {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        padding: var(--space-4);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
      }
      .semantic-model-row,
      .semantic-reindex {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
        min-height: 36px;
      }
      .semantic-reindex .btn,
      .semantic-model-row .btn {
        flex: none;
      }
      .semantic-note {
        font-size: 0.8125rem;
        line-height: 1.5;
      }
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
      .semantic-nudge {
        margin: 0;
        font-size: 0.85rem;
        line-height: 1.5;
      }
      .semantic-done-pill {
        align-self: flex-start;
      }
      .brain-error {
        margin: 0;
        font-size: 0.85rem;
      }

      /* Toggle row (for semantic-search checkbox). */
      .toggle-row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-4);
        cursor: pointer;
      }
      .toggle-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .toggle-title {
        color: var(--text-primary);
        font-size: 0.95rem;
        font-weight: 550;
      }
      .toggle-sub {
        font-size: 0.85rem;
      }

      /* Inline spinner on the Re-indexing button. */
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
export class OnDeviceIntelligenceBlockComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;
  readonly embedModelPresent = this.store.embedModelPresent;
  readonly downloadingEmbedModel = this.store.downloadingEmbedModel;
  readonly embedDownloadFrac = this.store.embedDownloadFrac;
  readonly embedPct = this.store.embedPct;
  readonly embedDownloadError = this.store.embedDownloadError;
  readonly reindexing = this.store.reindexing;
  readonly reindexFrac = this.store.reindexFrac;
  readonly reindexPct = this.store.reindexPct;
  readonly reindexResult = this.store.reindexResult;
  readonly reindexError = this.store.reindexError;

  downloadEmbedModel(): void {
    void this.store.downloadEmbedModel();
  }

  reindexEmbeddings(): void {
    void this.store.reindexEmbeddings();
  }
}
