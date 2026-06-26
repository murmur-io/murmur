import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  OnInit,
  inject,
  signal,
} from "@angular/core";
import { FormBuilder, FormControl, ReactiveFormsModule } from "@angular/forms";
import { Router } from "@angular/router";
import { open } from "@tauri-apps/plugin-dialog";
import { IpcService } from "../../core/ipc.service";
import type { AppConfigDto, ProviderStatus } from "../../core/models";

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

      <!-- Transcription model: language + quality + auto-download -->
      <div class="card model-card">
        <div class="model-copy">
          <h3>Transcription model</h3>
          <p class="text-secondary model-sub">
            Runs on-device. Pick your language and quality — the matching
            Whisper model downloads automatically.
          </p>
        </div>

        <div class="model-grid">
          <label class="field">
            <span class="field-label">Language</span>
            <select formControlName="language" (change)="onModelChoiceChange()">
              <option value="">Auto-detect</option>
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
          </label>

          <label class="field">
            <span class="field-label">Quality</span>
            <select
              formControlName="modelSize"
              (change)="onModelChoiceChange()"
            >
              <option value="tiny">Tiny — fastest (~75 MB)</option>
              <option value="base">Base (~150 MB)</option>
              <option value="small">Small — recommended (~470 MB)</option>
              <option value="medium">Medium — accurate (~1.5 GB)</option>
              <option value="large-v3">Large — best (~3 GB)</option>
            </select>
          </label>
        </div>

        <div class="model-status-row">
          @if (modelPresent() === true) {
            <span class="pill is-success">
              <span class="pill-dot"></span>
              Model ready
            </span>
          } @else if (modelPresent() === false) {
            <button
              type="button"
              class="btn btn-primary"
              (click)="downloadModel()"
              [disabled]="downloadingModel()"
            >
              @if (downloadingModel()) {
                Downloading…
              } @else {
                Download model ({{ downloadHint() }})
              }
            </button>
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
            before any text is sent to the Anthropic API, then restored in your
            notes. Local providers (Claude Code / Ollama) send nothing to the
            cloud.
          </p>
        </div>

        <!-- (2) Local MCP server -->
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
    ollamaBaseUrl: "http://localhost:11434",
    ollamaModel: "llama3.1",
    claudeBinary: "claude",
    captureSystemAudio: false,
    modelSize: "small",
    voiceTrigger: false,
    noteStyle: "standard",
    autoOrganize: false,
    noteLanguage: "auto",
  });
  readonly keyControl = new FormControl("", { nonNullable: true });

  readonly providers = signal<ProviderStatus[]>([]);
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
  readonly downloadHint = signal("~470 MB");

  /** Preserved from the loaded config (not a form field) so saving never un-onboards. */
  private loadedOnboarded = true;

  async ngOnInit(): Promise<void> {
    try {
      const cfg = await this.ipc.getConfig();
      this.loadedOnboarded = cfg.onboarded ?? true;
      this.form.patchValue({
        providerId: cfg.providerId,
        vaultPath: cfg.vaultPath ?? "",
        vaultSubfolder: cfg.vaultSubfolder ?? "",
        whisperModelPath: cfg.whisperModelPath ?? "",
        language: cfg.language ?? "",
        anthropicModel: cfg.anthropicModel,
        ollamaBaseUrl: cfg.ollamaBaseUrl,
        ollamaModel: cfg.ollamaModel,
        claudeBinary: cfg.claudeBinary,
        captureSystemAudio: cfg.captureSystemAudio ?? false,
        modelSize: cfg.modelSize ?? "small",
        voiceTrigger: cfg.voiceTrigger ?? false,
        noteStyle: cfg.noteStyle ?? "standard",
        autoOrganize: cfg.autoOrganize ?? false,
        noteLanguage: cfg.noteLanguage ?? "auto",
      });
      this.updateDownloadHint();
      this.hasKey.set(await this.ipc.hasAnthropicKey());
      this.modelPresent.set(await this.ipc.modelPresent());
      await this.refreshProviders();
    } catch (e) {
      this.loadError.set(String(e));
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
      ollamaBaseUrl: v.ollamaBaseUrl,
      ollamaModel: v.ollamaModel,
      claudeBinary: v.claudeBinary,
      captureSystemAudio: v.captureSystemAudio,
      modelSize: v.modelSize,
      voiceTrigger: v.voiceTrigger,
      onboarded: this.loadedOnboarded,
      noteStyle: v.noteStyle,
      autoOrganize: v.autoOrganize,
      noteLanguage: v.noteLanguage,
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
