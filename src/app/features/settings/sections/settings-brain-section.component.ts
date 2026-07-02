import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../settings.store";

/**
 * Settings → brain section (Stage-1 split): the `@case ("brain")` block of the
 * former settings.component.ts monolith, moved VERBATIM. State/actions live in
 * the shell-provided SettingsStore so section switches never drop them.
 */
@Component({
  selector: "app-settings-brain-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  template: `
    <div class="section-stack" [formGroup]="form">
              <div class="card brain-card">
                <div class="brain-copy">
                  <h3>Brain / AI</h3>
                  <p class="text-secondary brain-sub">
                    Powers grounded answers across your notes and the optional in-meeting
                    voice assistant. Your default AI is fastest for live use; a local
                    model keeps assistant reasoning on-device but is slower in real time.
                  </p>
                </div>

                <label class="field">
                  <span class="field-label">Assistant backend</span>
                  <select formControlName="brainBackend">
                    <option value="cloud">My default AI — recommended for live</option>
                    <option value="local">Local model — assistant reasoning on-device</option>
                    <option value="off">Off</option>
                  </select>
                  <span class="field-help text-muted">
                    @switch (form.controls.brainBackend.value) {
                      @case ("local") {
                        Runs assistant reasoning and note pre-analysis on-device (pick a
                        model below). Note summaries and Ask fallback still use your
                        provider from General.
                      }
                      @case ("off") {
                        Assistant answers become retrieval-only (no AI model). The
                        in-meeting voice assistant toggle below stays independent.
                      }
                      @default {
                        Uses the provider selected in General (redacted before any cloud
                        call) — lowest latency, best for the live voice assistant.
                      }
                    }
                  </span>
                </label>

                <label class="toggle-row">
                  <span class="toggle-copy">
                    <span class="toggle-title">In-meeting voice assistant</span>
                    <span class="text-secondary toggle-sub">
                      Listen for your wake phrase during a recording and answer grounded
                      questions live, with sources. Off by default — it adds listening
                      and (for cloud) sends audio-derived text mid-meeting.
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
                  dispatches voice actions through the active provider. With a
                  cloud-classified provider (providerIsCloud mirrors the backend's
                  egress_is_cloud: claude_code/anthropic/gateway, plus ollama on a
                  non-loopback base URL) it uploads mid-meeting context, and the
                  dispatch is fail-closed behind cloud_egress_consented. Surface the
                  requirement at enable time. Condition: realtime on, cloud-classified
                  provider, brain not off, not consented. Reuses the existing consent
                  flow (allowCloudProcessing). In-flow warning, so the frosted banner
                  is correct (no opaque overlay needed).
                -->
                @if (
                  form.controls.realtimeReactions.value &&
                  form.controls.brainBackend.value === "cloud" &&
                  providerIsCloud() &&
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

                <!--
                  Model + reasoning-effort overrides. providerModel steers ONLY the
                  claude_code/anthropic arms (gateway/ollama read gateway_model /
                  ollama_model instead), so the dropdown renders only for those two —
                  for gateway/ollama we point at the provider card that actually holds
                  the model. The hidden control keeps its value and round-trips on save.
                -->
                <div class="brain-tuning">
                  @switch (form.controls.providerId.value) {
                    @case ("gateway") {
                      <p class="brain-note text-muted">
                        The model for AI Gateway is set in Settings → Providers → AI
                        Gateway.
                      </p>
                    }
                    @case ("ollama") {
                      <p class="brain-note text-muted">
                        The model for Ollama is set in Settings → Providers.
                      </p>
                    }
                    @default {
                      <label class="field">
                        <span class="field-label">Default model</span>
                        <select formControlName="providerModel">
                          <option value="">Default (provider's pick)</option>
                          <option value="claude-opus-4-8">Opus 4.8</option>
                          <option value="claude-sonnet-4-6">Sonnet 4.6</option>
                          <option value="claude-haiku-4-5">Haiku 4.5</option>
                        </select>
                        <span class="field-help text-muted">
                          Used for everything Murmur writes with AI: meeting notes,
                          answers, digests and briefs. Default lets the provider choose.
                        </span>
                      </label>
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

                <!-- Local model picker — only meaningful for the local backend. -->
                @if (form.controls.brainBackend.value === "local") {
                  <div class="brain-models">
                    <div class="brain-models-head">
                      <span class="brain-models-label text-muted">Local models</span>
                      <button
                        type="button"
                        class="btn btn-sm"
                        (click)="refreshBrainModels()"
                        [disabled]="brainModelsLoading()"
                      >
                        {{ brainModelsLoading() ? "Loading…" : "Refresh" }}
                      </button>
                    </div>

                    <p class="brain-note text-muted">
                      Big local models are slow for the realtime voice assistant —
                      your default AI is recommended for live answers. Local is best for
                      private, non-time-critical analysis.
                    </p>

                    @if (brainModels(); as models) {
                      @if (models.length === 0 && !brainModelsLoading()) {
                        <p class="brain-empty text-muted">
                          No local models available.
                        </p>
                      } @else {
                        <ul class="brain-model-list">
                          @for (m of models; track m.id) {
                            <li
                              class="brain-model-row"
                              [class.is-unfit]="!m.fitsRam"
                              [class.is-selected]="m.selected"
                            >
                              <div class="brain-model-info">
                                <span class="brain-model-name">
                                  {{ m.name }}
                                  @if (m.selected) {
                                    <span class="pill is-success brain-inline-pill">
                                      <span class="pill-dot"></span>
                                      In use
                                    </span>
                                  }
                                </span>
                                <span class="brain-model-meta text-muted">
                                  {{ m.sizeLabel }} · needs ≥{{ m.minRamGb }} GB RAM
                                  @if (m.languages.length > 0) {
                                    · {{ m.languages.join("/") }}
                                  }
                                </span>
                                @if (!m.fitsRam) {
                                  <span class="pill is-warning brain-fit-pill">
                                    <span class="pill-dot"></span>
                                    May not fit this Mac's RAM
                                  </span>
                                }
                              </div>

                              <div class="brain-model-actions">
                                @if (brainDownloadingId() === m.id) {
                                  <div class="brain-progress" role="status">
                                    <div class="brain-progress-track" aria-hidden="true">
                                      <div
                                        class="brain-progress-fill"
                                        [style.width.%]="brainDownloadFrac() * 100"
                                      ></div>
                                    </div>
                                    <span class="brain-progress-label text-muted">
                                      Downloading… {{ brainPct() }}
                                    </span>
                                  </div>
                                } @else if (m.downloaded) {
                                  <button
                                    type="button"
                                    class="btn btn-sm"
                                    (click)="useBrainModel(m.id)"
                                    [disabled]="m.selected"
                                  >
                                    {{ m.selected ? "Selected" : "Use" }}
                                  </button>
                                } @else {
                                  <button
                                    type="button"
                                    class="btn btn-primary btn-sm"
                                    (click)="downloadBrainModel(m.id)"
                                    [disabled]="brainDownloadingId() !== null"
                                  >
                                    Download
                                  </button>
                                }
                              </div>
                            </li>
                          }
                        </ul>
                      }
                    }

                    <label class="field brain-custom">
                      <span class="field-label">Custom GGUF model</span>
                      <input
                        formControlName="brainModelId"
                        placeholder="/path/to/model.gguf or a registry id"
                      />
                      <span class="field-help text-muted">
                        Advanced: point at your own GGUF file (or a registry id). Saved
                        with your settings.
                      </span>
                    </label>

                    @if (brainError(); as berr) {
                      <p class="text-danger brain-error">{{ berr }}</p>
                    }
                  </div>
                }

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
                          Download embedding model (~120 MB)
                        </button>
                        <span class="text-muted semantic-note">
                          One time, on-device — required before semantic search can index.
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
                        Download the embedding model above first — semantic search can't
                        index without it.
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
      /* Stage-1 split: the host stays layout-transparent so this section's
         cards remain direct flex items of the shell's .section-body (identical
         spacing to the pre-split monolith); .section-stack reproduces the
         .section-body column gap between this section's own cards. */
      :host {
        display: contents;
      }
      .section-stack {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
      }

      /* --- Brain / AI card (Phase H) --- */
      .brain-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .brain-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .brain-copy h3 {
        margin: 0;
      }
      .brain-sub {
        margin: 0;
        font-size: 0.875rem;
        line-height: 1.55;
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
      .brain-models {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        padding: var(--space-4);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
      }
      .brain-models-head {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
      }
      .brain-models-label {
        font-size: 0.8125rem;
        font-weight: 550;
        letter-spacing: 0.01em;
        text-transform: uppercase;
      }
      .brain-note {
        margin: 0;
        font-size: 0.8125rem;
        line-height: 1.5;
      }
      .brain-empty {
        margin: 0;
        font-size: 0.875rem;
      }
      .brain-model-list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .brain-model-row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
        padding: var(--space-3);
        border-radius: var(--radius-md);
        background: var(--surface-raised);
        border: 1px solid var(--glass-border);
      }
      .brain-model-row.is-selected {
        border-color: var(--accent-hover);
      }
      .brain-model-row.is-unfit {
        opacity: 0.78;
      }
      .brain-model-info {
        display: flex;
        flex-direction: column;
        gap: 3px;
        min-width: 0;
      }
      .brain-model-name {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        color: var(--text-primary);
        font-weight: 550;
        font-size: 0.9rem;
        flex-wrap: wrap;
      }
      .brain-model-meta {
        font-size: 0.8125rem;
      }
      .brain-inline-pill,
      .brain-fit-pill {
        align-self: flex-start;
      }
      .brain-fit-pill {
        margin-top: 2px;
      }
      .brain-model-actions {
        flex: none;
        display: flex;
        align-items: center;
        gap: var(--space-2);
      }
      .brain-progress {
        display: flex;
        flex-direction: column;
        gap: 3px;
        min-width: 120px;
      }
      .brain-progress-track {
        height: 6px;
        border-radius: 3px;
        background: var(--surface-input);
        overflow: hidden;
      }
      .brain-progress-fill {
        height: 100%;
        background: var(--accent);
        border-radius: 3px;
        transition: width var(--transition);
      }
      .brain-progress-label {
        font-size: 0.75rem;
      }
      .brain-custom {
        margin-top: var(--space-1);
      }
      .brain-error {
        margin: 0;
        font-size: 0.85rem;
      }
      .brain-card .btn-sm {
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

      /* One-line helper that tracks the selected summary style. */
      .field-help {
        font-size: 0.8125rem;
        line-height: 1.5;
      }

      /* --- Capture-system-audio toggle row --- */
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

      /* Cloud-processing consent — button + reassurance, or the granted pill. */
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
      .cloud-consent-pill {
        align-self: flex-start;
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
export class SettingsBrainSectionComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;
  readonly providerIsCloud = this.store.providerIsCloud;
  readonly cloudConsented = this.store.cloudConsented;
  readonly consenting = this.store.consenting;
  readonly consentError = this.store.consentError;
  readonly brainModels = this.store.brainModels;
  readonly brainModelsLoading = this.store.brainModelsLoading;
  readonly brainError = this.store.brainError;
  readonly brainDownloadingId = this.store.brainDownloadingId;
  readonly brainDownloadFrac = this.store.brainDownloadFrac;
  readonly brainPct = this.store.brainPct;
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

  refreshBrainModels(): void {
    void this.store.refreshBrainModels();
  }

  useBrainModel(id: string): void {
    void this.store.useBrainModel(id);
  }

  downloadBrainModel(id: string): void {
    void this.store.downloadBrainModel(id);
  }

  downloadEmbedModel(): void {
    void this.store.downloadEmbedModel();
  }

  reindexEmbeddings(): void {
    void this.store.reindexEmbeddings();
  }
}
