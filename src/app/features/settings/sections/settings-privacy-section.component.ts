import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { SettingsStore } from "../settings.store";

/**
 * Settings → privacy section (Stage-1 split): the `@case ("privacy")` block of the
 * former settings.component.ts monolith, moved VERBATIM. State/actions live in
 * the shell-provided SettingsStore so section switches never drop them.
 */
@Component({
  selector: "app-settings-privacy-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="section-stack">
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
                    Emails, card numbers and phone numbers are automatically
                    scrubbed before any text leaves this Mac — that covers the Anthropic
                    API, Claude Code (the <code>claude</code> CLI uploads your
                    transcript to Anthropic too), an AI Gateway, and a remote Ollama
                    server — then restored in your notes. Only Ollama running on this
                    Mac keeps everything on-device.
                  </p>
                  <p class="text-secondary privacy-note">
                    Names: the pattern firewall alone does not catch them, but an
                    on-device name-redaction layer additionally masks people's names
                    when its local model is present. Without that model, names can
                    leave your device alongside the transcript when you use a cloud
                    provider.
                  </p>
                </div>

                <!-- (1b) Cloud processing consent (E10) -->
                <div class="privacy-section">
                  <span class="privacy-section-label text-muted">Cloud processing</span>
                  <p class="text-secondary privacy-note">
                    Cloud providers — Claude Code, the Anthropic API, an AI Gateway, or
                    a remote Ollama server — send your (redacted) transcript off this
                    Mac to write each summary. Only Ollama running on this Mac stays
                    fully on-device. Until you allow this once, cloud summaries are
                    turned off and won't run.
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
                        One-time. You can keep using a local Ollama with no cloud at all.
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
export class SettingsPrivacySectionComponent {
  private readonly store = inject(SettingsStore);

  readonly cloudConsented = this.store.cloudConsented;
  readonly consenting = this.store.consenting;
  readonly consentError = this.store.consentError;
  readonly configCopied = this.store.configCopied;
  readonly mcpUrl = this.store.mcpUrl;
  readonly mcpConfig = this.store.mcpConfig;

  allowCloudProcessing(): void {
    void this.store.allowCloudProcessing();
  }

  copyMcpConfig(): void {
    void this.store.copyMcpConfig();
  }
}
