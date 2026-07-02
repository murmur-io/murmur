import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  OnInit,
  computed,
  inject,
  signal,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { FormBuilder, FormControl, ReactiveFormsModule } from "@angular/forms";
import { startWith } from "rxjs";
import { Router } from "@angular/router";
import { open } from "@tauri-apps/plugin-dialog";
import { IpcService } from "../../core/ipc.service";
import type {
  AppConfigDto,
  AppInfo,
  BrainBackend,
  BrainModelDto,
  GatewayHealth,
  GatewayModel,
  InputDeviceInfo,
  ProviderStatus,
  ReindexResult,
} from "../../core/models";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { ThemeService, type ThemeMode } from "../../services/theme.service";
import { UpdateService } from "../../services/update.service";

/** One entry in the macOS-style Settings sidebar. `keywords` feeds the search box. */
interface SettingsSection {
  readonly id: string;
  readonly label: string;
  readonly keywords: string;
}

/**
 * The sidebar sections, in display order. `keywords` are matched (alongside the
 * label) by the search box so typing a setting's name surfaces its section.
 */
const SETTINGS_SECTIONS: readonly SettingsSection[] = [
  { id: "appearance", label: "Appearance", keywords: "theme light dark system look colour color mode" },
  { id: "general", label: "General", keywords: "provider vault folder subfolder whisper model path setup onboarding" },
  { id: "transcription", label: "Transcription", keywords: "language quality whisper model download on-device size accuracy" },
  { id: "audio", label: "Audio & Capture", keywords: "microphone input device system audio vad smart speech detection high fidelity masters diarization remote speakers echo cancellation aec voice trigger hands-free" },
  { id: "notes", label: "Notes", keywords: "summary style brief detailed action language auto organize subfolders thematic" },
  { id: "brain", label: "Brain & AI", keywords: "assistant backend cloud local gguf model reasoning effort semantic search embedding reindex in-meeting voice assistant wake" },
  { id: "connectors", label: "Connectors", keywords: "web search brave egress api key internet" },
  { id: "providers", label: "Providers", keywords: "anthropic ollama claude code gateway openai api key availability model binary" },
  { id: "privacy", label: "Privacy & Integrations", keywords: "redaction firewall cloud processing consent locked folders mcp server claude desktop" },
  { id: "obsidian", label: "Obsidian", keywords: "vault markdown notes companion export wikilinks" },
  { id: "about", label: "About", keywords: "about version update check for updates release changelog product info" },
];

@Component({
  selector: "app-settings",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  template: `
    <section class="settings-shell" [formGroup]="form">
      <!-- macOS-style left rail: search over sections, then the section list. -->
      <aside class="settings-sidebar" aria-label="Settings">
        <div class="sidebar-search">
          <svg
            class="sidebar-search-icon"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            stroke-width="1.6"
            stroke-linecap="round"
            aria-hidden="true"
          >
            <circle cx="7" cy="7" r="4.5" />
            <path d="M10.5 10.5 14 14" />
          </svg>
          <input
            type="search"
            class="sidebar-search-input"
            [formControl]="searchControl"
            placeholder="Search"
            aria-label="Search settings"
            autocomplete="off"
            spellcheck="false"
          />
        </div>

        <nav class="sidebar-nav" aria-label="Settings sections">
          @for (s of visibleSections(); track s.id) {
            <button
              type="button"
              class="nav-item"
              [class.active]="activeSection() === s.id"
              [attr.aria-current]="activeSection() === s.id ? 'page' : null"
              (click)="selectSection(s.id)"
            >
              <span class="nav-icon" aria-hidden="true">
                @switch (s.id) {
                  @case ("appearance") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="8" r="2.6" /><path d="M8 1.5v1.6M8 12.9v1.6M2.4 2.4l1.1 1.1M12.5 12.5l1.1 1.1M1.5 8h1.6M12.9 8h1.6M2.4 13.6l1.1-1.1M12.5 3.5l1.1-1.1" /></svg>
                  }
                  @case ("general") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M2 4.5h8M13 4.5h1M2 11.5h5M10 11.5h4" /><circle cx="11" cy="4.5" r="1.6" /><circle cx="8" cy="11.5" r="1.6" /></svg>
                  }
                  @case ("transcription") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M2 8h1.5M5 5v6M8 2.5v11M11 5v6M14 8h-1.5" /></svg>
                  }
                  @case ("audio") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="6" y="1.5" width="4" height="8" rx="2" /><path d="M3.5 7.5a4.5 4.5 0 0 0 9 0M8 12v2.5" /></svg>
                  }
                  @case ("notes") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M4 1.5h5l3 3v10H4z" /><path d="M9 1.5v3h3M5.8 8h4.4M5.8 10.6h4.4" /></svg>
                  }
                  @case ("brain") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M8 2.2 9 5l2.8 1L9 7l-1 2.8L7 7 4.2 6 7 5z" /><path d="M12 9.5l.6 1.5 1.5.6-1.5.6-.6 1.5-.6-1.5L9.4 11.6l1.5-.6z" /></svg>
                  }
                  @case ("connectors") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="8" r="6.2" /><path d="M1.8 8h12.4M8 1.8c1.8 1.7 2.8 3.9 2.8 6.2S9.8 12.5 8 14.2C6.2 12.5 5.2 10.3 5.2 8S6.2 3.5 8 1.8z" /></svg>
                  }
                  @case ("providers") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="2.5" width="12" height="4.5" rx="1.4" /><rect x="2" y="9" width="12" height="4.5" rx="1.4" /><path d="M4.4 4.75h.01M4.4 11.25h.01" /></svg>
                  }
                  @case ("privacy") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M8 1.6 3 3.5v3.4c0 3 2 5.3 5 6.1 3-.8 5-3.1 5-6.1V3.5L8 1.6z" /><path d="M6 7.7 7.4 9.1 10 6.3" /></svg>
                  }
                  @case ("obsidian") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M6 1.8 2.8 5.3 4.7 12 7 14l-.8-6z" /><path d="M6 1.8 6.2 8 7 14l3.4-2.6L12 5.4 9 2z" /></svg>
                  }
                  @case ("about") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="8" r="6.2" /><path d="M8 7.4v3.6M8 5.1h.01" /></svg>
                  }
                }
              </span>
              <span class="nav-label">{{ s.label }}</span>
            </button>
          } @empty {
            <p class="nav-empty text-muted">
              No settings match “{{ searchQuery() }}”.
            </p>
          }
        </nav>

        <!-- Save applies the whole form regardless of the visible section. -->
        <div class="sidebar-footer">
          <button type="button" class="btn btn-primary sidebar-save" (click)="save()">
            Save settings
          </button>
          @if (saved()) {
            <span class="pill is-success saved-pill">
              <span class="pill-dot"></span>
              Saved
            </span>
          }
        </div>
      </aside>

      <!-- Right pane: the selected section's controls. -->
      <div class="settings-content">
        @if (loadError(); as err) {
          <div class="banner is-danger" role="alert">
            <span class="banner-icon" aria-hidden="true">!</span>
            <span>Couldn't load settings: {{ err }}</span>
          </div>
        }

        <header class="content-header">
          <h2>{{ activeSectionLabel() }}</h2>
        </header>

        <div class="section-body">
          @switch (activeSection()) {
            <!-- ── Appearance: Light / Dark / System theme (applies instantly) ── -->
            @case ("appearance") {
              <div class="card appearance-card">
                <div class="appearance-copy">
                  <h3>Appearance</h3>
                  <p class="text-secondary">
                    Choose how Murmur looks. <b>System</b> follows your macOS
                    Light/Dark setting automatically.
                  </p>
                </div>
                <div class="theme-seg" role="group" aria-label="Theme">
                  <button
                    type="button"
                    [class.active]="themeMode() === 'light'"
                    [attr.aria-pressed]="themeMode() === 'light'"
                    (click)="setTheme('light')"
                  >
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="4" /><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" /></svg>
                    Light
                  </button>
                  <button
                    type="button"
                    [class.active]="themeMode() === 'dark'"
                    [attr.aria-pressed]="themeMode() === 'dark'"
                    (click)="setTheme('dark')"
                  >
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z" /></svg>
                    Dark
                  </button>
                  <button
                    type="button"
                    [class.active]="themeMode() === 'system'"
                    [attr.aria-pressed]="themeMode() === 'system'"
                    (click)="setTheme('system')"
                  >
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="3" y="4" width="18" height="12" rx="2" /><path d="M8 20h8M12 16v4" /></svg>
                    System
                  </button>
                </div>
              </div>
            }

            <!-- ── General ── -->
            @case ("general") {
              <div class="card">
                <fieldset>
                  <legend>General</legend>

                  <label class="field">
                    <span class="field-label">Provider</span>
                    <select formControlName="providerId">
                      <option value="claude_code">Claude Code (default)</option>
                      <option value="anthropic">Anthropic API</option>
                      <option value="ollama">Ollama</option>
                      <option value="gateway">AI Gateway (OpenAI-compatible)</option>
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
                        placeholder="leave blank — auto-managed by the model chosen in Settings → Transcription"
                      />
                      <button type="button" class="btn" (click)="pickModel()">
                        Browse…
                      </button>
                    </span>
                  </label>
                </fieldset>
              </div>

              <div class="card setup-card">
                <div class="setup-copy">
                  <span class="setup-title">First-run setup</span>
                  <span class="text-secondary setup-sub">
                    Re-open the guided wizard. Your existing settings are preserved
                    and prefilled.
                  </span>
                </div>
                <button
                  type="button"
                  class="btn btn-ghost"
                  (click)="rerunOnboarding()"
                >
                  Run setup again
                </button>
              </div>
            }

            <!-- ── Transcription model: language + quality + on-demand download ── -->
            @case ("transcription") {
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
                    @if (downloadingModel()) {
                      <div class="brain-progress" role="status">
                        <div class="brain-progress-track" aria-hidden="true">
                          <div
                            class="brain-progress-fill"
                            [style.width.%]="modelDownloadFrac() * 100"
                          ></div>
                        </div>
                        <span class="brain-progress-label text-muted">
                          @if (modelDownloadFrac() > 0) {
                            Downloading… {{ modelPct() }}
                          } @else {
                            Downloading…
                          }
                        </span>
                      </div>
                      <span class="text-muted model-note">
                        Fetching the model — large models can take a few minutes.
                      </span>
                    } @else {
                      <button
                        type="button"
                        class="btn btn-primary"
                        (click)="downloadModel()"
                      >
                        Download ({{ downloadHint() }})
                      </button>
                      <span class="text-muted model-note">
                        {{ downloadHint() }}, one time, on-device.
                      </span>
                    }
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
            }

            <!-- ── Audio & Capture: mic + system audio + capture-quality toggles ── -->
            @case ("audio") {
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
            }

            <!-- ── Notes: how the AI provider writes & files each meeting note ── -->
            @case ("notes") {
              <div class="card notes-card">
                <div class="notes-copy">
                  <h3>Notes</h3>
                  <p class="text-secondary notes-sub">
                    Shape how your AI provider writes each summary and where it lands
                    in your vault.
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
                      Your AI provider files each note into a topic subfolder of your
                      vault (e.g. Standups, 1-1s, Acme Project).
                    </span>
                  </span>
                  <input type="checkbox" formControlName="autoOrganize" />
                </label>
              </div>
            }

            <!-- ── Brain / AI — the assistant backend + in-meeting voice assistant ── -->
            @case ("brain") {
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
            }

            <!-- ── Connectors — web search (NEW CLOUD EGRESS, surfaced loudly) ── -->
            @case ("connectors") {
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
            }

            <!-- ── Providers — provider config, gateway, keys, availability ── -->
            @case ("providers") {
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

                  <label class="toggle-row">
                    <span class="toggle-copy">
                      <span class="toggle-title">Pass shell environment to the Claude CLI</span>
                      <span class="text-secondary toggle-sub">
                        Restores older-version behavior: an ANTHROPIC_API_KEY (and proxy /
                        base-URL vars) set in your shell reach the claude CLI again, so it
                        can authenticate via your env key. Off by default for security — your
                        database encryption keys are never passed through.
                      </span>
                    </span>
                    <input type="checkbox" formControlName="claudeCodeInheritEnv" />
                  </label>
                </fieldset>
              </div>

              <!-- AI Gateway configuration (shown only when "gateway" provider is selected) -->
              @if (form.controls.providerId.value === 'gateway') {
                <div class="card gateway-card">
                  <fieldset>
                    <legend>AI Gateway</legend>

                    <label class="field">
                      <span class="field-label">Base URL</span>
                      <input
                        formControlName="gatewayBaseUrl"
                        placeholder="http://localhost:4000/v1"
                        autocomplete="off"
                        spellcheck="false"
                      />
                      @if (gatewayUrlWarning()) {
                        <span class="field-help text-danger">
                          Use https:// (http:// is allowed only for localhost).
                        </span>
                      }
                      <span class="field-help text-muted">
                        Enter your gateway's OpenAI-compatible base URL (e.g.
                        https://…/v1) — or the full chat-completions endpoint if
                        your gateway uses a custom route (e.g. a Kong serverless
                        route like https://…/test).
                      </span>
                    </label>

                    <div class="field">
                      <span class="field-label">Model</span>
                      <div class="gateway-model-row">
                        @if (gatewayModels().length > 0) {
                          <select formControlName="gatewayModel" class="gateway-model-select">
                            <option value="">Gateway default</option>
                            @for (m of gatewayModels(); track m.id) {
                              <option [value]="m.id">{{ m.id }}</option>
                            }
                            <!--
                              If the currently-saved model is not in the catalog (e.g. the
                              catalog changed), keep it selectable so a manually-typed value
                              is never silently lost. gatewayModelIsCustom() is a computed
                              to avoid arrow-function syntax in the template.
                            -->
                            @if (gatewayModelIsCustom()) {
                              <option [value]="form.controls.gatewayModel.value">
                                {{ form.controls.gatewayModel.value }} (custom)
                              </option>
                            }
                          </select>
                        } @else {
                          <input
                            formControlName="gatewayModel"
                            placeholder="gpt-4o (leave blank to use the gateway default)"
                            autocomplete="off"
                            spellcheck="false"
                            class="gateway-model-input"
                          />
                        }
                        <button
                          type="button"
                          class="btn btn-ghost gateway-model-refresh"
                          (click)="refreshGatewayModels()"
                          [disabled]="gatewayModelsLoading()"
                          title="Fetch models from the gateway's /v1/models endpoint"
                        >
                          @if (gatewayModelsLoading()) {
                            Loading…
                          } @else {
                            ↻ Refresh models
                          }
                        </button>
                      </div>
                      @if (gatewayModelError()) {
                        <span class="field-help text-muted">
                          Couldn't load models — check the base URL and key, or type the
                          model id manually.
                        </span>
                      } @else {
                        <span class="field-help text-muted">
                          Sent as the <code>model</code> field in every request — leave
                          blank to let the gateway choose.
                        </span>
                      }
                    </div>

                    <!-- AI Gateway (Phase 4) — health probe -->
                    <div class="gateway-health-row">
                      <span class="text-secondary">Gateway status</span>
                      <div class="gateway-health-status">
                        @if (gatewayHealth(); as h) {
                          @if (h.reachable) {
                            <span class="pill is-success">
                              <span class="pill-dot"></span>
                              {{ h.modelCount }} {{ h.modelCount === 1 ? 'model' : 'models' }} reachable
                            </span>
                          } @else {
                            <span class="pill">
                              <span class="pill-dot gateway-dot-unreachable"></span>
                              Gateway unreachable
                            </span>
                          }
                        } @else {
                          <span class="text-muted gateway-health-hint">Not checked</span>
                        }
                        <button
                          type="button"
                          class="btn btn-ghost gateway-health-btn"
                          (click)="checkGatewayHealth()"
                          [disabled]="gatewayHealthChecking()"
                        >
                          @if (gatewayHealthChecking()) {
                            Checking…
                          } @else {
                            Check
                          }
                        </button>
                      </div>
                    </div>

                    <!-- Gateway API key (optional) -->
                    <div class="key-status">
                      <span class="text-secondary">
                        API key
                        <span class="text-muted">(optional)</span>
                      </span>
                      @if (hasGatewayKey()) {
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
                        [formControl]="gatewayKeyControl"
                        placeholder="sk-… or any bearer token"
                        autocomplete="new-password"
                      />
                      <button
                        type="button"
                        class="btn"
                        (click)="saveGatewayKey()"
                        [disabled]="!gatewayKeyControl.value.trim()"
                      >
                        Save key
                      </button>
                      @if (hasGatewayKey()) {
                        <button
                          type="button"
                          class="btn btn-ghost"
                          (click)="removeGatewayKey()"
                        >
                          Clear
                        </button>
                      }
                    </span>
                    @if (gatewayKeyError()) {
                      <p class="text-danger gateway-key-error">{{ gatewayKeyError() }}</p>
                    }
                  </fieldset>

                  <!-- Destination banner: calmer note for localhost, warning for remote -->
                  @if (gatewayDestination(); as dest) {
                    @if (dest.isRemote) {
                      <div class="banner is-warning gateway-banner">
                        <span class="banner-icon" aria-hidden="true">!</span>
                        <span>
                          Content will be sent to <strong>{{ dest.host }}</strong> over
                          the network — always scrubbed by the redaction firewall first
                          and requires cloud-egress consent.
                        </span>
                      </div>
                    } @else {
                      <div class="banner gateway-banner">
                        <span class="banner-icon" aria-hidden="true">i</span>
                        <span>
                          Localhost gateway — a local gateway can still forward to the
                          cloud, so content is still redacted and consent-gated.
                        </span>
                      </div>
                    }
                  }
                </div>
              }

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

              <!-- Provider availability -->
              <div class="card">
                <div class="provider-avail-head">
                  <h3>Provider availability</h3>
                  <button type="button" class="btn btn-sm" (click)="refreshProviders()">
                    Check providers
                  </button>
                </div>
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
            }

            <!-- ── Privacy & integrations: redaction firewall + local MCP server ── -->
            @case ("privacy") {
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
            }

            <!-- ── Works with Obsidian — optional vault companion ── -->
            @case ("obsidian") {
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
            }

            <!-- ── About — product identity + manual update check ── -->
            @case ("about") {
              <div class="card about-card">
                @if (appInfo(); as info) {
                  <div class="about-head">
                    <span class="about-name">{{ info.name }}</span>
                    <span class="about-version text-muted"
                      >Version {{ info.version }}</span
                    >
                  </div>
                  <p class="text-secondary about-desc">{{ info.description }}</p>
                } @else {
                  <p class="text-secondary about-desc">
                    Loading product info…
                  </p>
                }

                <div class="about-update">
                  <span class="about-section-label text-muted"
                    >Software update</span
                  >
                  <div class="about-update-row">
                    <button
                      type="button"
                      class="btn"
                      (click)="checkForUpdates()"
                      [disabled]="updateStatus() === 'checking'"
                    >
                      @if (updateStatus() === "checking") {
                        <span class="spin-ring" aria-hidden="true"></span>
                        Checking…
                      } @else {
                        Check for updates
                      }
                    </button>

                    @switch (updateStatus()) {
                      @case ("available") {
                        @if (latestUpdate(); as upd) {
                          <span class="about-update-result">
                            <span class="text-secondary"
                              >Version {{ upd.latestVersion }} is available.</span
                            >
                            <button
                              type="button"
                              class="btn btn-primary"
                              (click)="downloadUpdate(upd.releaseUrl)"
                            >
                              Download
                            </button>
                          </span>
                        }
                      }
                      @case ("upToDate") {
                        <span class="text-muted about-update-line"
                          >You're up to date.</span
                        >
                      }
                      @case ("error") {
                        <span class="text-danger about-update-line"
                          >Couldn't check for updates.</span
                        >
                      }
                    }
                  </div>
                </div>
              </div>
            }
          }
        </div>
      </div>
    </section>
  `,
  styles: [
    `
      /* ── macOS-style two-pane shell: sidebar + content ── */
      .settings-shell {
        display: grid;
        grid-template-columns: 216px minmax(0, 1fr);
        gap: var(--space-5);
        align-items: start;
      }

      /* Left rail — sticky under the app header, its own quiet frosted panel. */
      .settings-sidebar {
        position: sticky;
        top: calc(var(--space-8) + var(--space-2));
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        padding: var(--space-3);
        border-radius: var(--radius-lg);
        background: var(--surface-raised);
        border: 1px solid var(--glass-border);
        box-shadow: var(--glass-highlight);
        animation: rise 380ms var(--transition) both;
      }

      .sidebar-search {
        position: relative;
        display: flex;
        align-items: center;
      }
      .sidebar-search-icon {
        position: absolute;
        left: var(--space-3);
        width: 15px;
        height: 15px;
        color: var(--text-muted);
        pointer-events: none;
      }
      .sidebar-search-input {
        width: 100%;
        height: 34px;
        padding: 0 var(--space-3) 0 calc(var(--space-6) + var(--space-1));
        border: 1px solid var(--border);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        color: var(--text-primary);
        font: inherit;
        font-size: 0.875rem;
      }
      .sidebar-search-input::placeholder {
        color: var(--text-muted);
      }
      .sidebar-search-input:focus-visible {
        outline: none;
        border-color: var(--accent-hover);
        box-shadow: 0 0 0 3px var(--accent-soft);
      }
      /* Hide the native WebKit search clear affordance for a clean rail. */
      .sidebar-search-input::-webkit-search-decoration,
      .sidebar-search-input::-webkit-search-cancel-button {
        -webkit-appearance: none;
      }

      .sidebar-nav {
        display: flex;
        flex-direction: column;
        gap: 2px;
      }
      .nav-item {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        width: 100%;
        padding: var(--space-2) var(--space-3);
        border: 0;
        border-radius: var(--radius-md);
        background: transparent;
        color: var(--text-secondary);
        font: inherit;
        font-size: 0.9rem;
        font-weight: 550;
        text-align: left;
        cursor: pointer;
        transition:
          background var(--transition-fast),
          color var(--transition-fast);
      }
      .nav-item:hover {
        background: var(--surface-input);
        color: var(--text-primary);
      }
      .nav-item.active {
        background: var(--accent-soft);
        color: var(--accent);
      }
      .nav-icon {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 20px;
        height: 20px;
        flex: none;
        color: currentColor;
      }
      .nav-icon svg {
        width: 17px;
        height: 17px;
      }
      .nav-label {
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .nav-empty {
        margin: var(--space-2) var(--space-1);
        font-size: 0.85rem;
        line-height: 1.5;
      }

      .sidebar-footer {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex-wrap: wrap;
        margin-top: var(--space-1);
        padding-top: var(--space-3);
        border-top: 1px solid var(--border-subtle);
      }
      .sidebar-save {
        flex: 1 1 auto;
      }
      .saved-pill {
        flex: none;
      }

      /* Right pane — the section title + its stacked cards. */
      .settings-content {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
        min-width: 0;
      }
      .content-header h2 {
        margin: 0;
        font-size: 1.35rem;
        font-weight: 650;
        letter-spacing: -0.01em;
      }
      .section-body {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
        animation: rise 320ms var(--transition) both;
      }

      /* About — product identity + manual update check. */
      .about-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .about-head {
        display: flex;
        align-items: baseline;
        gap: var(--space-3);
        flex-wrap: wrap;
      }
      .about-name {
        font-size: 1.15rem;
        font-weight: 650;
        letter-spacing: -0.01em;
      }
      .about-version {
        font-size: 0.9rem;
      }
      .about-desc {
        margin: 0;
        line-height: 1.55;
      }
      .about-update {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        padding-top: var(--space-3);
        border-top: 1px solid var(--border-subtle);
      }
      .about-section-label {
        font-size: 0.72rem;
        font-weight: 650;
        letter-spacing: 0.04em;
        text-transform: uppercase;
      }
      .about-update-row,
      .about-update-result {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
      }
      .about-update-line {
        font-size: 0.9rem;
      }

      /* Collapse to a single column on narrow widths (sidebar stacks on top). */
      @media (max-width: 640px) {
        .settings-shell {
          grid-template-columns: 1fr;
        }
        .settings-sidebar {
          position: static;
        }
        .sidebar-nav {
          flex-direction: row;
          flex-wrap: wrap;
        }
        .nav-item {
          width: auto;
        }
      }

      /* --- Appearance / theme --- */
      .appearance-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .appearance-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .appearance-copy h3 {
        margin: 0;
      }
      .appearance-copy p {
        margin: 0;
        font-size: 0.875rem;
      }
      .theme-seg {
        display: inline-flex;
        gap: var(--space-1);
        padding: var(--space-1);
        width: fit-content;
        background: var(--surface-input);
        border: 1px solid var(--border);
        border-radius: var(--radius-pill);
      }
      .theme-seg button {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        padding: var(--space-2) var(--space-4);
        border: 0;
        background: transparent;
        color: var(--text-secondary);
        border-radius: var(--radius-pill);
        font: inherit;
        font-weight: 600;
        font-size: 0.875rem;
        cursor: pointer;
        transition:
          background var(--transition-fast),
          color var(--transition-fast);
      }
      .theme-seg button svg {
        width: 16px;
        height: 16px;
      }
      .theme-seg button:hover {
        color: var(--text-primary);
      }
      .theme-seg button.active {
        background: var(--accent-soft);
        color: var(--accent);
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

      /* --- General: re-run setup call-out --- */
      .setup-card {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-4);
        flex-wrap: wrap;
      }
      .setup-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        min-width: 0;
      }
      .setup-title {
        color: var(--text-primary);
        font-size: 0.95rem;
        font-weight: 550;
      }
      .setup-sub {
        font-size: 0.85rem;
        line-height: 1.5;
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

      /* --- Provider availability list --- */
      .provider-avail-head {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
        margin-bottom: var(--space-2);
      }
      .provider-avail-head h3 {
        margin: 0;
      }
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

      /* --- AI Gateway (Phase 3) — live model picker row --- */
      .gateway-model-row {
        display: flex;
        gap: var(--space-2);
        align-items: center;
        flex-wrap: wrap;
      }
      .gateway-model-select,
      .gateway-model-input {
        flex: 1 1 auto;
        min-width: 0;
      }
      .gateway-model-refresh {
        flex: none;
        height: 36px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
        white-space: nowrap;
      }

      /* --- AI Gateway (Phase 4) — health probe row --- */
      .gateway-health-row {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
        margin-bottom: var(--space-2);
      }
      .gateway-health-status {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex-wrap: wrap;
      }
      .gateway-health-hint {
        font-size: 0.8125rem;
      }
      .gateway-dot-unreachable {
        background: var(--text-muted);
      }
      .gateway-health-btn {
        height: 28px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
        white-space: nowrap;
      }
    `,
  ],
})
export class SettingsComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly fb = inject(FormBuilder);
  private readonly router = inject(Router);
  private readonly destroyRef = inject(DestroyRef);
  private readonly theme = inject(ThemeService);
  private readonly updates = inject(UpdateService);

  // ── About — product identity + shared update-check state ────────────────

  /** Static product identity (name/version/description), loaded once in ngOnInit. */
  readonly appInfo = signal<AppInfo | null>(null);

  /** Update-check lifecycle from the shared service (drives the button + result line). */
  readonly updateStatus = this.updates.status;

  /** The most-recent update-check result (the Download button reads its releaseUrl). */
  readonly latestUpdate = this.updates.latest;

  /** Run the shared manual update check (surfaces both outcomes as toasts). */
  checkForUpdates(): void {
    void this.updates.checkManually();
  }

  /** Open the GitHub release page for an available update. */
  downloadUpdate(url: string): void {
    void this.ipc.openReleasePage(url);
  }

  /** Current theme choice (Light / Dark / System) — drives the Appearance control. */
  readonly themeMode = this.theme.mode;

  /** Apply a theme immediately (persisted in the service; no save() needed). */
  setTheme(mode: ThemeMode): void {
    this.theme.setMode(mode);
  }

  // ── macOS-style navigation: sidebar sections + search ───────────────────

  /** The sidebar sections (icons rendered in the template by id). */
  readonly sections = SETTINGS_SECTIONS;

  /** The currently-shown section (right pane). Defaults to Appearance. */
  readonly activeSection = signal<string>(SETTINGS_SECTIONS[0].id);

  /** Search box for filtering the sidebar section list. Not part of the config form. */
  readonly searchControl = new FormControl("", { nonNullable: true });

  /** Live signal of the (raw) search text, seeded so `computed`s track it. */
  private readonly _search = toSignal(
    this.searchControl.valueChanges.pipe(startWith("")),
    { initialValue: "" },
  );

  /** Trimmed query — shown in the "no results" message. */
  readonly searchQuery = computed(() => this._search().trim());

  /**
   * Sections that match the search query (by label + keywords). With no query,
   * every section is shown. Filtering the sidebar only — the visible content
   * pane is driven by `activeSection` and is never changed by a search.
   */
  readonly visibleSections = computed(() => {
    const q = this.searchQuery().toLowerCase();
    if (!q) return this.sections;
    return this.sections.filter((s) =>
      (s.label + " " + s.keywords).toLowerCase().includes(q),
    );
  });

  /** Human label for the active section (right-pane header). */
  readonly activeSectionLabel = computed(
    () =>
      this.sections.find((s) => s.id === this.activeSection())?.label ?? "",
  );

  /** Switch the visible section (sidebar click). */
  selectSection(id: string): void {
    this.activeSection.set(id);
  }

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
    // Opt-in: pass the shell env to the `claude` CLI (restores env ANTHROPIC_API_KEY auth).
    claudeCodeInheritEnv: false,
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
    // Proactive brain (P2) — zero-egress recall cards while recording; default ON.
    proactiveHintsEnabled: true,
    /** Custom GGUF model path (or registry id). Empty → null on save. */
    brainModelId: "",
    // brain2 RAG — semantic-search master flag (round-tripped on save).
    semanticSearchEnabled: false,
    // brain2 connectors — web-search master toggle (NEW EGRESS; round-tripped).
    webSearchEnabled: false,
    // AI Gateway (Phase 1) — base URL and model, round-tripped on save.
    gatewayBaseUrl: "",
    gatewayModel: "",
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

  /** 0..1 download progress for the in-flight Whisper model (best-effort from events). */
  readonly modelDownloadFrac = signal(0);
  /** Whole-percent label for the in-flight Whisper-model download. */
  readonly modelPct = computed(
    () => Math.round(this.modelDownloadFrac() * 100) + "%",
  );
  /** Release handle for the EVENT_MODEL_DOWNLOAD subscription. */
  private unlistenModelDownload: UnlistenFn | null = null;

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

  // ── AI Gateway (Phase 1) — key management + destination computed signals ──

  /** Gateway API key input. Cleared after save; value never sent back. */
  readonly gatewayKeyControl = new FormControl("", { nonNullable: true });
  /** Whether a gateway API key is currently stored (has-key probe; never the value). */
  readonly hasGatewayKey = signal(false);
  /** Surfaced if storing or clearing the gateway key rejects. */
  readonly gatewayKeyError = signal<string | null>(null);

  // ── AI Gateway (Phase 3) — live model picker ────────────────────────────

  /** Models fetched from the gateway's `/v1/models` endpoint. Empty = use text fallback. */
  readonly gatewayModels = signal<GatewayModel[]>([]);
  /** True while list_gateway_models is in-flight — disables the Refresh button. */
  readonly gatewayModelsLoading = signal(false);
  /** Non-null when the last refreshGatewayModels() call failed — surfaces a fallback hint. */
  readonly gatewayModelError = signal<string | null>(null);

  // ── AI Gateway (Phase 4) — health probe ─────────────────────────────────

  /** Last health-probe result; null = not yet checked. */
  readonly gatewayHealth = signal<GatewayHealth | null>(null);
  /** True while gateway_health is in-flight — disables the Check button. */
  readonly gatewayHealthChecking = signal(false);

  /**
   * Live signal of the gatewayModel form control's value. Mirrors the pattern used
   * for `_gatewayBaseUrlValue` below so `gatewayModelIsCustom` is reactive.
   */
  private readonly _gatewayModelValue = toSignal(
    this.form.controls.gatewayModel.valueChanges.pipe(startWith("")),
    { initialValue: "" },
  );

  /**
   * True when a model is currently saved in the form AND that model is NOT present
   * in the fetched `gatewayModels` catalog. In that case the template adds it as a
   * "(custom)" option so the manually-typed value is never silently lost.
   *
   * Implemented as a `computed` rather than an inline arrow function in the template
   * to satisfy the Angular template parser (arrow functions are banned in expressions).
   */
  readonly gatewayModelIsCustom = computed(() => {
    const current = this._gatewayModelValue();
    if (!current) return false;
    return !this.gatewayModels().some((m) => m.id === current);
  });

  /**
   * Live signal of the gatewayBaseUrl form control's value — built from
   * `valueChanges` so computed() signals can track it reactively. `startWith`
   * seeds the initial value (the form control starts as `""`).
   */
  private readonly _gatewayBaseUrlValue = toSignal(
    // valueChanges not available until the form is fully constructed, but because
    // this field initialiser runs after the `form` field above, the form group
    // (and its controls) already exist at this point.
    this.form.controls.gatewayBaseUrl.valueChanges.pipe(startWith("")),
    { initialValue: "" },
  );

  /**
   * Computed URL validation warning: true when the URL is non-empty AND is not a
   * valid https:// URL AND is not an http:// loopback (localhost / 127.0.0.1 / [::1]).
   * Derived from `_gatewayBaseUrlValue` so it updates on every keystroke.
   */
  readonly gatewayUrlWarning = computed(() => {
    const url = this._gatewayBaseUrlValue();
    if (!url) return false;
    if (url.startsWith("https://")) return false;
    if (/^http:\/\/(localhost|127\.0\.0\.1|\[::1\])(:\d+)?(\/|$)/i.test(url))
      return false;
    return true;
  });

  /**
   * Computed destination info from the gateway base URL:
   * - `null` when the URL is empty or unparseable (no banner shown)
   * - `{ isRemote: true, host }` for https:// non-loopback → shows the warning banner
   * - `{ isRemote: false, host }` for loopback http:// → shows the calmer note
   */
  readonly gatewayDestination = computed((): { isRemote: boolean; host: string } | null => {
    const url = this._gatewayBaseUrlValue();
    if (!url) return null;
    try {
      const parsed = new URL(url);
      const host = parsed.hostname;
      const isLoopback =
        host === "localhost" ||
        host === "127.0.0.1" ||
        host === "[::1]" ||
        host === "::1";
      return { isRemote: !isLoopback, host: parsed.host };
    } catch {
      return null;
    }
  });

  /**
   * Live signals of the providerId / ollamaBaseUrl form controls — same
   * `valueChanges` bridge as `_gatewayBaseUrlValue` above, seeded with the
   * form defaults so `providerIsCloud` is correct before the config loads.
   */
  private readonly _providerIdValue = toSignal(
    this.form.controls.providerId.valueChanges.pipe(startWith("claude_code")),
    { initialValue: "claude_code" },
  );
  private readonly _ollamaBaseUrlValue = toSignal(
    this.form.controls.ollamaBaseUrl.valueChanges.pipe(
      startWith("http://localhost:11434"),
    ),
    { initialValue: "http://localhost:11434" },
  );

  /**
   * FE mirror of the backend's egress classification (`egress_is_cloud`,
   * summarize/mod.rs): claude_code / anthropic / gateway always send content
   * off-device (gateway even on loopback — it can forward to the cloud);
   * ollama is local ONLY when its base URL host is loopback; anything
   * unknown or unparseable fails safe as cloud. Reuse this wherever the FE
   * decides "is this cloud" so the two classifications can't diverge.
   */
  readonly providerIsCloud = computed(() => {
    const id = this._providerIdValue();
    if (id === "ollama") {
      try {
        const host = new URL(this._ollamaBaseUrlValue()).hostname;
        return !(
          host === "localhost" ||
          host === "127.0.0.1" ||
          host === "[::1]" ||
          host === "::1"
        );
      } catch {
        return true; // unparseable → fail safe (treat as cloud)
      }
    }
    return true; // claude_code | anthropic | gateway | any future id
  });

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
        claudeCodeInheritEnv: cfg.claudeCodeInheritEnv ?? false,
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
        proactiveHintsEnabled: cfg.proactiveHintsEnabled ?? true,
        brainModelId: cfg.brainModelId ?? "",
        semanticSearchEnabled: cfg.semanticSearchEnabled ?? false,
        webSearchEnabled: cfg.webSearchEnabled ?? false,
        // AI Gateway (Phase 1) — base URL + model, default "" for pre-existing configs.
        gatewayBaseUrl: cfg.gatewayBaseUrl ?? "",
        gatewayModel: cfg.gatewayModel ?? "",
      });
      this.updateDownloadHint();
      this.inputDevices.set(await this.ipc.listInputDevices().catch(() => []));
      this.hasKey.set(await this.ipc.hasAnthropicKey());
      this.hasWebKey.set(await this.ipc.hasWebSearchKey().catch(() => false));
      this.hasGatewayKey.set(await this.ipc.hasGatewayKey().catch(() => false));
      this.modelPresent.set(await this.ipc.modelPresent());
      // Whisper transcribe-model download-progress stream (best-effort).
      await this.subscribeModelDownload();
      await this.refreshProviders();
      // Phase H — brain model registry + download-progress stream (best-effort).
      await this.subscribeBrainDownload();
      await this.refreshBrainModels();
      // brain2 RAG — embedding-model presence + reindex/download progress streams.
      await this.subscribeSemanticStreams();
      this.embedModelPresent.set(
        await this.ipc.embedModelPresent().catch(() => false),
      );
      // About section — product identity (best-effort; null leaves a "loading" line).
      this.appInfo.set(await this.ipc.appInfo().catch(() => null));
    } catch (e) {
      this.loadError.set(String(e));
    }
  }

  /**
   * Subscribe ONCE to the Whisper model-download progress stream and store the
   * unlisten so DestroyRef can release it (no leaked listener). Best-effort: a
   * missing backend stream just leaves the progress bar inert (the download still
   * resolves via the command promise).
   */
  private async subscribeModelDownload(): Promise<void> {
    try {
      this.unlistenModelDownload = await this.ipc.onModelDownload((p) => {
        // Only meaningful while a download this component started is in-flight.
        if (!this.downloadingModel()) return;
        if (p.total && p.total > 0) {
          this.modelDownloadFrac.set(Math.min(1, p.downloaded / p.total));
        }
        if (p.done) this.modelDownloadFrac.set(1);
      });
      this.destroyRef.onDestroy(() => this.unlistenModelDownload?.());
    } catch {
      // No model-download stream available — progress stays inert.
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
      // Proactive brain hints — round-tripped so a save preserves the mute.
      proactiveHintsEnabled: v.proactiveHintsEnabled,
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
      // Opt-in: pass the shell env to the `claude` CLI (restores env ANTHROPIC_API_KEY auth).
      claudeCodeInheritEnv: v.claudeCodeInheritEnv,
      // AI Gateway (Phase 1) — base URL + model, round-tripped so a settings save preserves them.
      gatewayBaseUrl: v.gatewayBaseUrl,
      gatewayModel: v.gatewayModel,
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

  /**
   * AI Gateway (Phase 1) — store/replace the gateway API key in Keychain, then
   * re-probe presence so the pill flips. The value is cleared from the input after
   * saving (it's never shown back). Mirrors saveKey() / saveWebKey().
   */
  async saveGatewayKey(): Promise<void> {
    const key = this.gatewayKeyControl.value;
    if (!key.trim()) return;
    this.gatewayKeyError.set(null);
    try {
      await this.ipc.setGatewayKey(key);
      this.gatewayKeyControl.setValue("");
      this.hasGatewayKey.set(await this.ipc.hasGatewayKey());
    } catch (e) {
      this.gatewayKeyError.set(String(e));
    }
  }

  /**
   * AI Gateway (Phase 1) — remove the stored gateway API key from Keychain.
   * Updates the pill afterward. No-op when no key is stored.
   */
  async removeGatewayKey(): Promise<void> {
    this.gatewayKeyError.set(null);
    try {
      await this.ipc.clearGatewayKey();
      this.hasGatewayKey.set(await this.ipc.hasGatewayKey());
    } catch (e) {
      this.gatewayKeyError.set(String(e));
    }
  }

  /**
   * AI Gateway (Phase 3) — fetch the model catalog from the configured gateway's
   * `/v1/models` endpoint and populate the model picker. Leaves the list empty on
   * error so the text-input fallback is shown instead — the user can still type the
   * model id manually. Not an effect: driven by the explicit "↻ Refresh models"
   * button click (no NG0600 risk, no unwanted network call on load).
   */
  async refreshGatewayModels(): Promise<void> {
    this.gatewayModelError.set(null);
    this.gatewayModelsLoading.set(true);
    try {
      this.gatewayModels.set(await this.ipc.listGatewayModels());
    } catch (e) {
      // Leave the existing list (may be empty) and show the fallback hint.
      this.gatewayModels.set([]);
      this.gatewayModelError.set(String(e));
    } finally {
      this.gatewayModelsLoading.set(false);
    }
  }

  /**
   * AI Gateway (Phase 4) — probe the configured gateway and update the health
   * indicator. Driven by the explicit "Check" button click (no NG0600 risk, no
   * unwanted network call on load). The backend never errors on this command but
   * we catch for safety.
   */
  async checkGatewayHealth(): Promise<void> {
    this.gatewayHealthChecking.set(true);
    try {
      this.gatewayHealth.set(
        await this.ipc
          .gatewayHealth()
          .catch(() => ({ reachable: false, modelCount: 0 })),
      );
    } finally {
      this.gatewayHealthChecking.set(false);
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
    this.modelDownloadFrac.set(0);
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
