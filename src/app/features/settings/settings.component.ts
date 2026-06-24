import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  inject,
  signal,
} from "@angular/core";
import {
  FormBuilder,
  FormControl,
  FormGroup,
  ReactiveFormsModule,
} from "@angular/forms";
import { open } from "@tauri-apps/plugin-dialog";
import { IpcService } from "../../core/ipc.service";
import type { AppConfigDto, ProviderStatus } from "../../core/models";

@Component({
  selector: "app-settings",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  template: `
    @if (form; as f) {
      <section class="settings" [formGroup]="f">
        <label>
          Provider
          <select formControlName="providerId">
            <option value="claude_code">Claude Code (default)</option>
            <option value="anthropic">Anthropic API</option>
            <option value="ollama">Ollama</option>
          </select>
        </label>

        <label>
          Vault folder
          <span class="row">
            <input formControlName="vaultPath" placeholder="/path/to/vault" />
            <button type="button" (click)="pickVault()">Browse…</button>
          </span>
        </label>

        <label>
          Vault subfolder
          <input formControlName="vaultSubfolder" placeholder="Meetings" />
        </label>

        <label>
          Whisper model path
          <span class="row">
            <input
              formControlName="whisperModelPath"
              placeholder="/path/to/ggml-model.bin"
            />
            <button type="button" (click)="pickModel()">Browse…</button>
          </span>
        </label>

        <label>
          Language (blank = auto)
          <input formControlName="language" placeholder="en" />
        </label>

        <label>
          Anthropic model
          <input formControlName="anthropicModel" />
        </label>

        <label>
          Ollama base URL
          <input formControlName="ollamaBaseUrl" />
        </label>

        <label>
          Ollama model
          <input formControlName="ollamaModel" />
        </label>

        <label>
          Claude binary
          <input formControlName="claudeBinary" />
        </label>

        <label class="check">
          <input type="checkbox" formControlName="captureSystemAudio" />
          Capture system audio (the other side of the call) — needs the Screen
          Recording permission on first use
        </label>

        <fieldset>
          <legend>Anthropic API key</legend>
          <p>Status: {{ hasKey() ? "set" : "not set" }}</p>
          <span class="row">
            <input
              type="password"
              [formControl]="keyControl"
              placeholder="sk-ant-…"
            />
            <button type="button" (click)="saveKey()">Save key</button>
          </span>
        </fieldset>

        <div class="actions">
          <button type="button" (click)="save()">Save settings</button>
          <button type="button" (click)="refreshProviders()">
            Check providers
          </button>
        </div>

        @if (saved()) {
          <p class="ok">Saved.</p>
        }

        <h3>Provider availability</h3>
        <ul>
          @for (p of providers(); track p.id) {
            <li>
              <strong>{{ p.id }}</strong
              >:
              {{ p.available ? "available" : "unavailable" }}
              @if (p.reason) {
                <span> ({{ p.reason }})</span>
              }
            </li>
          }
        </ul>
      </section>
    } @else {
      <p>Loading settings…</p>
    }
  `,
  styles: [
    `
      .settings {
        max-width: 560px;
        display: flex;
        flex-direction: column;
        gap: 0.75rem;
      }
      label {
        display: flex;
        flex-direction: column;
        gap: 0.25rem;
        font-size: 0.9rem;
      }
      .row {
        display: flex;
        gap: 0.5rem;
      }
      .row input {
        flex: 1;
      }
      .check {
        flex-direction: row;
        align-items: center;
        gap: 0.5rem;
      }
      .ok {
        color: #27ae60;
      }
    `,
  ],
})
export class SettingsComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly fb = inject(FormBuilder);

  /** Reactive form, built once config is loaded (SF-6 — replaces [(ngModel)]). */
  form: FormGroup | null = null;
  readonly keyControl = new FormControl("", { nonNullable: true });

  readonly providers = signal<ProviderStatus[]>([]);
  readonly hasKey = signal(false);
  readonly saved = signal(false);

  async ngOnInit(): Promise<void> {
    const cfg = await this.ipc.getConfig();
    this.form = this.fb.nonNullable.group({
      providerId: cfg.providerId,
      vaultPath: cfg.vaultPath ?? "",
      vaultSubfolder: cfg.vaultSubfolder ?? "",
      whisperModelPath: cfg.whisperModelPath ?? "",
      language: cfg.language ?? "",
      anthropicModel: cfg.anthropicModel,
      ollamaBaseUrl: cfg.ollamaBaseUrl,
      ollamaModel: cfg.ollamaModel,
      claudeBinary: cfg.claudeBinary,
      captureSystemAudio: cfg.captureSystemAudio,
    });
    this.hasKey.set(await this.ipc.hasAnthropicKey());
    await this.refreshProviders();
  }

  async pickVault(): Promise<void> {
    const dir = await open({ directory: true, multiple: false });
    if (typeof dir === "string") this.form?.patchValue({ vaultPath: dir });
  }

  async pickModel(): Promise<void> {
    const file = await open({ directory: false, multiple: false });
    if (typeof file === "string")
      this.form?.patchValue({ whisperModelPath: file });
  }

  async save(): Promise<void> {
    if (!this.form) return;
    const v = this.form.getRawValue();
    // Empty optional fields → null (the Rust side also normalizes, but keep the DTO clean).
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
    };
    await this.ipc.saveConfig(cfg);
    this.saved.set(true);
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
}
