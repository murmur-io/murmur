import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  computed,
  inject,
  signal,
} from "@angular/core";
import { Router } from "@angular/router";
import { open } from "@tauri-apps/plugin-dialog";
import { IpcService } from "../../core/ipc.service";
import type { AppConfigDto, ProviderStatus } from "../../core/models";

/** The wizard steps, in order. Drives the dot indicator + progress copy. */
type Step = "welcome" | "model" | "provider" | "vault" | "done";
const STEPS: readonly Step[] = [
  "welcome",
  "model",
  "provider",
  "vault",
  "done",
];

/** Human-readable provider names for the AI-provider step. */
const PROVIDER_LABELS: Record<string, string> = {
  claude_code: "Claude Code",
  anthropic: "Anthropic API",
  ollama: "Ollama",
};

/** Approx download size per Whisper quality (mirrors Settings). */
const SIZE_HINTS: Record<string, string> = {
  tiny: "~75 MB",
  base: "~150 MB",
  small: "~470 MB",
  medium: "~1.5 GB",
  "large-v3": "~3 GB",
};

/**
 * First-run wizard — a full-bleed, focused glassmorphism flow that gets a fresh
 * macOS user from launch to a working recorder in five calm steps:
 * Welcome → Transcription model → AI provider → Vault (optional) → Done.
 *
 * State lives entirely in signals; config is persisted to the backend as the
 * user makes choices (so the model can download for the chosen language/size,
 * and so re-running setup later picks up where they left off). The final step
 * flips `onboarded` and routes to /record.
 */
@Component({
  selector: "app-onboarding",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="ob">
      <div class="ob-stage">
        <!-- Progress: dots + step counter, hidden on the welcome splash. -->
        @if (currentStep() !== "welcome") {
          <div class="ob-progress" role="group" aria-label="Setup progress">
            <div class="dots">
              @for (s of steps; track s; let i = $index) {
                <span
                  class="dot"
                  [class.is-done]="i < stepIndex()"
                  [class.is-active]="i === stepIndex()"
                  aria-hidden="true"
                ></span>
              }
            </div>
            <span class="step-count">
              Step {{ stepIndex() }} of {{ steps.length - 1 }}
            </span>
          </div>
        }

        <!-- One panel renders at a time; @switch keys the entrance animation. -->
        <div class="ob-panel card" [attr.data-step]="currentStep()">
          @switch (currentStep()) {
            <!-- ───────────────────────── WELCOME ───────────────────────── -->
            @case ("welcome") {
              <div class="welcome">
                <span class="orb-brand" aria-hidden="true">
                  <span class="orb-core"></span>
                </span>
                <h1 class="ob-title welcome-title">
                  <span class="brand-dot" aria-hidden="true"></span>
                  Welcome to Murmur
                </h1>
                <p class="ob-lede">
                  On-device meeting notes. Your audio never leaves this Mac.
                </p>
                <button
                  type="button"
                  class="btn btn-primary ob-cta"
                  (click)="next()"
                >
                  Get started
                  <span class="cta-arrow" aria-hidden="true">→</span>
                </button>
                <p class="ob-fineprint text-muted">
                  Takes about a minute · everything runs locally
                </p>
              </div>
            }

            <!-- ────────────────────── TRANSCRIPTION MODEL ───────────────── -->
            @case ("model") {
              <div class="ob-head">
                <span class="ob-eyebrow">On-device transcription</span>
                <h2 class="ob-title">Choose your transcription model</h2>
                <p class="ob-sub text-secondary">
                  Whisper runs entirely on your Mac. Pick a language and quality
                  — we’ll fetch the matching model once.
                </p>
              </div>

              <div class="field-grid">
                <label class="field">
                  <span class="field-label">Language</span>
                  <select
                    [value]="language()"
                    (change)="onLanguage($event)"
                    [disabled]="downloading()"
                  >
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
                    [value]="modelSize()"
                    (change)="onModelSize($event)"
                    [disabled]="downloading()"
                  >
                    <option value="tiny">Tiny — fastest (~75 MB)</option>
                    <option value="base">Base (~150 MB)</option>
                    <option value="small">Small — recommended (~470 MB)</option>
                    <option value="medium">Medium — accurate (~1.5 GB)</option>
                    <option value="large-v3">Large — best (~3 GB)</option>
                  </select>
                </label>
              </div>

              <div class="model-state">
                @if (modelPresent() === true) {
                  <span class="pill is-success">
                    <span class="pill-dot"></span>
                    Model ready
                  </span>
                  <span class="text-muted model-note">
                    Stored on this Mac — used for every recording.
                  </span>
                } @else if (modelPresent() === false) {
                  <button
                    type="button"
                    class="btn btn-primary"
                    (click)="downloadModel()"
                    [disabled]="downloading()"
                  >
                    @if (downloading()) {
                      <span class="spin-ring" aria-hidden="true"></span>
                      Downloading…
                    } @else {
                      <span class="dl-arrow" aria-hidden="true">↓</span>
                      Download model ({{ sizeHint() }})
                    }
                  </button>
                  <span class="text-muted model-note">
                    {{ sizeHint() }}, one time, on-device.
                  </span>
                } @else {
                  <span class="pill">
                    <span class="pill-dot"></span>
                    Checking…
                  </span>
                }
              </div>
              @if (downloadError(); as derr) {
                <p class="ob-error text-danger">{{ derr }}</p>
              }
            }

            <!-- ───────────────────────── AI PROVIDER ────────────────────── -->
            @case ("provider") {
              <div class="ob-head">
                <span class="ob-eyebrow">Summaries</span>
                <h2 class="ob-title">Pick how notes get written</h2>
                <p class="ob-sub text-secondary">
                  After transcription, an AI turns the transcript into a clean
                  summary. You just need one of these working.
                </p>
              </div>

              <div class="providers">
                @for (p of providers(); track p.id) {
                  <button
                    type="button"
                    class="provider"
                    [class.is-selected]="provider() === p.id"
                    [class.is-available]="p.available"
                    (click)="selectProvider(p.id)"
                    [attr.aria-pressed]="provider() === p.id"
                  >
                    <span class="provider-main">
                      <span class="provider-name">{{ labelFor(p.id) }}</span>
                      @if (p.available) {
                        <span class="pill is-success">
                          <span class="pill-dot"></span>
                          Available
                        </span>
                      } @else {
                        <span class="pill is-warning">
                          <span class="pill-dot"></span>
                          Needs setup
                        </span>
                      }
                    </span>
                    @if (!p.available && p.reason) {
                      <span class="provider-reason text-muted">{{
                        p.reason
                      }}</span>
                    }
                    <span class="provider-check" aria-hidden="true">
                      <svg viewBox="0 0 16 16" width="13" height="13">
                        <path
                          d="M3 8.5 6.2 12 13 4.5"
                          fill="none"
                          stroke="currentColor"
                          stroke-width="2"
                          stroke-linecap="round"
                          stroke-linejoin="round"
                        />
                      </svg>
                    </span>
                  </button>
                }
              </div>

              <!-- Contextual setup help for the chosen provider. -->
              @switch (provider()) {
                @case ("anthropic") {
                  @if (!isProviderAvailable("anthropic")) {
                    <div class="setup-well">
                      <p class="setup-title">Paste your Anthropic API key</p>
                      <p class="setup-text text-secondary">
                        Create a key at
                        <span class="inline-url">console.anthropic.com</span>,
                        then paste it here. Stored securely on this Mac.
                      </p>
                      <div class="row">
                        <input
                          type="password"
                          placeholder="sk-ant-…"
                          [value]="apiKey()"
                          (input)="onApiKey($event)"
                          [disabled]="savingKey()"
                          autocomplete="off"
                        />
                        <button
                          type="button"
                          class="btn"
                          (click)="saveAnthropicKey()"
                          [disabled]="
                            savingKey() || apiKey().trim().length === 0
                          "
                        >
                          {{ savingKey() ? "Saving…" : "Save key" }}
                        </button>
                      </div>
                      @if (keyError(); as kerr) {
                        <p class="ob-error text-danger">{{ kerr }}</p>
                      }
                    </div>
                  }
                }
                @case ("ollama") {
                  @if (!isProviderAvailable("ollama")) {
                    <div class="setup-well">
                      <p class="setup-title">Run Ollama locally</p>
                      <p class="setup-text text-secondary">
                        Install Ollama, then in a terminal run
                        <code class="inline-cmd">ollama serve</code> and pull a
                        model with
                        <code class="inline-cmd">ollama pull llama3.1</code>.
                      </p>
                      <button
                        type="button"
                        class="btn"
                        (click)="recheckProviders()"
                        [disabled]="checking()"
                      >
                        {{ checking() ? "Checking…" : "Re-check" }}
                      </button>
                    </div>
                  }
                }
                @case ("claude_code") {
                  @if (!isProviderAvailable("claude_code")) {
                    <div class="setup-well">
                      <p class="setup-title">Install the Claude CLI</p>
                      <p class="setup-text text-secondary">
                        Install the
                        <span class="inline-url">claude</span> command-line tool
                        and sign in, then re-check below.
                      </p>
                      <button
                        type="button"
                        class="btn"
                        (click)="recheckProviders()"
                        [disabled]="checking()"
                      >
                        {{ checking() ? "Checking…" : "Re-check" }}
                      </button>
                    </div>
                  } @else {
                    <div class="setup-well is-ok">
                      <span class="pill is-success">
                        <span class="pill-dot"></span>
                        Claude Code detected
                      </span>
                      <span class="setup-text text-secondary">
                        You’re set — nothing to configure.
                      </span>
                    </div>
                  }
                }
              }
            }

            <!-- ──────────────────────── VAULT (OPTIONAL) ────────────────── -->
            @case ("vault") {
              <div class="ob-head">
                <span class="ob-eyebrow">Optional</span>
                <h2 class="ob-title">Export to Obsidian?</h2>
                <p class="ob-sub text-secondary">
                  Everything is viewable right here in Murmur — the Meetings tab
                  keeps every recording, summary and transcript. Optionally also
                  export each note to your Obsidian vault.
                </p>
              </div>

              <div class="vault-pick">
                @if (vaultPath(); as vp) {
                  <div class="vault-chosen">
                    <span class="vault-folder" aria-hidden="true">
                      <svg viewBox="0 0 20 20" width="18" height="18">
                        <path
                          d="M2.5 5.5A1.5 1.5 0 0 1 4 4h3.2l1.4 1.6H16a1.5 1.5 0 0 1 1.5 1.5v7A1.5 1.5 0 0 1 16 15.6H4A1.5 1.5 0 0 1 2.5 14V5.5Z"
                          fill="none"
                          stroke="currentColor"
                          stroke-width="1.4"
                          stroke-linejoin="round"
                        />
                      </svg>
                    </span>
                    <span class="vault-path" [title]="vp">{{ vp }}</span>
                    <span class="pill is-success">
                      <span class="pill-dot"></span>
                      Linked
                    </span>
                  </div>
                  <button type="button" class="btn" (click)="pickVault()">
                    Change folder
                  </button>
                } @else {
                  <button
                    type="button"
                    class="btn btn-primary vault-cta"
                    (click)="pickVault()"
                  >
                    <span class="vault-folder" aria-hidden="true">
                      <svg viewBox="0 0 20 20" width="18" height="18">
                        <path
                          d="M2.5 5.5A1.5 1.5 0 0 1 4 4h3.2l1.4 1.6H16a1.5 1.5 0 0 1 1.5 1.5v7A1.5 1.5 0 0 1 16 15.6H4A1.5 1.5 0 0 1 2.5 14V5.5Z"
                          fill="none"
                          stroke="currentColor"
                          stroke-width="1.5"
                          stroke-linejoin="round"
                        />
                      </svg>
                    </span>
                    Choose vault folder
                  </button>
                  <p class="vault-skip-note text-muted">
                    Not using Obsidian? Skip this — your notes still live in
                    Murmur.
                  </p>
                }
              </div>
            }

            <!-- ───────────────────────────── DONE ───────────────────────── -->
            @case ("done") {
              <div class="done">
                <span class="done-mark" aria-hidden="true">
                  <svg viewBox="0 0 24 24" width="30" height="30">
                    <path
                      d="M5 12.5 10 17.5 19.5 7"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="2.4"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                    />
                  </svg>
                </span>
                <h2 class="ob-title done-title">You’re all set</h2>
                <p class="ob-sub text-secondary done-sub">
                  Murmur is ready. A few things worth knowing:
                </p>
                <ul class="tips">
                  <li class="tip" style="--d: 0">
                    <span class="kbd" aria-hidden="true">⌘R</span>
                    <span>Start &amp; stop recording instantly</span>
                  </li>
                  <li class="tip" style="--d: 1">
                    <span class="kbd" aria-hidden="true">⌘⇧R</span>
                    <span>Pop out the floating recorder bar</span>
                  </li>
                  <li class="tip" style="--d: 2">
                    <span class="kbd kbd-glyph" aria-hidden="true">◌</span>
                    <span
                      >Find Murmur in your menu bar, always a click away</span
                    >
                  </li>
                </ul>
                <button
                  type="button"
                  class="btn btn-primary ob-cta"
                  (click)="finish()"
                  [disabled]="finishing()"
                >
                  @if (finishing()) {
                    <span class="spin-ring" aria-hidden="true"></span>
                    Starting…
                  } @else {
                    Start recording
                    <span class="cta-arrow" aria-hidden="true">→</span>
                  }
                </button>
              </div>
            }
          }
        </div>

        <!-- Footer nav: Back + (Continue / Skip) on the working steps. -->
        @if (currentStep() !== "welcome" && currentStep() !== "done") {
          <div class="ob-nav">
            <button type="button" class="btn btn-ghost" (click)="back()">
              Back
            </button>
            <span class="ob-nav-spacer"></span>
            @if (currentStep() === "provider") {
              <button
                type="button"
                class="btn btn-ghost ob-skip"
                (click)="next()"
              >
                I’ll set this up later
              </button>
            }
            @if (currentStep() === "vault") {
              <button
                type="button"
                class="btn btn-ghost ob-skip"
                (click)="next()"
              >
                Skip — keep notes in Murmur
              </button>
            }
            <button
              type="button"
              class="btn btn-primary"
              (click)="next()"
              [disabled]="!canAdvance()"
            >
              Continue
              <span class="cta-arrow" aria-hidden="true">→</span>
            </button>
          </div>
        }
      </div>
    </div>
  `,
  styles: [
    `
      /* Full-bleed focused wizard — fills the view, centres the panel. The app
         header stays, but the stage owns the vertical space below it. */
      .ob {
        display: flex;
        align-items: center;
        justify-content: center;
        min-height: calc(100vh - 120px);
        padding: var(--space-5) 0 var(--space-7);
      }
      .ob-stage {
        position: relative;
        width: 100%;
        max-width: 560px;
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
      }

      /* ── Progress: filling dots + counter ─────────────────────────────── */
      .ob-progress {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-4);
        animation: rise 420ms var(--transition) both;
      }
      .dots {
        display: flex;
        align-items: center;
        gap: var(--space-2);
      }
      .dot {
        width: 8px;
        height: 8px;
        border-radius: var(--radius-pill);
        background: var(--border-strong);
        transition:
          width var(--transition),
          background var(--transition),
          box-shadow var(--transition);
      }
      .dot.is-done {
        background: var(--accent);
      }
      .dot.is-active {
        width: 26px;
        background: var(--accent-gradient);
        box-shadow: 0 0 12px rgba(110, 118, 255, 0.6);
      }
      .step-count {
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-size: 0.75rem;
        font-variant-numeric: tabular-nums;
        letter-spacing: 0.02em;
      }

      /* ── The panel — one frosted card, re-animated per step ───────────── */
      .ob-panel {
        padding: var(--space-6);
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
        min-height: 320px;
        justify-content: center;
      }
      /* Keying the animation off data-step replays the entrance on each step. */
      .ob-panel[data-step] {
        animation: panel-in 460ms var(--ease-spring) both;
      }
      @keyframes panel-in {
        from {
          opacity: 0;
          transform: translateY(14px) scale(0.985);
        }
        to {
          opacity: 1;
          transform: translateY(0) scale(1);
        }
      }

      /* ── Shared step copy ─────────────────────────────────────────────── */
      .ob-head {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .ob-eyebrow {
        color: var(--accent-hover);
        font-size: 0.75rem;
        font-weight: 600;
        letter-spacing: 0.08em;
        text-transform: uppercase;
      }
      .ob-title {
        margin: 0;
        font-size: 1.5rem;
        font-weight: 650;
        letter-spacing: -0.025em;
      }
      .ob-sub {
        margin: 0;
        font-size: 0.95rem;
        line-height: 1.55;
      }
      .ob-error {
        margin: 0;
        font-size: 0.85rem;
      }

      /* ── Welcome splash ───────────────────────────────────────────────── */
      .welcome {
        display: flex;
        flex-direction: column;
        align-items: center;
        text-align: center;
        gap: var(--space-4);
        padding: var(--space-4) 0;
      }
      .orb-brand {
        position: relative;
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 96px;
        height: 96px;
        margin-bottom: var(--space-2);
      }
      .orb-core {
        width: 64px;
        height: 64px;
        border-radius: 50%;
        background: var(--accent-gradient);
        box-shadow:
          var(--shadow-accent),
          0 0 40px rgba(110, 118, 255, 0.55),
          inset 0 2px 6px rgba(255, 255, 255, 0.4);
        animation: orb-float 4s ease-in-out infinite;
      }
      /* Two breathing rings echoing the record-screen orb language. */
      .orb-brand::before,
      .orb-brand::after {
        content: "";
        position: absolute;
        inset: 0;
        border-radius: 50%;
        border: 1.5px solid var(--accent);
        opacity: 0.5;
        animation: orb-ring 3s ease-in-out infinite;
      }
      .orb-brand::after {
        animation-delay: 1.5s;
      }
      @keyframes orb-ring {
        0% {
          transform: scale(0.66);
          opacity: 0.6;
        }
        100% {
          transform: scale(1.1);
          opacity: 0;
        }
      }
      @keyframes orb-float {
        0%,
        100% {
          transform: translateY(0) scale(1);
        }
        50% {
          transform: translateY(-6px) scale(1.03);
        }
      }
      .welcome-title {
        display: inline-flex;
        align-items: center;
        gap: var(--space-3);
        font-size: 1.65rem;
      }
      .brand-dot {
        width: 11px;
        height: 11px;
        border-radius: 50%;
        background: var(--accent-gradient);
        box-shadow: var(--shadow-accent);
      }
      .ob-lede {
        margin: 0;
        max-width: 38ch;
        color: var(--text-secondary);
        font-size: 1.0625rem;
        line-height: 1.55;
      }
      .ob-fineprint {
        margin: 0;
        font-size: 0.8125rem;
      }
      .ob-cta {
        height: 46px;
        padding: 0 var(--space-5);
        font-size: 1rem;
        margin-top: var(--space-2);
      }
      .cta-arrow {
        transition: transform var(--transition);
      }
      .ob-cta:hover .cta-arrow {
        transform: translateX(3px);
      }

      /* ── Form fields (model step) ─────────────────────────────────────── */
      .field-grid {
        display: grid;
        grid-template-columns: 1fr 1fr;
        gap: var(--space-4);
      }
      @media (max-width: 520px) {
        .field-grid {
          grid-template-columns: 1fr;
        }
      }
      .field {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .field-label {
        color: var(--text-secondary);
        font-size: 0.9rem;
        font-weight: 550;
      }
      .model-state {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
        min-height: 40px;
      }
      .model-note {
        font-size: 0.85rem;
      }

      .row {
        display: flex;
        gap: var(--space-2);
      }
      .row input {
        flex: 1;
        min-width: 0;
      }
      .row .btn {
        flex: none;
      }

      /* Inline spinner — reuses the record-screen spin language for buttons. */
      .spin-ring {
        width: 15px;
        height: 15px;
        border-radius: 50%;
        border: 2px solid rgba(255, 255, 255, 0.35);
        border-top-color: var(--text-on-accent);
        animation: spin 0.8s linear infinite;
      }
      @keyframes spin {
        to {
          transform: rotate(360deg);
        }
      }
      .dl-arrow,
      .vault-folder {
        display: inline-flex;
      }

      /* ── Provider chooser ─────────────────────────────────────────────── */
      .providers {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .provider {
        position: relative;
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        width: 100%;
        text-align: left;
        padding: var(--space-3) var(--space-4);
        padding-right: var(--space-7);
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        color: var(--text-primary);
        font-family: inherit;
        cursor: pointer;
        transition:
          border-color var(--transition),
          background var(--transition),
          transform var(--transition-fast),
          box-shadow var(--transition);
      }
      .provider:hover {
        border-color: var(--border-strong);
        background: var(--surface-hover);
      }
      .provider:active {
        transform: translateY(1px);
      }
      .provider:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .provider.is-selected {
        border-color: transparent;
        background: var(--accent-soft);
        box-shadow:
          0 0 0 1px var(--accent),
          var(--glass-highlight);
      }
      .provider-main {
        display: flex;
        align-items: center;
        gap: var(--space-3);
      }
      .provider-name {
        font-size: 0.95rem;
        font-weight: 600;
        letter-spacing: -0.01em;
      }
      .provider-reason {
        font-size: 0.8125rem;
        line-height: 1.4;
      }
      .provider-check {
        position: absolute;
        top: 50%;
        right: var(--space-4);
        transform: translateY(-50%) scale(0.5);
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 22px;
        height: 22px;
        border-radius: 50%;
        background: var(--accent-gradient);
        color: var(--text-on-accent);
        opacity: 0;
        transition:
          opacity var(--transition),
          transform var(--transition);
      }
      .provider.is-selected .provider-check {
        opacity: 1;
        transform: translateY(-50%) scale(1);
      }

      /* Contextual provider setup help. */
      .setup-well {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        padding: var(--space-4);
        border-radius: var(--radius-md);
        background: var(--surface-raised);
        border: 1px solid var(--border-subtle);
        animation: rise 320ms var(--transition) both;
      }
      .setup-well.is-ok {
        flex-direction: row;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
      }
      .setup-title {
        margin: 0;
        font-weight: 600;
        font-size: 0.95rem;
        color: var(--text-primary);
      }
      .setup-text {
        margin: 0;
        font-size: 0.875rem;
        line-height: 1.55;
      }
      .inline-url {
        font-family: var(--font-mono);
        font-size: 0.85em;
        color: var(--text-primary);
        user-select: text;
        -webkit-user-select: text;
      }
      .inline-cmd {
        font-family: var(--font-mono);
        font-size: 0.85em;
        padding: 1px 6px;
        border-radius: 6px;
        background: rgba(255, 255, 255, 0.07);
        border: 1px solid var(--border);
        color: var(--text-primary);
        user-select: text;
        -webkit-user-select: text;
        white-space: nowrap;
      }

      /* ── Vault step ───────────────────────────────────────────────────── */
      .vault-pick {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        align-items: flex-start;
      }
      .vault-cta {
        height: 46px;
        padding: 0 var(--space-5);
      }
      .vault-chosen {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        width: 100%;
        padding: var(--space-3) var(--space-4);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
      }
      .vault-folder {
        color: var(--accent-hover);
        flex: none;
      }
      .vault-path {
        flex: 1;
        min-width: 0;
        font-family: var(--font-mono);
        font-size: 0.85rem;
        color: var(--text-primary);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        user-select: text;
        -webkit-user-select: text;
      }
      .vault-skip-note {
        margin: 0;
        font-size: 0.85rem;
      }

      /* ── Done step ────────────────────────────────────────────────────── */
      .done {
        display: flex;
        flex-direction: column;
        align-items: center;
        text-align: center;
        gap: var(--space-3);
      }
      .done-mark {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 64px;
        height: 64px;
        margin-bottom: var(--space-1);
        border-radius: 50%;
        background: var(--success-soft);
        color: var(--success);
        box-shadow:
          0 0 0 1px rgba(74, 222, 128, 0.3),
          0 0 32px rgba(74, 222, 128, 0.28);
        animation: pop-in 520ms var(--ease-spring) both;
      }
      @keyframes pop-in {
        from {
          opacity: 0;
          transform: scale(0.5);
        }
        to {
          opacity: 1;
          transform: scale(1);
        }
      }
      .done-title {
        margin: 0;
      }
      .done-sub {
        margin: 0 0 var(--space-2);
      }
      .tips {
        list-style: none;
        margin: 0 0 var(--space-3);
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        width: 100%;
      }
      .tip {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-3) var(--space-4);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
        color: var(--text-secondary);
        font-size: 0.9rem;
        text-align: left;
        animation: rise 420ms var(--transition) both;
        animation-delay: calc(120ms + var(--d) * 80ms);
      }
      .kbd {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        height: 28px;
        min-width: 34px;
        padding: 0 var(--space-2);
        border-radius: var(--radius-sm);
        background: rgba(255, 255, 255, 0.07);
        border: 1px solid var(--border);
        color: var(--text-secondary);
        font-family: var(--font-mono);
        font-size: 0.8rem;
        font-weight: 500;
        flex: none;
      }
      .kbd-glyph {
        font-size: 1rem;
        line-height: 1;
      }

      /* ── Footer nav ───────────────────────────────────────────────────── */
      .ob-nav {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        animation: rise 460ms var(--transition) both;
        animation-delay: 60ms;
      }
      .ob-nav-spacer {
        flex: 1;
      }
      .ob-skip {
        color: var(--text-muted);
      }

      @media (prefers-reduced-motion: reduce) {
        .orb-core,
        .orb-brand::before,
        .orb-brand::after,
        .spin-ring {
          animation: none !important;
        }
      }
    `,
  ],
})
export class OnboardingComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly router = inject(Router);

  readonly steps = STEPS;
  readonly currentStep = signal<Step>("welcome");
  readonly stepIndex = computed(() => STEPS.indexOf(this.currentStep()));

  /** Loaded config snapshot — preserved so finishing never drops other settings. */
  private loadedConfig: AppConfigDto | null = null;

  /** Model step. */
  readonly language = signal("");
  readonly modelSize = signal("small");
  readonly modelPresent = signal<boolean | null>(null);
  readonly downloading = signal(false);
  readonly downloadError = signal<string | null>(null);
  readonly sizeHint = computed(() => SIZE_HINTS[this.modelSize()] ?? "");

  /** Provider step. */
  readonly providers = signal<ProviderStatus[]>([]);
  readonly provider = signal("claude_code");
  readonly checking = signal(false);
  readonly apiKey = signal("");
  readonly savingKey = signal(false);
  readonly keyError = signal<string | null>(null);

  /** Vault step. */
  readonly vaultPath = signal<string | null>(null);

  /** Done step. */
  readonly finishing = signal(false);

  /** Gate for the per-step Continue button. */
  readonly canAdvance = computed(() => {
    switch (this.currentStep()) {
      case "model":
        // Must have a ready model before transcription can work.
        return this.modelPresent() === true;
      case "provider":
      case "vault":
      default:
        return true;
    }
  });

  async ngOnInit(): Promise<void> {
    try {
      const cfg = await this.ipc.getConfig();
      this.loadedConfig = cfg;
      this.language.set(cfg.language ?? "");
      this.modelSize.set(cfg.modelSize ?? "small");
      this.provider.set(cfg.providerId ?? "claude_code");
      this.vaultPath.set(cfg.vaultPath ?? null);
    } catch {
      // Fresh install with no config yet — defaults already cover us.
    }
  }

  labelFor(id: string): string {
    return PROVIDER_LABELS[id] ?? id;
  }

  isProviderAvailable(id: string): boolean {
    return this.providers().some((p) => p.id === id && p.available);
  }

  // ── Navigation ──────────────────────────────────────────────────────────

  async next(): Promise<void> {
    const i = this.stepIndex();
    if (i >= STEPS.length - 1) return;
    const target = STEPS[i + 1];
    this.currentStep.set(target);
    await this.onEnterStep(target);
  }

  async back(): Promise<void> {
    const i = this.stepIndex();
    if (i <= 0) return;
    const target = STEPS[i - 1];
    this.currentStep.set(target);
    await this.onEnterStep(target);
  }

  /** Side-effects run when a step becomes visible (probe model / providers). */
  private async onEnterStep(step: Step): Promise<void> {
    if (step === "model") {
      await this.persistConfig();
      this.modelPresent.set(await this.ipc.modelPresent());
    } else if (step === "provider") {
      await this.recheckProviders();
    }
  }

  // ── Model step ────────────────────────────────────────────────────────────

  async onLanguage(event: Event): Promise<void> {
    this.language.set((event.target as HTMLSelectElement).value);
    await this.refreshModelPresence();
  }

  async onModelSize(event: Event): Promise<void> {
    this.modelSize.set((event.target as HTMLSelectElement).value);
    await this.refreshModelPresence();
  }

  /** Persist the chosen language + size, then re-check what's on disk. */
  private async refreshModelPresence(): Promise<void> {
    this.modelPresent.set(null);
    await this.persistConfig();
    this.modelPresent.set(await this.ipc.modelPresent());
  }

  async downloadModel(): Promise<void> {
    this.downloadError.set(null);
    this.downloading.set(true);
    try {
      // The model is fetched for the SAVED language + size — persist first.
      await this.persistConfig();
      await this.ipc.downloadModel();
      this.modelPresent.set(await this.ipc.modelPresent());
    } catch (e) {
      this.downloadError.set(String(e));
    } finally {
      this.downloading.set(false);
    }
  }

  // ── Provider step ───────────────────────────────────────────────────────

  selectProvider(id: string): void {
    this.provider.set(id);
    this.keyError.set(null);
    void this.persistConfig();
  }

  async recheckProviders(): Promise<void> {
    this.checking.set(true);
    try {
      this.providers.set(await this.ipc.providerStatuses());
    } finally {
      this.checking.set(false);
    }
  }

  onApiKey(event: Event): void {
    this.apiKey.set((event.target as HTMLInputElement).value);
  }

  async saveAnthropicKey(): Promise<void> {
    const key = this.apiKey().trim();
    if (!key) return;
    this.keyError.set(null);
    this.savingKey.set(true);
    try {
      await this.ipc.setAnthropicKey(key);
      this.apiKey.set("");
      // Re-probe so the "Available" pill updates once the key takes effect.
      await this.recheckProviders();
    } catch (e) {
      this.keyError.set(String(e));
    } finally {
      this.savingKey.set(false);
    }
  }

  // ── Vault step ────────────────────────────────────────────────────────────

  async pickVault(): Promise<void> {
    const dir = await open({ directory: true, multiple: false });
    if (typeof dir === "string") {
      this.vaultPath.set(dir);
      await this.persistConfig();
    }
  }

  // ── Finish ──────────────────────────────────────────────────────────────

  async finish(): Promise<void> {
    this.finishing.set(true);
    try {
      await this.persistConfig(true);
      await this.router.navigate(["/record"]);
    } catch {
      // If saving the final flag fails, let them retry rather than trap them.
      this.finishing.set(false);
    }
  }

  /**
   * Save the current wizard choices, preserving every other config field from
   * the loaded snapshot. `markOnboarded` flips the first-run gate on the final
   * step. A tracked timeout is NOT needed here — these are awaited one-shots.
   */
  private async persistConfig(markOnboarded = false): Promise<void> {
    const base = this.loadedConfig;
    const cfg: AppConfigDto = {
      providerId: this.provider(),
      vaultPath: this.vaultPath(),
      vaultSubfolder: base?.vaultSubfolder ?? null,
      whisperModelPath: base?.whisperModelPath ?? null,
      language: this.language() || null,
      anthropicModel: base?.anthropicModel ?? "claude-opus-4-8",
      ollamaBaseUrl: base?.ollamaBaseUrl ?? "http://localhost:11434",
      ollamaModel: base?.ollamaModel ?? "llama3.1",
      claudeBinary: base?.claudeBinary ?? "claude",
      inputDevice: base?.inputDevice ?? null,
      captureSystemAudio: base?.captureSystemAudio ?? false,
      modelSize: this.modelSize(),
      voiceTrigger: base?.voiceTrigger ?? false,
      onboarded: markOnboarded ? true : (base?.onboarded ?? false),
      noteStyle: base?.noteStyle ?? "standard",
      autoOrganize: base?.autoOrganize ?? false,
      noteLanguage: base?.noteLanguage ?? "auto",
    };
    await this.ipc.saveConfig(cfg);
    // Keep the snapshot current so successive saves don't clobber fresh choices.
    this.loadedConfig = cfg;
  }
}
