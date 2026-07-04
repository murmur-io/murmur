import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../../settings.store";
import { AiRoleRowsComponent } from "./ai-role-rows.component";

/**
 * AI & Models → Block B: WHAT MURMUR USES. The "Default AI" row (the Provider
 * select moved from General) + the Default-model picker (options fetched via
 * `list_models` — the backend constant is the single source of truth, no more
 * hardcoded Claude ids) + reasoning effort, then the Stage-4 "Customize per
 * feature" override rows (AiRoleRowsComponent — the Ask row SUPERSEDES the old
 * "Assistant backend" select, and the GGUF registry lives there now), then
 * "Live during meetings" (the voice-assistant + proactive toggles, unchanged)
 * and "On-device intelligence" (fixed always-on-device badges + semantic
 * search).
 */
@Component({
  selector: "app-ai-defaults-block",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule, AiRoleRowsComponent],
  template: `
    <div class="card defaults-card" [formGroup]="form">
      <div class="defaults-head">
        <h3>What Murmur uses</h3>
        <p class="text-secondary defaults-sub">
          One default AI powers everything Murmur writes; individual features
          can run differently below.
        </p>
      </div>

      <label class="field">
        <span class="field-label">Default AI</span>
        <select formControlName="providerId" (change)="onDefaultAiChanged($event)">
          <option value="claude_code">Claude Code (default)</option>
          <option value="anthropic">Anthropic API</option>
          <option value="ollama">Ollama</option>
          <option value="gateway">Kong AI Gateway (OpenAI-compatible)</option>
        </select>
        <span class="field-help text-muted">
          Used for everything Murmur writes: notes, answers, digests, briefs.
          Set the connection up in the Providers block above.
        </span>
      </label>

      <!--
        Model + reasoning-effort overrides. providerModel steers ONLY the
        claude_code/anthropic arms (gateway/ollama read gateway_model /
        ollama_model instead), so the dropdown renders only for those two —
        for gateway/ollama we point at the connection card that actually holds
        the model. The old "Anthropic model" free-text is intentionally
        UNRENDERED (its FormControl still round-trips in the store).
      -->
      <div class="brain-tuning">
        @switch (form.controls.providerId.value) {
          @case ("gateway") {
            <p class="brain-note text-muted">
              The model for Kong AI Gateway is set in its connection card above.
            </p>
          }
          @case ("ollama") {
            <p class="brain-note text-muted">
              The model for Ollama is set in its connection card above.
            </p>
          }
          @default {
            <!-- div.field (not label) — the control sits in a nested row div,
                 same as the gateway card's Model field. -->
            <div class="field">
              <span class="field-label">Default model</span>
              <!--
                Options come from list_models (the backend Claude-id constant —
                single source of truth, no hardcoded ids here). Empty catalog
                (fetch failed / older backend) → free-text fallback; a saved
                model missing from the catalog stays selectable as "(custom)"
                — the gateway picker's keep-manually-typed pattern.
              -->
              <div class="default-model-row">
                @if (defaultModelCatalog().length > 0) {
                  <select
                    formControlName="providerModel"
                    class="default-model-select"
                  >
                    <option value="">Default (provider's pick)</option>
                    @for (id of defaultModelCatalog(); track id) {
                      <option [value]="id">{{ id }}</option>
                    }
                    @if (defaultModelIsCustom()) {
                      <option [value]="form.controls.providerModel.value">
                        {{ form.controls.providerModel.value }} (custom)
                      </option>
                    }
                  </select>
                } @else {
                  <input
                    formControlName="providerModel"
                    placeholder="Model id (blank = provider's pick)"
                    autocomplete="off"
                    spellcheck="false"
                    class="default-model-input"
                  />
                }
                <button
                  type="button"
                  class="btn btn-ghost default-model-refresh"
                  (click)="refreshDefaultModels()"
                  [disabled]="defaultModelsLoading()"
                  title="Fetch this provider's model list"
                >
                  @if (defaultModelsLoading()) {
                    Loading…
                  } @else {
                    ↻ Refresh
                  }
                </button>
              </div>
              <span class="field-help text-muted">
                Used for everything Murmur writes with AI: meeting notes,
                answers, digests and briefs. Default lets the provider choose.
              </span>
            </div>
          }
        }

        @if (form.controls.providerId.value === "anthropic") {
          <label class="field">
            <span class="field-label">Reasoning effort</span>
            <select formControlName="providerEffort">
              <option value="">Default</option>
              <option value="low">Low</option>
              <option value="medium">Medium</option>
              <option value="high">High</option>
            </select>
            <span class="field-help text-muted">
              Applies to the Anthropic provider — higher effort spends more
              thinking on harder questions.
            </span>
          </label>
        }
      </div>

      <!--
        Stage 4 — per-feature overrides (Notes / Ask / Live). The Ask row is
        the SUCCESSOR of the old "Assistant backend" select (removed from this
        block): Local/Off are selectable targets there, and the GGUF registry
        renders inside the rows block when a row picks Local.
      -->
      <app-ai-role-rows />

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

      /* Light regrouping headings (Ask & assistant / Live / On-device). */
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

      .brain-tuning {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }

      /* #20 — proactive cloud-egress consent warning under the assistant toggle. */
      .realtime-consent {
        flex-direction: column;
        gap: var(--space-3);
      }
      .realtime-consent-copy {
        line-height: 1.55;
      }
      .brain-note {
        margin: 0;
        font-size: 0.8125rem;
        line-height: 1.5;
      }
      .brain-error {
        margin: 0;
        font-size: 0.85rem;
      }

      /* Default-model picker — select-or-input + the catalog refresh. */
      .default-model-row {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex-wrap: wrap;
      }
      .default-model-select,
      .default-model-input {
        flex: 1 1 220px;
        min-width: 0;
      }
      .default-model-refresh {
        flex: none;
        white-space: nowrap;
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

      /* --- Stacked label + control --- */
      .field {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .field-label {
        color: var(--text-secondary);
        font-size: 0.9rem;
        font-weight: 550;
      }
      .field-help {
        font-size: 0.8125rem;
        line-height: 1.5;
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
  readonly defaultModelCatalog = this.store.defaultModelCatalog;
  readonly defaultModelsLoading = this.store.defaultModelsLoading;
  readonly defaultModelIsCustom = this.store.defaultModelIsCustom;
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

  /** Prefetch the newly-picked Default AI's model catalog (claude_code/anthropic only). */
  onDefaultAiChanged(e: Event): void {
    const id = (e.target as HTMLSelectElement).value;
    if (id === "claude_code" || id === "anthropic") {
      void this.store.ensureModels(id);
    }
  }

  /** Re-fetch the Default-model catalog for the current provider. */
  refreshDefaultModels(): void {
    void this.store.refreshModels(this.form.controls.providerId.value);
  }

  downloadEmbedModel(): void {
    void this.store.downloadEmbedModel();
  }

  reindexEmbeddings(): void {
    void this.store.reindexEmbeddings();
  }
}
