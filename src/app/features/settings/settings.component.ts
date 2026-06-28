import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  OnInit,
  computed,
  inject,
  signal,
} from "@angular/core";
import { FormBuilder, FormControl, ReactiveFormsModule } from "@angular/forms";
import { Router } from "@angular/router";
import { open } from "@tauri-apps/plugin-dialog";
import { IpcService } from "../../core/ipc.service";
import type {
  AppConfigDto,
  BrainBackend,
  BrainModelDto,
  InputDeviceInfo,
  ProviderStatus,
  ReindexResult,
} from "../../core/models";
import type { UnlistenFn } from "@tauri-apps/api/event";

@Component({
  selector: "app-settings",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  template: `
    <section class="settings" [formGroup]="form">
      @if (loadError(); as err) {
        <div class="banner is-danger" role="alert">
          <span class="banner-icon" aria-hidden="true">!</span>
          <span>Couldn't load settings: {{ err }}</span>
        </div>
      }

      <!-- Transcription model: language + quality + on-demand download -->
      <div class="card model-card">
        <div class="model-copy">
          <h3>Transcription model</h3>
          <p class="text-secondary model-sub">
            Runs entirely on-device. Pick your language and quality — the
            matching Whisper model is fetched once and reused for every
            recording.
          </p>
        </div>

        <div class="model-grid">
          <label class="field">
            <span class="field-label">Language</span>
            <select formControlName="language" (change)="onModelChoiceChange()">
              <option value="">Auto-detect</option>
              <option value="pl">Polski</option>
              <option value="en">English</option>
              <option value="de">Deutsch</option>
              <option value="es">Español</option>
              <option value="fr">Français</option>
              <option value="it">Italiano</option>
              <option value="pt">Português</option>
              <option value="uk">Українська</option>
              <option value="nl">Nederlands</option>
            </select>
            <span class="field-help text-muted">
              Force the transcription language. Polish recommended if you record
              mostly in Polish (auto-detect can misfire on short clips).
            </span>
          </label>

          <label class="field">
            <span class="field-label">Quality</span>
            <select
              formControlName="modelSize"
              (change)="onModelChoiceChange()"
            >
              <option value="tiny">Tiny — fastest (~75 MB)</option>
              <option value="base">Base (~150 MB)</option>
              <option value="small">Small (~470 MB)</option>
              <option value="medium">Medium (~1.5 GB)</option>
              <option value="large-v3-turbo">
                Large v3 Turbo — fast &amp; accurate (~1.6 GB)
              </option>
              <option value="large-v3">
                Large v3 — best accuracy, recommended (~3 GB)
              </option>
            </select>
            <span class="field-help text-muted">
              Large v3 is the most accurate and the default — it’s a one-time ~3
              GB download. Turbo is nearly as good and much smaller.
            </span>
          </label>
        </div>

        <div class="model-status-row">
          @if (modelPresent() === true) {
            <span class="pill is-success">
              <span class="pill-dot"></span>
              Downloaded ✓
            </span>
            <span class="text-muted model-note">
              Stored on this Mac — used for every recording.
            </span>
          } @else if (modelPresent() === false) {
            <button
              type="button"
              class="btn btn-primary"
              (click)="downloadModel()"
              [disabled]="downloadingModel()"
            >
              @if (downloadingModel()) {
                <span class="spin-ring" aria-hidden="true"></span>
                Downloading…
              } @else {
                Download ({{ downloadHint() }})
              }
            </button>
            <span class="text-muted model-note">
              @if (downloadingModel()) {
                Fetching the model — large models can take a few minutes.
              } @else {
                {{ downloadHint() }}, one time, on-device.
              }
            </span>
          } @else {
            <span class="pill">
              <span class="pill-dot"></span>
              Checking…
            </span>
          }
        </div>
        @if (modelDownloadError(); as derr) {
          <p class="model-error text-danger">{{ derr }}</p>
        }
      </div>

      <!-- General -->
      <div class="card">
        <fieldset>
          <legend>General</legend>

          <label class="field">
            <span class="field-label">Provider</span>
            <select formControlName="providerId">
              <option value="claude_code">Claude Code (default)</option>
              <option value="anthropic">Anthropic API</option>
              <option value="ollama">Ollama</option>
            </select>
          </label>

          <label class="field">
            <span class="field-label">Vault folder</span>
            <span class="row">
              <input formControlName="vaultPath" placeholder="/path/to/vault" />
              <button type="button" class="btn" (click)="pickVault()">
                Browse…
              </button>
            </span>
          </label>

          <label class="field">
            <span class="field-label">Vault subfolder</span>
            <input formControlName="vaultSubfolder" placeholder="Meetings" />
          </label>

          <label class="field">
            <span class="field-label"
              >Whisper model path (optional override)</span
            >
            <span class="row">
              <input
                formControlName="whisperModelPath"
                placeholder="leave blank — auto-managed in Transcription model above"
              />
              <button type="button" class="btn" (click)="pickModel()">
                Browse…
              </button>
            </span>
          </label>
        </fieldset>
      </div>

      <!-- Notes: how Claude writes & files each meeting note -->
      <div class="card notes-card">
        <div class="notes-copy">
          <h3>Notes</h3>
          <p class="text-secondary notes-sub">
            Shape how Claude writes each summary and where it lands in your
            vault.
          </p>
        </div>

        <label class="field">
          <span class="field-label">Summary style</span>
          <select formControlName="noteStyle">
            <option value="standard">Standard (balanced)</option>
            <option value="brief">Brief (TL;DR + actions)</option>
            <option value="detailed">Detailed (full depth)</option>
            <option value="action">Action-focused</option>
          </select>
          <span class="field-help text-muted">
            @switch (form.controls.noteStyle.value) {
              @case ("brief") {
                A tight TL;DR up top, then just the decisions and action items.
              }
              @case ("detailed") {
                The full picture — discussion, context, decisions and every
                follow-up.
              }
              @case ("action") {
                Front-loads who-does-what — owners, tasks and due dates first.
              }
              @default {
                A balanced summary, key points and action items — good for most
                meetings.
              }
            }
          </span>
        </label>

        <label class="field">
          <span class="field-label">Notes language</span>
          <select formControlName="noteLanguage">
            <option value="auto">Auto — match the meeting</option>
            <option value="en">English</option>
            <option value="pl">Polski</option>
            <option value="de">Deutsch</option>
            <option value="es">Español</option>
            <option value="fr">Français</option>
            <option value="it">Italiano</option>
            <option value="pt">Português</option>
            <option value="uk">Українська</option>
            <option value="nl">Nederlands</option>
          </select>
          <span class="field-help text-muted">
            @if (form.controls.noteLanguage.value === "auto") {
              The whole note (headings + content) is written in the meeting's
              language.
            } @else {
              The whole note is written in this language, whatever was spoken.
            }
          </span>
        </label>

        <label class="toggle-row">
          <span class="toggle-copy">
            <span class="toggle-title">Organize into thematic subfolders</span>
            <span class="text-secondary toggle-sub">
              Claude files each note into a topic subfolder of your vault (e.g.
              Standups, 1-1s, Acme Project).
            </span>
          </span>
          <input type="checkbox" formControlName="autoOrganize" />
        </label>
      </div>

      <!-- Brain / AI — the assistant backend + in-meeting voice assistant (Phase H) -->
      <div class="card brain-card">
        <div class="brain-copy">
          <h3>Brain / AI</h3>
          <p class="text-secondary brain-sub">
            Powers grounded answers across your notes and the optional in-meeting
            voice assistant. Claude (cloud) is fastest for live use; local models
            keep everything on-device but are slower in real time.
          </p>
        </div>

        <label class="field">
          <span class="field-label">Assistant backend</span>
          <select formControlName="brainBackend">
            <option value="cloud">Claude (cloud) — recommended for live</option>
            <option value="local">Local model — fully on-device</option>
            <option value="off">Off</option>
          </select>
          <span class="field-help text-muted">
            @switch (form.controls.brainBackend.value) {
              @case ("local") {
                Runs a local GGUF model on this Mac — private, but large models
                are slow for realtime. Pick a model below.
              }
              @case ("off") {
                The brain and the in-meeting voice assistant are disabled.
              }
              @default {
                Sends your (redacted) text to Anthropic's cloud — lowest latency,
                best for the live voice assistant.
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

        <!-- Model + reasoning-effort overrides for the active cloud provider. -->
        <div class="brain-tuning">
          <label class="field">
            <span class="field-label">Model</span>
            <select formControlName="providerModel">
              <option value="">Default (provider's pick)</option>
              <option value="claude-opus-4-8">Opus 4.8</option>
              <option value="claude-sonnet-4-6">Sonnet 4.6</option>
              <option value="claude-haiku-4-5">Haiku 4.5</option>
            </select>
            <span class="field-help text-muted">
              Overrides the model used for grounded answers — leave on Default to
              let the provider choose.
            </span>
          </label>

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
              Claude (cloud) is recommended for live answers. Local is best for
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

      <!-- Connectors — web search (NEW CLOUD EGRESS, surfaced loudly) -->
      <div class="card connectors-card">
        <div class="brain-copy">
          <h3>Connectors</h3>
          <p class="text-secondary brain-sub">
            Let the brain reach beyond your notes. Connectors are
            <strong>off by default</strong> — each one that leaves this Mac asks
            for an explicit, one-time consent first.
          </p>
        </div>

        <!-- Web search (Brave) connector -->
        <label class="toggle-row">
          <span class="toggle-copy">
            <span class="toggle-title">Web search</span>
            <span class="text-secondary toggle-sub">
              When enabled (and allowed below, with a key), the assistant can
              look facts up on the web and cite them. Answers stay grounded in
              your notes first; web results are added as “via web” sources.
            </span>
          </span>
          <input type="checkbox" formControlName="webSearchEnabled" />
        </label>

        @if (form.controls.webSearchEnabled.value) {
          <!-- Egress banner — make the new off-device path impossible to miss. -->
          <p class="banner is-warning connector-egress" role="note">
            <strong>This sends data off your Mac.</strong> When the brain runs a
            web search, your (redacted) query leaves the device and goes to the
            search provider (Brave). Only the query is sent — never your notes or
            transcript. Disable this, or skip the consent below, to keep
            everything local.
          </p>

          <!-- BYO API key (Brave) -->
          <fieldset class="connector-fieldset">
            <legend>Brave Search API key</legend>
            <div class="key-status">
              <span class="text-secondary">Status</span>
              @if (hasWebKey()) {
                <span class="pill is-success">
                  <span class="pill-dot"></span>
                  Key set ✓
                </span>
              } @else {
                <span class="pill">
                  <span class="pill-dot"></span>
                  Not set
                </span>
              }
            </div>
            <span class="row">
              <input
                type="password"
                [formControl]="webKeyControl"
                placeholder="Brave Search API key"
                autocomplete="off"
              />
              <button
                type="button"
                class="btn"
                (click)="saveWebKey()"
                [disabled]="savingWebKey()"
              >
                {{ savingWebKey() ? "Saving…" : "Save key" }}
              </button>
            </span>
            <span class="field-help text-muted">
              Bring your own key — it's stored in your macOS Keychain, never
              logged, and never leaves with your notes.
            </span>
            @if (webKeyError(); as wkerr) {
              <p class="text-danger brain-error">{{ wkerr }}</p>
            }
          </fieldset>

          <!-- One-time egress consent (mirrors the Cloud-processing UX) -->
          <div class="privacy-section connector-consent">
            <span class="privacy-section-label text-muted"
              >Allow web search</span
            >
            <p class="text-secondary privacy-note">
              Your search query leaves this device for the search provider
              (redacted first). Until you allow this once, web search stays off
              and no query is ever sent.
            </p>
            @if (webConsented()) {
              <span class="pill is-success cloud-consent-pill">
                <span class="pill-dot"></span>
                Web search allowed
              </span>
            } @else {
              <div class="cloud-consent-row">
                <button
                  type="button"
                  class="btn btn-primary"
                  (click)="allowWebSearch()"
                  [disabled]="webConsenting()"
                >
                  @if (webConsenting()) {
                    <span class="spin-ring" aria-hidden="true"></span>
                    Enabling…
                  } @else {
                    Allow web search
                  }
                </button>
                <span class="text-muted cloud-consent-hint">
                  One-time. The brain works fully offline on your notes without
                  it.
                </span>
              </div>
            }
            @if (webConsentError(); as wcerr) {
              <p class="text-danger privacy-note">{{ wcerr }}</p>
            }
          </div>
        }
      </div>

      <!-- Works with Obsidian — optional vault companion -->
      <div class="card obsidian-card">
        <div class="obsidian-head">
          <span class="obsidian-mark" aria-hidden="true">
            <svg
              viewBox="0 0 24 24"
              width="30"
              height="30"
              fill="none"
              role="img"
              aria-label="Obsidian"
            >
              <defs>
                <linearGradient
                  id="obs-face-a"
                  x1="3"
                  y1="2"
                  x2="20"
                  y2="22"
                  gradientUnits="userSpaceOnUse"
                >
                  <stop stop-color="var(--accent-hover)" />
                  <stop offset="1" stop-color="var(--accent-active)" />
                </linearGradient>
                <linearGradient
                  id="obs-face-b"
                  x1="11"
                  y1="2"
                  x2="22"
                  y2="20"
                  gradientUnits="userSpaceOnUse"
                >
                  <stop stop-color="#b79bff" />
                  <stop offset="1" stop-color="var(--accent)" />
                </linearGradient>
              </defs>
              <path
                d="M9.4 2.3 4 8.1l3.1 10.2L11 22l-1.3-9.5L9.4 2.3Z"
                fill="url(#obs-face-a)"
              />
              <path
                d="M9.4 2.3 9.7 12.5 11 22l5.6-4.2L20 8.4 14.6 2.6 9.4 2.3Z"
                fill="url(#obs-face-b)"
              />
              <path
                d="M9.7 12.5 9.4 2.3l5.2.3-.7 8 .9 1.9-5.1.0Z"
                fill="#ffffff"
                fill-opacity="0.16"
              />
            </svg>
          </span>
          <div class="obsidian-copy">
            <h3>Works with Obsidian</h3>
            <p class="text-secondary obsidian-sub">
              Murmur saves each meeting as a Markdown note in your Obsidian
              vault. Obsidian is <strong>optional</strong> — you can read every
              recording, AI summary and transcript right here in Murmur (the
              Meetings tab, with audio playback). Want the full vault
              experience?
            </p>
          </div>
        </div>

        <div class="obsidian-get">
          <span class="obsidian-get-label text-muted"
            >Get Obsidian — it's free</span
          >
          <span class="obsidian-url-row">
            <span class="obsidian-url" role="text">{{ obsidianUrl }}</span>
            <button
              type="button"
              class="btn obsidian-copy-btn"
              [class.is-copied]="urlCopied()"
              (click)="copyObsidianUrl()"
              [attr.aria-label]="
                urlCopied() ? 'Copied' : 'Copy ' + obsidianUrl + ' to clipboard'
              "
            >
              @if (urlCopied()) {
                <svg
                  class="obsidian-copy-icon"
                  viewBox="0 0 16 16"
                  width="14"
                  height="14"
                  aria-hidden="true"
                >
                  <path
                    d="M3 8.5 6.2 12 13 4.5"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.8"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                  />
                </svg>
                Copied
              } @else {
                <svg
                  class="obsidian-copy-icon"
                  viewBox="0 0 16 16"
                  width="14"
                  height="14"
                  aria-hidden="true"
                >
                  <rect
                    x="5.5"
                    y="5.5"
                    width="8"
                    height="8"
                    rx="2"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.5"
                  />
                  <path
                    d="M10.5 5.5V4a2 2 0 0 0-2-2H4a2 2 0 0 0-2 2v4.5a2 2 0 0 0 2 2h1.5"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.5"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                  />
                </svg>
                Copy
              }
            </button>
          </span>
        </div>
      </div>

      <!-- Provider configuration -->
      <div class="card">
        <fieldset>
          <legend>Provider configuration</legend>

          <label class="field">
            <span class="field-label">Anthropic model</span>
            <input formControlName="anthropicModel" />
          </label>

          <label class="field">
            <span class="field-label">Ollama base URL</span>
            <input formControlName="ollamaBaseUrl" />
          </label>

          <label class="field">
            <span class="field-label">Ollama model</span>
            <input formControlName="ollamaModel" />
          </label>

          <label class="field">
            <span class="field-label">Claude binary</span>
            <input formControlName="claudeBinary" />
          </label>
        </fieldset>
      </div>

      <!-- Microphone input device -->
      <div class="card">
        <label class="field">
          <span class="field-label">Microphone</span>
          <select formControlName="inputDevice">
            <option value="">System default</option>
            @for (dev of inputDevices(); track dev.name) {
              <option [value]="dev.name">
                {{ dev.name }}{{ dev.isDefault ? " (default)" : "" }}
              </option>
            }
          </select>
          <span class="field-help text-muted">
            Which microphone to record. “System default” follows your macOS input
            selection; a chosen device falls back to the default if it’s unplugged.
          </span>
        </label>
      </div>

      <!-- Capture system audio — toggle row -->
      <div class="card">
        <label class="toggle-row">
          <span class="toggle-copy">
            <span class="toggle-title">Capture system audio</span>
            <span class="text-secondary toggle-sub">
              Records the other side of the call — needs the Screen Recording
              permission on first use.
            </span>
          </span>
          <input type="checkbox" formControlName="captureSystemAudio" />
        </label>
      </div>

      <!-- Smart transcription (VAD) — toggle row -->
      <div class="card">
        <label class="toggle-row">
          <span class="toggle-copy">
            <span class="toggle-title">Smart speech detection</span>
            <span class="text-secondary toggle-sub">
              Skips silence and resets context between pauses for cleaner, faster
              transcripts (voice-activity detection). Recommended.
            </span>
          </span>
          <input type="checkbox" formControlName="vadEnabled" />
        </label>
      </div>

      <!-- High-fidelity masters — toggle row -->
      <div class="card">
        <label class="toggle-row">
          <span class="toggle-copy">
            <span class="toggle-title">Keep high-fidelity masters</span>
            <span class="text-secondary toggle-sub">
              Archive faithful per-stream float32 recordings (mic + system)
              alongside the standard mix. Best quality; roughly doubles audio disk
              use per meeting.
            </span>
          </span>
          <input type="checkbox" formControlName="keepHiresMasters" />
        </label>
      </div>

      <!-- Speaker diarization — toggle row -->
      <div class="card">
        <label class="toggle-row">
          <span class="toggle-copy">
            <span class="toggle-title">Identify remote speakers</span>
            <span class="text-secondary toggle-sub">
              Label individual people on the other side of the call (Speaker
              1/2/3) instead of one “Others”. Needs system-audio capture;
              downloads ~40 MB of models on first use.
            </span>
          </span>
          <input type="checkbox" formControlName="diarizeOthers" />
        </label>
      </div>

      <!-- Echo cancellation (experimental) — toggle row -->
      <div class="card">
        <label class="toggle-row">
          <span class="toggle-copy">
            <span class="toggle-title">Cancel speaker echo (experimental)</span>
            <span class="text-secondary toggle-sub">
              When recording without headphones, apply system echo cancellation to
              the microphone used for transcription. Experimental — headphones are
              still the most reliable fix.
            </span>
          </span>
          <input type="checkbox" formControlName="aecEnabled" />
        </label>
      </div>

      <!-- Voice trigger — toggle row -->
      <div class="card">
        <label class="toggle-row">
          <span class="toggle-copy">
            <span class="toggle-title">Voice trigger</span>
            <span class="text-secondary toggle-sub">
              Start recording hands-free when you say “start recording”. Listens
              with your Whisper model while idle.
            </span>
          </span>
          <input type="checkbox" formControlName="voiceTrigger" />
        </label>
      </div>

      <!-- Anthropic API key -->
      <div class="card">
        <fieldset>
          <legend>Anthropic API key</legend>
          <div class="key-status">
            <span class="text-secondary">Status</span>
            @if (hasKey()) {
              <span class="pill is-success">
                <span class="pill-dot"></span>
                Set
              </span>
            } @else {
              <span class="pill">
                <span class="pill-dot"></span>
                Not set
              </span>
            }
          </div>
          <span class="row">
            <input
              type="password"
              [formControl]="keyControl"
              placeholder="sk-ant-…"
            />
            <button type="button" class="btn" (click)="saveKey()">
              Save key
            </button>
          </span>
        </fieldset>
      </div>

      <!-- Actions -->
      <div class="actions">
        <button type="button" class="btn btn-primary" (click)="save()">
          Save settings
        </button>
        <button type="button" class="btn" (click)="refreshProviders()">
          Check providers
        </button>
        @if (saved()) {
          <span class="pill is-success saved-pill">
            <span class="pill-dot"></span>
            Saved
          </span>
        }
        <button
          type="button"
          class="btn btn-ghost rerun-setup"
          (click)="rerunOnboarding()"
        >
          Run setup again
        </button>
      </div>

      <!-- Provider availability -->
      <div class="card">
        <h3>Provider availability</h3>
        <ul class="provider-list">
          @for (p of providers(); track p.id) {
            <li class="provider-row">
              <span class="provider-name">{{ p.id }}</span>
              @if (p.available) {
                <span class="pill is-success">
                  <span class="pill-dot"></span>
                  Available
                </span>
              } @else {
                <span class="provider-unavailable">
                  <span class="pill is-danger">
                    <span class="pill-dot"></span>
                    Unavailable
                  </span>
                  @if (p.reason) {
                    <span class="text-muted provider-reason">{{
                      p.reason
                    }}</span>
                  }
                </span>
              }
            </li>
          }
        </ul>
      </div>

      <!-- Privacy & integrations: redaction firewall + local MCP server -->
      <div class="card privacy-card">
        <div class="privacy-head">
          <span class="privacy-mark" aria-hidden="true">
            <svg
              viewBox="0 0 24 24"
              width="26"
              height="26"
              fill="none"
              role="img"
              aria-label="Privacy"
            >
              <path
                d="M12 2.5 4.5 5.5v5.2c0 4.6 3.1 8.1 7.5 9.3 4.4-1.2 7.5-4.7 7.5-9.3V5.5L12 2.5Z"
                fill="var(--accent-soft)"
                stroke="var(--accent-hover)"
                stroke-width="1.3"
                stroke-linejoin="round"
              />
              <path
                d="M9.4 11.8 11.2 13.6 14.8 9.8"
                fill="none"
                stroke="var(--accent-hover)"
                stroke-width="1.6"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
          </span>
          <div class="privacy-copy">
            <h3>Privacy &amp; integrations</h3>
            <p class="text-secondary privacy-sub">
              How Murmur protects your text and connects to your tools.
            </p>
          </div>
        </div>

        <!-- (1) Redaction firewall -->
        <div class="privacy-section">
          <span class="privacy-section-label text-muted"
            >Redaction firewall</span
          >
          <p class="text-secondary privacy-note">
            Emails, card numbers and phone numbers are automatically scrubbed
            before any text is sent to a cloud model — that's both the Anthropic
            API and Claude Code (the <code>claude</code> CLI uploads your
            transcript to Anthropic too), then restored in your notes. Only
            Ollama runs fully on-device and sends nothing to the cloud.
          </p>
          <p class="text-secondary privacy-note">
            Heads up: names are <strong>not</strong> redacted — the firewall is
            regex-only (emails, cards, phone numbers), so people's names can
            leave your device alongside the transcript when you use a cloud
            provider.
          </p>
        </div>

        <!-- (1b) Cloud processing consent (E10) -->
        <div class="privacy-section">
          <span class="privacy-section-label text-muted">Cloud processing</span>
          <p class="text-secondary privacy-note">
            Claude Code and the Anthropic API send your (redacted) transcript to
            Anthropic's cloud to write each summary — your data leaves this Mac.
            Ollama stays fully on-device. Until you allow this once, cloud
            summaries are turned off and won't run.
          </p>
          @if (cloudConsented()) {
            <span class="pill is-success cloud-consent-pill">
              <span class="pill-dot"></span>
              Cloud processing allowed
            </span>
          } @else {
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
                  Allow cloud processing
                }
              </button>
              <span class="text-muted cloud-consent-hint">
                One-time. You can keep using Ollama with no cloud at all.
              </span>
            </div>
          }
          @if (consentError(); as cerr) {
            <p class="text-danger privacy-note">{{ cerr }}</p>
          }
        </div>

        <!-- (2) Locked folders (honest encryption boundary) -->
        <div class="privacy-section">
          <span class="privacy-section-label text-muted">Locked folders</span>
          <p class="text-secondary privacy-note">
            Locked folders are encrypted and pulled out of your Obsidian vault;
            open notes remain plaintext .md files Obsidian can read.
          </p>
        </div>

        <!-- (3) Local MCP server -->
        <div class="privacy-section">
          <span class="privacy-section-label text-muted">Local MCP server</span>
          <p class="text-secondary privacy-note">
            Murmur runs a localhost MCP server that exposes your meetings
            (read-only) to Claude Desktop and Claude Code at
            <span class="privacy-inline-url">{{ mcpUrl }}</span
            >.
          </p>

          <div class="mcp-config">
            <div class="mcp-config-head">
              <span class="mcp-config-label text-muted">Config</span>
              <button
                type="button"
                class="btn mcp-copy-btn"
                [class.is-copied]="configCopied()"
                (click)="copyMcpConfig()"
                [attr.aria-label]="
                  configCopied() ? 'Copied' : 'Copy config to clipboard'
                "
              >
                @if (configCopied()) {
                  <svg
                    class="mcp-copy-icon"
                    viewBox="0 0 16 16"
                    width="14"
                    height="14"
                    aria-hidden="true"
                  >
                    <path
                      d="M3 8.5 6.2 12 13 4.5"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.8"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                    />
                  </svg>
                  Copied
                } @else {
                  <svg
                    class="mcp-copy-icon"
                    viewBox="0 0 16 16"
                    width="14"
                    height="14"
                    aria-hidden="true"
                  >
                    <rect
                      x="5.5"
                      y="5.5"
                      width="8"
                      height="8"
                      rx="2"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.5"
                    />
                    <path
                      d="M10.5 5.5V4a2 2 0 0 0-2-2H4a2 2 0 0 0-2 2v4.5a2 2 0 0 0 2 2h1.5"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.5"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                    />
                  </svg>
                  Copy config
                }
              </button>
            </div>
            <pre class="mcp-config-block" role="text">{{ mcpConfig }}</pre>
          </div>

          <span class="mcp-hint text-muted">
            Add this to your Claude Desktop config, then restart Claude Desktop.
          </span>
        </div>
      </div>
    </section>
  `,
  styles: [
    `
      .settings {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
        animation: rise 380ms var(--transition) both;
      }

      /* --- Banner icon (matches the record screen) --- */
      .banner-icon {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 24px;
        height: 24px;
        min-width: 24px;
        border-radius: 50%;
        background: rgba(255, 255, 255, 0.08);
        font-weight: 700;
        font-size: 0.85rem;
        line-height: 1;
      }

      /* --- Whisper model status card --- */
      .model-head {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-4);
        flex-wrap: wrap;
      }
      .model-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .model-copy h3 {
        margin: 0;
      }
      .model-sub {
        margin: 0;
        font-size: 0.875rem;
      }
      .model-error {
        margin: var(--space-3) 0 0;
        font-size: 0.85rem;
      }
      .model-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .model-grid {
        display: grid;
        grid-template-columns: 1fr 1fr;
        gap: var(--space-4);
      }
      @media (max-width: 520px) {
        .model-grid {
          grid-template-columns: 1fr;
        }
      }
      .model-status-row {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
        min-height: 40px;
      }
      .model-note {
        font-size: 0.85rem;
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

      /* --- Notes card (summary style + auto-organize) --- */
      .notes-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .notes-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .notes-copy h3 {
        margin: 0;
      }
      .notes-sub {
        margin: 0;
        font-size: 0.875rem;
      }
      /* One-line helper that tracks the selected summary style. */
      .field-help {
        font-size: 0.8125rem;
        line-height: 1.5;
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

      /* --- Works-with-Obsidian card --- */
      .obsidian-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
      }
      .obsidian-head {
        display: flex;
        align-items: flex-start;
        gap: var(--space-4);
      }
      .obsidian-mark {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 52px;
        height: 52px;
        min-width: 52px;
        border-radius: var(--radius-md);
        background: var(--accent-soft);
        border: 1px solid var(--glass-border);
        box-shadow: var(--glass-highlight);
        /* Gentle staggered settle, in keeping with the global entrance language. */
        animation: rise 460ms var(--ease-spring) both;
        animation-delay: 80ms;
        transition:
          transform var(--transition),
          box-shadow var(--transition),
          border-color var(--transition);
      }
      .obsidian-card:hover .obsidian-mark {
        transform: translateY(-1px) rotate(-3deg);
        border-color: var(--border-strong);
        box-shadow: var(--shadow-accent), var(--glass-highlight);
      }
      .obsidian-mark svg {
        display: block;
        filter: drop-shadow(0 2px 6px rgba(110, 118, 255, 0.45));
      }
      .obsidian-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        min-width: 0;
      }
      .obsidian-copy h3 {
        margin: 0;
      }
      .obsidian-sub {
        margin: 0;
        font-size: 0.9rem;
        line-height: 1.55;
      }
      .obsidian-sub strong {
        color: var(--text-primary);
        font-weight: 600;
      }

      /* The free-Obsidian call-out: a quiet inset well with copyable URL + button. */
      .obsidian-get {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        padding: var(--space-4);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
      }
      .obsidian-get-label {
        font-size: 0.8125rem;
        font-weight: 550;
        letter-spacing: 0.01em;
        text-transform: uppercase;
      }
      .obsidian-url-row {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex-wrap: wrap;
      }
      .obsidian-url {
        flex: 1 1 auto;
        min-width: 0;
        padding: 0 var(--space-3);
        height: 40px;
        display: inline-flex;
        align-items: center;
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-md);
        background: var(--surface-raised);
        color: var(--text-primary);
        font-family: var(--font-mono);
        font-size: 0.9375rem;
        letter-spacing: -0.01em;
        /* Selectable text — the user can also just copy it by hand. */
        user-select: text;
        -webkit-user-select: text;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .obsidian-copy-btn {
        flex: none;
      }
      .obsidian-copy-btn.is-copied {
        color: var(--success);
        border-color: rgba(74, 222, 128, 0.4);
        background: var(--success-soft);
      }
      .obsidian-copy-icon {
        flex: none;
      }

      /* --- Cards stack their fieldset flush (card already provides padding) --- */
      .card fieldset {
        border: none;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .card fieldset legend {
        padding: 0;
        margin-bottom: var(--space-4);
        float: left;
        width: 100%;
        font-size: 0.8125rem;
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

      .row {
        display: flex;
        gap: var(--space-2);
      }
      .row input {
        flex: 1;
      }
      .row .btn {
        flex: none;
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

      /* --- API-key status --- */
      .key-status {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        margin-bottom: var(--space-2);
      }

      /* --- Actions --- */
      .actions {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
      }
      .saved-pill {
        margin-left: var(--space-1);
      }
      /* Quiet escape hatch to re-run the first-run wizard — pushed to the edge. */
      .rerun-setup {
        margin-left: auto;
        font-size: 0.875rem;
        color: var(--text-muted);
      }

      /* --- Provider availability list --- */
      .provider-list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
      }
      .provider-row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
        padding: var(--space-3) 0;
        border-bottom: 1px solid var(--border-subtle);
      }
      .provider-row:last-child {
        border-bottom: none;
      }
      .provider-name {
        color: var(--text-primary);
        font-weight: 550;
        font-family: var(--font-mono);
        font-size: 0.875rem;
      }
      .provider-unavailable {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        flex-wrap: wrap;
        justify-content: flex-end;
      }
      .provider-reason {
        font-size: 0.8125rem;
      }

      /* --- Connectors card (web search — NEW EGRESS) --- */
      .connectors-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .connector-egress {
        margin: 0;
        font-size: 0.875rem;
        line-height: 1.55;
      }
      .connector-fieldset {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .connector-consent {
        margin-top: var(--space-1);
      }

      /* --- Privacy & integrations card --- */
      .privacy-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
      }
      .privacy-head {
        display: flex;
        align-items: flex-start;
        gap: var(--space-4);
      }
      .privacy-mark {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 48px;
        height: 48px;
        min-width: 48px;
        border-radius: var(--radius-md);
        background: var(--accent-soft);
        border: 1px solid var(--glass-border);
        box-shadow: var(--glass-highlight);
      }
      .privacy-mark svg {
        display: block;
      }
      .privacy-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        min-width: 0;
      }
      .privacy-copy h3 {
        margin: 0;
      }
      .privacy-sub {
        margin: 0;
        font-size: 0.9rem;
        line-height: 1.55;
      }

      /* Each subsection: a small uppercase label over its explanatory note. */
      .privacy-section {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .privacy-section-label {
        font-size: 0.8125rem;
        font-weight: 550;
        letter-spacing: 0.01em;
        text-transform: uppercase;
      }
      .privacy-note {
        margin: 0;
        font-size: 0.9rem;
        line-height: 1.55;
      }
      .privacy-inline-url {
        font-family: var(--font-mono);
        font-size: 0.85em;
        color: var(--text-primary);
        letter-spacing: -0.01em;
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

      /* Copyable JSON well — a quiet inset block with its own copy button. */
      .mcp-config {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        padding: var(--space-4);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
      }
      .mcp-config-head {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
      }
      .mcp-config-label {
        font-size: 0.8125rem;
        font-weight: 550;
        letter-spacing: 0.01em;
        text-transform: uppercase;
      }
      .mcp-config-block {
        margin: 0;
        padding: var(--space-3);
        border-radius: var(--radius-md);
        background: var(--surface-raised);
        border: 1px solid var(--glass-border);
        color: var(--text-primary);
        font-family: var(--font-mono);
        font-size: 0.8125rem;
        line-height: 1.55;
        letter-spacing: -0.01em;
        /* Exact-as-written JSON; selectable so it can also be copied by hand. */
        white-space: pre;
        overflow-x: auto;
        user-select: text;
        -webkit-user-select: text;
      }
      .mcp-copy-btn {
        flex: none;
        height: 32px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
      }
      .mcp-copy-btn.is-copied {
        color: var(--success);
        border-color: rgba(74, 222, 128, 0.4);
        background: var(--success-soft);
      }
      .mcp-copy-icon {
        flex: none;
      }
      .mcp-hint {
        font-size: 0.8125rem;
        line-height: 1.5;
      }
    `,
  ],
})
export class SettingsComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly fb = inject(FormBuilder);
  private readonly router = inject(Router);
  private readonly destroyRef = inject(DestroyRef);

  /** Tracked so we can cancel the pending "Copied" reset on destroy (no leaks). */
  private copyResetTimer: ReturnType<typeof setTimeout> | null = null;

  /** Same as copyResetTimer, but for the MCP "Copied" flash — cancelled on destroy. */
  private mcpCopyResetTimer: ReturnType<typeof setTimeout> | null = null;

  /**
   * Built eagerly with defaults so the panel always renders (no "stuck on loading"),
   * then `patchValue`d from the loaded config in ngOnInit. SF-6: reactive forms.
   */
  readonly form = this.fb.nonNullable.group({
    providerId: "claude_code",
    vaultPath: "",
    vaultSubfolder: "",
    whisperModelPath: "",
    language: "",
    anthropicModel: "claude-opus-4-8",
    // Brain/AI model + reasoning-effort overrides ("" = provider default). Effort is
    // honored only by the anthropic provider; the picker is gated on providerId below.
    providerModel: "",
    providerEffort: "",
    ollamaBaseUrl: "http://localhost:11434",
    ollamaModel: "llama3.1",
    claudeBinary: "claude",
    inputDevice: "",
    captureSystemAudio: false,
    vadEnabled: true,
    keepHiresMasters: false,
    diarizeOthers: false,
    aecEnabled: false,
    modelSize: "large-v3",
    voiceTrigger: false,
    noteStyle: "standard",
    autoOrganize: false,
    noteLanguage: "auto",
    // Phase H — brain / in-meeting voice assistant.
    brainBackend: "cloud" as BrainBackend,
    realtimeReactions: false,
    /** Custom GGUF model path (or registry id). Empty → null on save. */
    brainModelId: "",
    // brain2 RAG — semantic-search master flag (round-tripped on save).
    semanticSearchEnabled: false,
    // brain2 connectors — web-search master toggle (NEW EGRESS; round-tripped).
    webSearchEnabled: false,
  });
  readonly keyControl = new FormControl("", { nonNullable: true });
  /** BYO Brave Search API key input (web-search connector). Cleared after save. */
  readonly webKeyControl = new FormControl("", { nonNullable: true });

  readonly providers = signal<ProviderStatus[]>([]);
  /** Available mic input devices for the picker (loaded best-effort in ngOnInit). */
  readonly inputDevices = signal<InputDeviceInfo[]>([]);
  readonly hasKey = signal(false);
  readonly saved = signal(false);
  readonly loadError = signal<string | null>(null);

  /** The Obsidian homepage — shown as copyable text (no in-webview navigation). */
  readonly obsidianUrl = "obsidian.md";

  /** Flips true for ~1.6s after copying the Obsidian URL — drives the button's confirmed state. */
  readonly urlCopied = signal(false);

  /** The localhost MCP server address — shown inline and embedded in the config. */
  readonly mcpUrl = "http://127.0.0.1:8765";

  /** Exact JSON to drop into the Claude Desktop config — copied verbatim. */
  readonly mcpConfig = `{
  "mcpServers": {
    "murmur": {
      "url": "${this.mcpUrl}"
    }
  }
}`;

  /** Flips true for ~1.6s after copying the MCP config — drives the button's confirmed state. */
  readonly configCopied = signal(false);

  /**
   * Real Whisper-model presence (same UX as the record screen).
   * `null` = not yet checked, `true`/`false` = detected via ipc.modelPresent().
   */
  readonly modelPresent = signal<boolean | null>(null);

  /** True while a download is in-flight — disables the download button. */
  readonly downloadingModel = signal(false);

  /** Surfaced if ipc.downloadModel() rejects. */
  readonly modelDownloadError = signal<string | null>(null);

  /** Approx download size for the selected quality (shown on the Download button). */
  readonly downloadHint = signal("~3 GB");

  /** Preserved from the loaded config (not a form field) so saving never un-onboards. */
  private loadedOnboarded = true;

  /**
   * Stage E security flags — preserved from the loaded config (not form-edited)
   * so save() round-trips them instead of letting the backend default them off.
   */
  private loadedMcpRequireToken = true;
  private loadedLockRequireBiometric = true;
  private loadedRelockOnScreenshare = true;

  /** Cloud-egress consent state — drives the "Cloud processing" section; round-tripped on save. */
  readonly cloudConsented = signal(false);
  /** True while the one-time consent command is in flight. */
  readonly consenting = signal(false);
  /** Surfaced if granting consent rejects. */
  readonly consentError = signal<string | null>(null);

  // ── brain2 connectors — web search (NEW EGRESS) ────────────────────────

  /** Web-search egress consent state — drives the "Allow web search" section; round-tripped on save. */
  readonly webConsented = signal(false);
  /** True while the one-time web-search consent command is in flight. */
  readonly webConsenting = signal(false);
  /** Surfaced if granting web-search consent rejects. */
  readonly webConsentError = signal<string | null>(null);
  /** Whether a Brave Search API key is stored (has-key check; never the value). */
  readonly hasWebKey = signal(false);
  /** True while the BYO key is being saved. */
  readonly savingWebKey = signal(false);
  /** Surfaced if storing the web-search key rejects. */
  readonly webKeyError = signal<string | null>(null);

  // ── Phase H — brain (AI assistant) model registry ──────────────────────

  /** The selectable local brain models (from list_brain_models). */
  readonly brainModels = signal<BrainModelDto[]>([]);
  /** True while the model list is loading (best-effort). */
  readonly brainModelsLoading = signal(false);
  /** Surfaced if loading / selecting / downloading a brain model rejects. */
  readonly brainError = signal<string | null>(null);
  /** Model id currently downloading, or null. Drives the per-row progress UI. */
  readonly brainDownloadingId = signal<string | null>(null);
  /** 0..1 download progress for the in-flight model (best-effort from events). */
  readonly brainDownloadFrac = signal(0);
  /** Whole-percent label for the in-flight brain-model download. */
  readonly brainPct = computed(
    () => Math.round(this.brainDownloadFrac() * 100) + "%",
  );
  /** Release handle for the EVENT_BRAIN_DOWNLOAD subscription. */
  private unlistenBrainDownload: UnlistenFn | null = null;

  // ── brain2 RAG — semantic search (embedding model + reindex) ────────────

  /**
   * Whether the on-device embedding model is present.
   * `null` = not yet checked, `true`/`false` = detected via ipc.embedModelPresent().
   */
  readonly embedModelPresent = signal<boolean | null>(null);
  /** True while the embedding model is downloading — disables its button. */
  readonly downloadingEmbedModel = signal(false);
  /** 0..1 download progress for the in-flight embed-model download. */
  readonly embedDownloadFrac = signal(0);
  /** Whole-percent label for the in-flight embed-model download. */
  readonly embedPct = computed(
    () => Math.round(this.embedDownloadFrac() * 100) + "%",
  );
  /** Surfaced if ipc.downloadEmbedModel() rejects. */
  readonly embedDownloadError = signal<string | null>(null);

  /** True while a reindex backfill is running — disables the button + shows progress. */
  readonly reindexing = signal(false);
  /** 0..1 progress for the in-flight reindex backfill. */
  readonly reindexFrac = signal(0);
  /** Whole-percent label for the in-flight reindex backfill. */
  readonly reindexPct = computed(
    () => Math.round(this.reindexFrac() * 100) + "%",
  );
  /** Last reindex outcome — drives the "model_missing" nudge / "indexed" confirmation. */
  readonly reindexResult = signal<ReindexResult | null>(null);
  /** Surfaced if ipc.reindexEmbeddings() rejects. */
  readonly reindexError = signal<string | null>(null);

  /** Release handles for the embed-download + reindex event streams. */
  private unlistenEmbedDownload: UnlistenFn | null = null;
  private unlistenReindex: UnlistenFn | null = null;

  async ngOnInit(): Promise<void> {
    try {
      const cfg = await this.ipc.getConfig();
      this.loadedOnboarded = cfg.onboarded ?? true;
      // Stage E security flags are not form-edited here — snapshot them so save()
      // round-trips them instead of letting the backend's serde defaults clobber
      // them (mcpRequireToken / cloudEgressConsented would otherwise reset to false).
      this.loadedMcpRequireToken = cfg.mcpRequireToken ?? true;
      this.loadedLockRequireBiometric = cfg.lockRequireBiometric ?? true;
      this.loadedRelockOnScreenshare = cfg.relockOnScreenshare ?? true;
      this.cloudConsented.set(cfg.cloudEgressConsented ?? false);
      // brain2 connectors — web-search consent is preserve-only (granted only via
      // consent_to_web_search); snapshot it so save() round-trips it unchanged.
      this.webConsented.set(cfg.webSearchConsented ?? false);
      this.form.patchValue({
        providerId: cfg.providerId,
        vaultPath: cfg.vaultPath ?? "",
        vaultSubfolder: cfg.vaultSubfolder ?? "",
        whisperModelPath: cfg.whisperModelPath ?? "",
        language: cfg.language ?? "",
        anthropicModel: cfg.anthropicModel,
        providerModel: cfg.providerModel ?? "",
        providerEffort: cfg.providerEffort ?? "",
        ollamaBaseUrl: cfg.ollamaBaseUrl,
        ollamaModel: cfg.ollamaModel,
        claudeBinary: cfg.claudeBinary,
        inputDevice: cfg.inputDevice ?? "",
        captureSystemAudio: cfg.captureSystemAudio ?? false,
        vadEnabled: cfg.vadEnabled ?? true,
        keepHiresMasters: cfg.keepHiresMasters ?? false,
        diarizeOthers: cfg.diarizeOthers ?? false,
        aecEnabled: cfg.aecEnabled ?? false,
        modelSize: cfg.modelSize ?? "large-v3",
        voiceTrigger: cfg.voiceTrigger ?? false,
        noteStyle: cfg.noteStyle ?? "standard",
        autoOrganize: cfg.autoOrganize ?? false,
        noteLanguage: cfg.noteLanguage ?? "auto",
        brainBackend: cfg.brainBackend ?? "cloud",
        realtimeReactions: cfg.realtimeReactions ?? false,
        brainModelId: cfg.brainModelId ?? "",
        semanticSearchEnabled: cfg.semanticSearchEnabled ?? false,
        webSearchEnabled: cfg.webSearchEnabled ?? false,
      });
      this.updateDownloadHint();
      this.inputDevices.set(await this.ipc.listInputDevices().catch(() => []));
      this.hasKey.set(await this.ipc.hasAnthropicKey());
      this.hasWebKey.set(await this.ipc.hasWebSearchKey().catch(() => false));
      this.modelPresent.set(await this.ipc.modelPresent());
      await this.refreshProviders();
      // Phase H — brain model registry + download-progress stream (best-effort).
      await this.subscribeBrainDownload();
      await this.refreshBrainModels();
      // brain2 RAG — embedding-model presence + reindex/download progress streams.
      await this.subscribeSemanticStreams();
      this.embedModelPresent.set(
        await this.ipc.embedModelPresent().catch(() => false),
      );
    } catch (e) {
      this.loadError.set(String(e));
    }
  }

  /**
   * Subscribe ONCE to the brain-download progress stream and store the unlisten
   * so DestroyRef can release it (no leaked listener). Best-effort: a missing
   * backend command just leaves the progress bar inert.
   */
  private async subscribeBrainDownload(): Promise<void> {
    try {
      this.unlistenBrainDownload = await this.ipc.onBrainDownload((p) => {
        // The backend emits one download at a time and the component already
        // tracks which model it started (brainDownloadingId), so every progress
        // event applies to it. (Download errors surface via the command promise.)
        if (this.brainDownloadingId() === null) return;
        if (p.total && p.total > 0) {
          this.brainDownloadFrac.set(Math.min(1, p.downloaded / p.total));
        }
        if (p.done) {
          this.brainDownloadingId.set(null);
          void this.refreshBrainModels();
        }
      });
      this.destroyRef.onDestroy(() => this.unlistenBrainDownload?.());
    } catch {
      // No brain-download stream available — progress stays inert; downloads
      // still resolve via the command promise.
    }
  }

  /** Reload the brain model registry (downloaded / fits-RAM / selected state). */
  async refreshBrainModels(): Promise<void> {
    this.brainModelsLoading.set(true);
    this.brainError.set(null);
    try {
      this.brainModels.set(await this.ipc.listBrainModels());
    } catch (e) {
      this.brainError.set(String(e));
    } finally {
      this.brainModelsLoading.set(false);
    }
  }

  /** Make a registry model the active local brain model, then refresh the list. */
  async useBrainModel(id: string): Promise<void> {
    this.brainError.set(null);
    try {
      await this.ipc.selectBrainModel(id);
      this.form.patchValue({ brainModelId: id });
      await this.refreshBrainModels();
    } catch (e) {
      this.brainError.set(String(e));
    }
  }

  /**
   * Download a registry model. The promise resolves on completion; live
   * progress (when available) rides the EVENT_BRAIN_DOWNLOAD stream.
   */
  async downloadBrainModel(id: string): Promise<void> {
    this.brainError.set(null);
    this.brainDownloadFrac.set(0);
    this.brainDownloadingId.set(id);
    try {
      await this.ipc.downloadBrainModel(id);
      await this.refreshBrainModels();
    } catch (e) {
      this.brainError.set(String(e));
    } finally {
      this.brainDownloadingId.set(null);
    }
  }

  // ── brain2 RAG — semantic search (embedding model + reindex backfill) ───

  /**
   * Subscribe ONCE to the embed-download + reindex progress streams and store the
   * unlisten handles so DestroyRef can release them (no leaked listeners).
   * Best-effort: a missing backend stream just leaves the relevant bar inert.
   */
  private async subscribeSemanticStreams(): Promise<void> {
    try {
      this.unlistenEmbedDownload = await this.ipc.onEmbedDownload((p) => {
        // Per-file progress: blend the completed files + the current file's fraction
        // across the whole set so the single bar advances smoothly.
        if (p.fileCount > 0) {
          const cur = p.total && p.total > 0 ? p.downloaded / p.total : 0;
          this.embedDownloadFrac.set(
            Math.min(1, (p.fileIndex + cur) / p.fileCount),
          );
        }
        if (p.done) this.embedDownloadFrac.set(1);
      });
      this.unlistenReindex = await this.ipc.onReindex((p) => {
        if (p.total > 0) {
          this.reindexFrac.set(Math.min(1, p.done / p.total));
        }
      });
      this.destroyRef.onDestroy(() => {
        this.unlistenEmbedDownload?.();
        this.unlistenReindex?.();
      });
    } catch {
      // No stream available — progress bars stay inert; commands still resolve.
    }
  }

  /** Download the on-device embedding model, then re-check presence. */
  async downloadEmbedModel(): Promise<void> {
    this.embedDownloadError.set(null);
    this.embedDownloadFrac.set(0);
    this.downloadingEmbedModel.set(true);
    try {
      await this.ipc.downloadEmbedModel();
      this.embedModelPresent.set(await this.ipc.embedModelPresent());
    } catch (e) {
      this.embedDownloadError.set(String(e));
    } finally {
      this.downloadingEmbedModel.set(false);
    }
  }

  /**
   * Backfill the semantic vector index over all visible meetings. A
   * `"model_missing"` result means the e5 model isn't installed yet — surfaced as
   * a nudge to download it first (no indexing was attempted).
   */
  async reindexEmbeddings(): Promise<void> {
    this.reindexError.set(null);
    this.reindexResult.set(null);
    this.reindexFrac.set(0);
    this.reindexing.set(true);
    try {
      const res = await this.ipc.reindexEmbeddings();
      this.reindexResult.set(res);
      // The model could have been (un)installed between the presence probe and now.
      if (res.status === "model_missing") this.embedModelPresent.set(false);
    } catch (e) {
      this.reindexError.set(String(e));
    } finally {
      this.reindexing.set(false);
    }
  }

  async pickVault(): Promise<void> {
    const dir = await open({ directory: true, multiple: false });
    if (typeof dir === "string") this.form.patchValue({ vaultPath: dir });
  }

  async pickModel(): Promise<void> {
    const file = await open({ directory: false, multiple: false });
    if (typeof file === "string")
      this.form.patchValue({ whisperModelPath: file });
  }

  async save(): Promise<void> {
    const v = this.form.getRawValue();
    const cfg: AppConfigDto = {
      providerId: v.providerId,
      vaultPath: v.vaultPath || null,
      vaultSubfolder: v.vaultSubfolder || null,
      whisperModelPath: v.whisperModelPath || null,
      language: v.language || null,
      anthropicModel: v.anthropicModel,
      providerModel: v.providerModel,
      providerEffort: v.providerEffort,
      ollamaBaseUrl: v.ollamaBaseUrl,
      ollamaModel: v.ollamaModel,
      claudeBinary: v.claudeBinary,
      inputDevice: v.inputDevice || null,
      captureSystemAudio: v.captureSystemAudio,
      vadEnabled: v.vadEnabled,
      keepHiresMasters: v.keepHiresMasters,
      diarizeOthers: v.diarizeOthers,
      aecEnabled: v.aecEnabled,
      modelSize: v.modelSize,
      voiceTrigger: v.voiceTrigger,
      onboarded: this.loadedOnboarded,
      noteStyle: v.noteStyle,
      autoOrganize: v.autoOrganize,
      noteLanguage: v.noteLanguage,
      // Phase H — brain / in-meeting voice assistant.
      brainBackend: v.brainBackend,
      realtimeReactions: v.realtimeReactions,
      brainModelId: v.brainModelId || null,
      // brain2 RAG — semantic-search master flag (round-tripped so a save preserves it).
      semanticSearchEnabled: v.semanticSearchEnabled,
      // brain2 connectors — web-search toggle is settable from the form; its consent
      // is PRESERVE-ONLY (granted via allowWebSearch's dedicated command), so a save
      // just carries the current value back instead of letting the backend default it.
      webSearchEnabled: v.webSearchEnabled,
      webSearchConsented: this.webConsented(),
      // Round-trip the Stage E security flags so a settings save never silently
      // resets them. Cloud-egress consent is GRANTED only via the dedicated
      // command (allowCloudProcessing) — here we just carry the current value back.
      mcpRequireToken: this.loadedMcpRequireToken,
      lockRequireBiometric: this.loadedLockRequireBiometric,
      relockOnScreenshare: this.loadedRelockOnScreenshare,
      cloudEgressConsented: this.cloudConsented(),
    };
    try {
      await this.ipc.saveConfig(cfg);
      this.saved.set(true);
    } catch (e) {
      this.loadError.set("Save failed: " + String(e));
    }
  }

  async saveKey(): Promise<void> {
    const key = this.keyControl.value;
    if (!key) return;
    await this.ipc.setAnthropicKey(key);
    this.keyControl.setValue("");
    this.hasKey.set(await this.ipc.hasAnthropicKey());
  }

  /** Re-open the first-run wizard. Existing settings are preserved and prefilled. */
  rerunOnboarding(): void {
    void this.router.navigate(["/onboarding"]);
  }

  async refreshProviders(): Promise<void> {
    this.providers.set(await this.ipc.providerStatuses());
  }

  /**
   * E10 — grant the one-time cloud-egress consent via the dedicated command (an
   * explicit, auditable user act — NOT a side effect of a normal settings save).
   * After it resolves, cloud providers (Claude Code / Anthropic) can summarize, so
   * we re-probe provider availability. There is no FE "revoke": consent is granted
   * once; save() simply carries the current value back so it isn't cleared.
   */
  async allowCloudProcessing(): Promise<void> {
    this.consentError.set(null);
    this.consenting.set(true);
    try {
      await this.ipc.consentToCloudEgress();
      this.cloudConsented.set(true);
      await this.refreshProviders();
    } catch (e) {
      this.consentError.set(String(e));
    } finally {
      this.consenting.set(false);
    }
  }

  /**
   * brain2 connectors — store/replace the BYO Brave Search API key in the
   * Keychain, then re-probe presence so the "Key set ✓" pill flips. The value is
   * cleared from the input after saving (it's never shown back). Mirrors saveKey().
   */
  async saveWebKey(): Promise<void> {
    const key = this.webKeyControl.value;
    if (!key.trim()) return;
    this.webKeyError.set(null);
    this.savingWebKey.set(true);
    try {
      await this.ipc.setWebSearchApiKey(key);
      this.webKeyControl.setValue("");
      this.hasWebKey.set(await this.ipc.hasWebSearchKey());
    } catch (e) {
      this.webKeyError.set(String(e));
    } finally {
      this.savingWebKey.set(false);
    }
  }

  /**
   * brain2 connectors — grant the one-time web-search egress consent via the
   * dedicated command (an explicit, auditable user act — NOT a side effect of a
   * normal settings save). After it resolves the brain may expose the web
   * connector (when web search is enabled AND a key is stored). There is no FE
   * "revoke": save() simply carries the current value back so it isn't cleared.
   * Mirrors allowCloudProcessing().
   */
  async allowWebSearch(): Promise<void> {
    this.webConsentError.set(null);
    this.webConsenting.set(true);
    try {
      await this.ipc.consentToWebSearch();
      this.webConsented.set(true);
    } catch (e) {
      this.webConsentError.set(String(e));
    } finally {
      this.webConsenting.set(false);
    }
  }

  /** Persist the chosen language + quality, then re-check which model is present. */
  async onModelChoiceChange(): Promise<void> {
    this.updateDownloadHint();
    await this.save();
    this.modelPresent.set(await this.ipc.modelPresent());
  }

  private updateDownloadHint(): void {
    const hints: Record<string, string> = {
      tiny: "~75 MB",
      base: "~150 MB",
      small: "~470 MB",
      medium: "~1.5 GB",
      "large-v3-turbo": "~1.6 GB",
      "large-v3": "~3 GB",
    };
    this.downloadHint.set(hints[this.form.getRawValue().modelSize] ?? "");
  }

  /**
   * Copy the Obsidian URL to the clipboard and briefly confirm.
   * No <a href> — opening an external URL would navigate the webview away.
   */
  async copyObsidianUrl(): Promise<void> {
    try {
      await navigator.clipboard.writeText(this.obsidianUrl);
      this.urlCopied.set(true);
      if (this.copyResetTimer) clearTimeout(this.copyResetTimer);
      this.copyResetTimer = setTimeout(() => this.urlCopied.set(false), 1600);
      this.destroyRef.onDestroy(() => {
        if (this.copyResetTimer) clearTimeout(this.copyResetTimer);
      });
    } catch {
      // Clipboard unavailable — the URL stays visible and selectable as a fallback.
    }
  }

  /**
   * Copy the MCP server config JSON to the clipboard and briefly confirm.
   * The <pre> block stays selectable as a fallback if the clipboard is blocked.
   */
  async copyMcpConfig(): Promise<void> {
    try {
      await navigator.clipboard.writeText(this.mcpConfig);
      this.configCopied.set(true);
      if (this.mcpCopyResetTimer) clearTimeout(this.mcpCopyResetTimer);
      this.mcpCopyResetTimer = setTimeout(
        () => this.configCopied.set(false),
        1600,
      );
      this.destroyRef.onDestroy(() => {
        if (this.mcpCopyResetTimer) clearTimeout(this.mcpCopyResetTimer);
      });
    } catch {
      // Clipboard unavailable — the config stays visible and selectable as a fallback.
    }
  }

  /** Download the model for the chosen language + quality, then re-check presence. */
  async downloadModel(): Promise<void> {
    this.modelDownloadError.set(null);
    this.downloadingModel.set(true);
    try {
      await this.save(); // ensure the chosen language + size are persisted first
      await this.ipc.downloadModel();
      this.modelPresent.set(await this.ipc.modelPresent());
    } catch (e) {
      this.modelDownloadError.set(String(e));
    } finally {
      this.downloadingModel.set(false);
    }
  }
}
