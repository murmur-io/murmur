import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  inject,
  signal,
} from "@angular/core";
import { FormsModule } from "@angular/forms";
import { open } from "@tauri-apps/plugin-dialog";
import { IpcService } from "../../core/ipc.service";
import type { AppConfigDto, ProviderStatus } from "../../core/models";

@Component({
  selector: "app-settings",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule],
  template: `
    @if (config(); as cfg) {
      <section class="settings">
        <label>
          Provider
          <select [(ngModel)]="cfg.providerId">
            <option value="claude_code">Claude Code (default)</option>
            <option value="anthropic">Anthropic API</option>
            <option value="ollama">Ollama</option>
          </select>
        </label>

        <label>
          Vault folder
          <span class="row">
            <input [(ngModel)]="cfg.vaultPath" placeholder="/path/to/vault" />
            <button type="button" (click)="pickVault(cfg)">Browse…</button>
          </span>
        </label>

        <label>
          Vault subfolder
          <input [(ngModel)]="cfg.vaultSubfolder" placeholder="Meetings" />
        </label>

        <label>
          Whisper model path
          <span class="row">
            <input
              [(ngModel)]="cfg.whisperModelPath"
              placeholder="/path/to/ggml-model.bin"
            />
            <button type="button" (click)="pickModel(cfg)">Browse…</button>
          </span>
        </label>

        <label>
          Language (blank = auto)
          <input [(ngModel)]="cfg.language" placeholder="en" />
        </label>

        <label>
          Anthropic model
          <input [(ngModel)]="cfg.anthropicModel" />
        </label>

        <label>
          Ollama base URL
          <input [(ngModel)]="cfg.ollamaBaseUrl" />
        </label>

        <label>
          Ollama model
          <input [(ngModel)]="cfg.ollamaModel" />
        </label>

        <label>
          Claude binary
          <input [(ngModel)]="cfg.claudeBinary" />
        </label>

        <fieldset>
          <legend>Anthropic API key</legend>
          <p>Status: {{ hasKey() ? "set" : "not set" }}</p>
          <span class="row">
            <input
              type="password"
              [(ngModel)]="keyInput"
              placeholder="sk-ant-…"
            />
            <button type="button" (click)="saveKey()">Save key</button>
          </span>
        </fieldset>

        <div class="actions">
          <button type="button" (click)="save(cfg)">Save settings</button>
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
      .ok {
        color: #27ae60;
      }
    `,
  ],
})
export class SettingsComponent implements OnInit {
  private readonly ipc = inject(IpcService);

  readonly config = signal<AppConfigDto | null>(null);
  readonly providers = signal<ProviderStatus[]>([]);
  readonly hasKey = signal(false);
  readonly saved = signal(false);
  keyInput = "";

  async ngOnInit(): Promise<void> {
    this.config.set(await this.ipc.getConfig());
    this.hasKey.set(await this.ipc.hasAnthropicKey());
    await this.refreshProviders();
  }

  async pickVault(cfg: AppConfigDto): Promise<void> {
    const dir = await open({ directory: true, multiple: false });
    if (typeof dir === "string") cfg.vaultPath = dir;
  }

  async pickModel(cfg: AppConfigDto): Promise<void> {
    const file = await open({ directory: false, multiple: false });
    if (typeof file === "string") cfg.whisperModelPath = file;
  }

  async save(cfg: AppConfigDto): Promise<void> {
    await this.ipc.saveConfig(cfg);
    this.saved.set(true);
  }

  async saveKey(): Promise<void> {
    if (!this.keyInput) return;
    await this.ipc.setAnthropicKey(this.keyInput);
    this.keyInput = "";
    this.hasKey.set(await this.ipc.hasAnthropicKey());
  }

  async refreshProviders(): Promise<void> {
    this.providers.set(await this.ipc.providerStatuses());
  }
}
