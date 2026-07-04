import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../../settings.store";

/**
 * AI & Models → Block C: LIVE DURING MEETINGS + ON-DEVICE INTELLIGENCE.
 *
 * After Task 4 the Default AI / Default model / reasoning effort and the
 * per-feature role rows moved to `AiAdvancedBlockComponent`. This card now
 * owns the two always-visible sections that are NOT behind the Advanced
 * disclosure:
 *
 *  • "Live during meetings" — in-meeting voice assistant + proactive hints
 *    toggles, plus the cloud-egress consent warning when needed.
 *  • "On-device intelligence" — fixed always-on-device badges
 *    (Embeddings / Name redaction / Transcription) + the semantic-search
 *    toggle, embedding-model download, and re-index controls.
 */
@Component({
  selector: "app-ai-defaults-block",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  template: `
    <div class="card defaults-card" [formGroup]="form">
      <div class="defaults-head">
        <h3>What Murmur uses</h3>
        <p class="text-secondary defaults-sub">
          One default AI powers everything Murmur writes; individual features
          can run differently below.
        </p>
      </div>

      <!-- ── Live during meetings ────────────────────────────────────── -->
      <div class="use-group">
        <span class="use-group-label text-muted">Live during meetings</span>

        <label class="toggle-row">
          <span class="toggle-copy">
            <span class="toggle-title">In-meeting voice assistant</span>
            <span class="text-secondary toggle-sub">
              Listen for your wake phrase during a recording and answer
              grounded questions live, with sources. Off by default — it adds
              listening and (for cloud) sends audio-derived text mid-meeting.
            </span>
          </span>
          <input type="checkbox" formControlName="realtimeReactions" />
        </label>

        <label class="toggle-row">
          <span class="toggle-copy">
            <span class="toggle-title">Proactive brain hints</span>
            <span class="text-secondary toggle-sub">
              While recording, surface a dismissible recall card when the
              conversation touches a past meeting, an open commitment, or a
              known fact. 100% on-device — no cloud calls; at most one card
              every two minutes.
            </span>
          </span>
          <input type="checkbox" formControlName="proactiveHintsEnabled" />
        </label>

        <!--
          Proactive cloud-egress consent (issue 20). The in-meeting assistant
          dispatches voice actions through the LIVE role's resolved target —
          since Stage 4 that need not be brainBackend/the default provider, so
          the condition keys on liveTargetIsCloud (the store's resolver
          mirror: explicit roleLiveConnection wins, "" falls back to the
          brainBackend mapping, ollama is cloud only off-loopback). Surface
          the requirement at enable time: realtime on, live target
          cloud-classified, not consented. Reuses the existing consent flow
          (allowCloudProcessing). In-flow warning, so the frosted banner is
          correct (no opaque overlay needed).
        -->
        @if (
          form.controls.realtimeReactions.value &&
          liveTargetIsCloud() &&
          !cloudConsented()
        ) {
          <div class="banner is-warning realtime-consent">
            <span class="realtime-consent-copy">
              ⚠ The in-meeting assistant sends live meeting context to your
              provider's cloud (redacted first). Allow cloud processing once,
              or live answers stay off.
            </span>
            <div class="cloud-consent-row">
              <button
                type="button"
                class="btn btn-primary"
                (click)="allowCloudProcessing()"
                [disabled]="consenting()"
              >
                @if (consenting()) {
                  <span class="spin-ring" aria-hidden="true"></span>
                  Enabling…
                } @else {
                  Allow
                }
              </button>
              <span class="text-muted cloud-consent-hint">
                One-time, redacted first. Same consent as cloud summaries.
              </span>
            </div>
            @if (consentError(); as cerr) {
              <p class="text-danger privacy-note">{{ cerr }}</p>
            }
          </div>
        }
      </div>

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
            <input type="checkbox" formControlName="semanticSearchEnabled" />
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

      .defaults-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .defaults-head {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .defaults-head h3 {
        margin: 0;
      }
      .defaults-sub {
        margin: 0;
        font-size: 0.875rem;
        line-height: 1.55;
      }

      /* Light regrouping headings (Live / On-device). */
      .use-group {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        padding-top: var(--space-3);
        border-top: 1px solid var(--border-subtle);
      }
      .use-group-label {
        font-size: 0.8125rem;
        font-weight: 550;
        letter-spacing: 0.01em;
        text-transform: uppercase;
      }

      /* #20 — proactive cloud-egress consent warning under the assistant toggle. */
      .realtime-consent {
        flex-direction: column;
        gap: var(--space-3);
      }
      .realtime-consent-copy {
        line-height: 1.55;
      }
      .brain-error {
        margin: 0;
        font-size: 0.85rem;
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
      .defaults-card .btn-sm {
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

      /* --- Toggle rows --- */
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

      /* Cloud-processing consent — button + reassurance. */
      .cloud-consent-row {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
        margin-top: var(--space-1);
      }
      .cloud-consent-row .btn {
        flex: none;
      }
      .cloud-consent-hint {
        font-size: 0.85rem;
        line-height: 1.5;
      }
      .privacy-note {
        margin: 0;
        font-size: 0.9rem;
        line-height: 1.55;
      }

      /* Inline spinner on the Download button (matches the onboarding wizard). */
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
export class AiDefaultsBlockComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;
  readonly cloudConsented = this.store.cloudConsented;
  readonly consenting = this.store.consenting;
  readonly consentError = this.store.consentError;
  readonly liveTargetIsCloud = this.store.liveTargetIsCloud;
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

  allowCloudProcessing(): void {
    void this.store.allowCloudProcessing();
  }

  downloadEmbedModel(): void {
    void this.store.downloadEmbedModel();
  }

  reindexEmbeddings(): void {
    void this.store.reindexEmbeddings();
  }
}
