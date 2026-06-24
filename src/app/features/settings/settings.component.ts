import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  inject,
  signal,
} from "@angular/core";
import { FormBuilder, FormControl, ReactiveFormsModule } from "@angular/forms";
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
    </section>
  `,
  styles: [
    `
      .settings {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
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
    `,
  ],
})
export class SettingsComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly fb = inject(FormBuilder);

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
  });
  readonly keyControl = new FormControl("", { nonNullable: true });

  readonly providers = signal<ProviderStatus[]>([]);
  readonly hasKey = signal(false);
  readonly saved = signal(false);
  readonly loadError = signal<string | null>(null);

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

  async ngOnInit(): Promise<void> {
    try {
      const cfg = await this.ipc.getConfig();
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
