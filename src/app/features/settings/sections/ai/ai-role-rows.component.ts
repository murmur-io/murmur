import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../../settings.store";

/** Provider-backed connection ids (a per-role model select makes sense on these). */
const PROVIDER_CONNECTION_IDS: readonly string[] = [
  "claude_code",
  "anthropic",
  "ollama",
  "gateway",
];

/** One rendered override row — everything the template needs, derived once. */
interface RoleRowVm {
  readonly role: "notes" | "ask" | "live";
  readonly title: string;
  readonly what: string;
  readonly connCtrl: string;
  readonly modelCtrl: string;
  readonly effortCtrl: string;
  /** Ask/Live only — the Notes row must NOT offer Local/Off (see template comment). */
  readonly offersReasonerTargets: boolean;
  readonly conn: string;
  readonly isProviderConn: boolean;
  readonly models: readonly string[];
  readonly modelsLoading: boolean;
  readonly modelValue: string;
  readonly modelIsCustom: boolean;
  readonly inheritSummary: string;
}

/**
 * AI & Models → the "Customize per feature" progressive-disclosure block
 * (Stage 4): three per-role override rows — Meeting notes / Ask & assistant /
 * Live during meetings — each writing its `role{Notes,Ask,Live}{Connection,
 * Model,Effort}` keys ("" = Inherit → the row shows a resolver-mirror summary
 * instead). The Ask row is the successor of the old "Assistant backend"
 * select (its Local/Off targets live here now, and changing it compat-writes
 * the legacy `brainBackend` — see SettingsStore.setRoleConnection). The
 * global GGUF registry block (owns `brainModelId`) renders below the rows
 * whenever Ask or Live picks "Local model" — it is shared, not per-role.
 */
@Component({
  selector: "app-ai-role-rows",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  template: `
    <div class="role-block" [formGroup]="form">
      <button
        type="button"
        class="role-toggle"
        (click)="toggleExpanded()"
        [attr.aria-expanded]="expanded()"
      >
        <svg
          class="role-chevron"
          [class.is-open]="expanded()"
          width="12"
          height="12"
          viewBox="0 0 12 12"
          fill="none"
          aria-hidden="true"
        >
          <path
            d="M4 2l4 4-4 4"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
        Customize per feature
      </button>

      @if (expanded()) {
        <div class="role-rows">
          @for (row of rows(); track row.role) {
            <div class="role-row">
              <div class="role-row-head">
                <span class="role-title">{{ row.title }}</span>
                <span class="role-what text-muted">{{ row.what }}</span>
              </div>

              <!--
                The Notes row deliberately offers NO Local/Off: notes, digests,
                briefs, recipes and the graph are SummarizerProvider surfaces,
                and the backend's provider_for REFUSES reasoner-only targets
                for them (every summary would hard-error) — the lock-security
                carry-over from the stage-3 review. "Local notes" already has
                a first-class answer: Ollama.
              -->
              <select
                [formControlName]="row.connCtrl"
                (change)="onConnectionChange(row.role, $event)"
              >
                <option value="">Inherit default</option>
                <option value="claude_code">Claude Code</option>
                <option value="anthropic">Anthropic API</option>
                <option value="ollama">Ollama</option>
                <option value="gateway">Kong AI Gateway (OpenAI-compatible)</option>
                @if (row.offersReasonerTargets) {
                  <option value="local">Local model — on-device</option>
                  <option value="off">Off — retrieval only</option>
                }
              </select>

              @if (!row.conn) {
                <span class="field-help text-muted">
                  {{ row.inheritSummary }}
                </span>
              }

              @if (row.isProviderConn) {
                <div class="role-model-row">
                  @if (row.models.length > 0) {
                    <select
                      [formControlName]="row.modelCtrl"
                      class="role-model-select"
                    >
                      <option value="">Default (connection's pick)</option>
                      @for (id of row.models; track id) {
                        <option [value]="id">{{ id }}</option>
                      }
                      <!--
                        Keep a manually-typed model selectable when it's not in
                        the catalog — never silently lose it (the gateway
                        picker's keep-manually-typed pattern).
                      -->
                      @if (row.modelIsCustom) {
                        <option [value]="row.modelValue">
                          {{ row.modelValue }} (custom)
                        </option>
                      }
                    </select>
                  } @else {
                    <input
                      [formControlName]="row.modelCtrl"
                      placeholder="Model id (blank = connection default)"
                      autocomplete="off"
                      spellcheck="false"
                      class="role-model-input"
                    />
                  }
                  <button
                    type="button"
                    class="btn btn-ghost role-model-refresh"
                    (click)="refreshModels(row.conn)"
                    [disabled]="row.modelsLoading"
                    title="Fetch this connection's model list"
                  >
                    @if (row.modelsLoading) {
                      Loading…
                    } @else {
                      ↻ Refresh
                    }
                  </button>
                </div>

                @if (row.conn === "anthropic") {
                  <label class="field">
                    <span class="field-label">Reasoning effort</span>
                    <select [formControlName]="row.effortCtrl">
                      <option value="">Default</option>
                      <option value="low">Low</option>
                      <option value="medium">Medium</option>
                      <option value="high">High</option>
                    </select>
                  </label>
                }
              }

              @if (row.conn === "off") {
                <span class="field-help text-muted">
                  @if (row.role === "ask") {
                    Ask answers become retrieval-only (no AI model). The
                    in-meeting voice assistant toggle stays independent.
                  } @else {
                    Live answers become retrieval-only (no AI model). The
                    in-meeting voice assistant toggle stays independent.
                  }
                </span>
              }

              @if (row.conn === "local") {
                @if (row.role === "live") {
                  <span class="field-help text-muted">
                    Big local models are slow for the realtime voice assistant
                    — your default AI is recommended for live answers. Local is
                    best for private, non-time-critical analysis.
                  </span>
                } @else {
                  <span class="field-help text-muted">
                    Runs on-device with the shared local model — pick or
                    download it under Local models below.
                  </span>
                }
              }
            </div>
          }

          <!--
            The GGUF registry is GLOBAL (it owns brainModelId — the resolver's
            local default for every role), so it renders once, shared by every
            row set to "Local model", not per-row.
          -->
          @if (anyLocal()) {
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
                Shared by every feature set to "Local model" — the selected
                model runs fully on this Mac.
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
                              <div
                                class="brain-progress-track"
                                aria-hidden="true"
                              >
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

              <!--
                One shared input driving TWO mutually-exclusive controls
                (brainModelPath for a file path, brainModelId for a registry id),
                so it can't be a formControlName — it's a store-backed
                [value]/(input) pair. A registry pick above clears the path.
              -->
              <label class="field brain-custom">
                <span class="field-label">Custom GGUF model</span>
                <input
                  [value]="customGgufValue()"
                  (input)="setCustomGguf($any($event.target).value)"
                  placeholder="/path/to/model.gguf or a registry id"
                  autocomplete="off"
                  spellcheck="false"
                />
                <span class="field-help text-muted">
                  Point at your own .gguf file, or type a registry id. Saved with
                  your settings.
                </span>
              </label>

              @if (brainError(); as berr) {
                <p class="text-danger brain-error">{{ berr }}</p>
              }
            </div>
          }
        </div>
      }
    </div>
  `,
  styles: [
    `
      :host {
        display: contents;
      }

      .role-block {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }

      /* Progressive-disclosure trigger — a quiet inline text button. */
      .role-toggle {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        align-self: flex-start;
        background: none;
        border: none;
        padding: 0;
        cursor: pointer;
        color: var(--text-secondary);
        font-size: 0.9rem;
        font-weight: 550;
        transition: color var(--transition);
      }
      .role-toggle:hover {
        color: var(--text-primary);
      }
      .role-chevron {
        transition: transform var(--transition);
      }
      .role-chevron.is-open {
        transform: rotate(90deg);
      }

      .role-rows {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .role-row {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        padding: var(--space-3);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
      }
      .role-row-head {
        display: flex;
        align-items: baseline;
        gap: var(--space-2);
        flex-wrap: wrap;
      }
      .role-title {
        color: var(--text-primary);
        font-size: 0.9rem;
        font-weight: 550;
      }
      .role-what {
        font-size: 0.8125rem;
      }

      .role-model-row {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex-wrap: wrap;
      }
      .role-model-select,
      .role-model-input {
        flex: 1 1 220px;
        min-width: 0;
      }
      .role-model-refresh {
        flex: none;
        white-space: nowrap;
      }

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

      /* --- Local GGUF registry (moved verbatim from the defaults block) --- */
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
      .role-block .btn-sm {
        height: 32px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
      }
    `,
  ],
})
export class AiRoleRowsComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;
  /** The shared custom-GGUF input's value (path-or-id) — drives two controls. */
  readonly customGgufValue = this.store.customGgufValue;
  readonly brainModels = this.store.brainModels;
  readonly brainModelsLoading = this.store.brainModelsLoading;
  readonly brainError = this.store.brainError;
  readonly brainDownloadingId = this.store.brainDownloadingId;
  readonly brainDownloadFrac = this.store.brainDownloadFrac;
  readonly brainPct = this.store.brainPct;

  /** Disclosure state — collapsed by default (overrides are the power path). */
  readonly expanded = signal(false);

  toggleExpanded(): void {
    this.expanded.update((v) => !v);
  }

  /**
   * Auto-open once when any override is active (loaded from config or just
   * picked) so an active override is never invisible. A manual collapse sticks
   * — the effect only re-runs when a connection value changes.
   */
  private readonly _autoExpand = effect(
    () => {
      if (
        this.store.roleNotesConnValue() ||
        this.store.roleAskConnValue() ||
        this.store.roleLiveConnValue()
      ) {
        this.expanded.set(true);
      }
    },
    { allowSignalWrites: true },
  );

  /**
   * True when anything runs on the shared local GGUF — explicit role picks OR
   * the legacy `brainBackend=local` fallback (a legacy local install with no
   * role keys still runs Ask/Live fallback + Notes pre-analysis on the local
   * model, and must keep the registry UI to switch/download models).
   */
  readonly anyLocal = computed(
    () =>
      this.store.roleAskConnValue() === "local" ||
      this.store.roleLiveConnValue() === "local" ||
      this.store.brainBackendValue() === "local",
  );

  /** The three override rows, derived from the store's role/catalog signals. */
  readonly rows = computed<RoleRowVm[]>(() => [
    this.buildRow(
      "notes",
      "Meeting notes",
      "Summaries, digests, briefs, recipes, graph",
      "roleNotesConnection",
      "roleNotesModel",
      "roleNotesEffort",
      false,
      this.store.roleNotesConnValue(),
      this.store.roleNotesModelValue(),
      this.store.notesInheritSummary(),
    ),
    this.buildRow(
      "ask",
      "Ask & assistant",
      "Vault Q&A + meeting chat",
      "roleAskConnection",
      "roleAskModel",
      "roleAskEffort",
      true,
      this.store.roleAskConnValue(),
      this.store.roleAskModelValue(),
      this.store.assistantInheritSummary(),
    ),
    this.buildRow(
      "live",
      "Live during meetings",
      "@brain threads + the voice assistant",
      "roleLiveConnection",
      "roleLiveModel",
      "roleLiveEffort",
      true,
      this.store.roleLiveConnValue(),
      this.store.roleLiveModelValue(),
      this.store.assistantInheritSummary(),
    ),
  ]);

  private buildRow(
    role: RoleRowVm["role"],
    title: string,
    what: string,
    connCtrl: string,
    modelCtrl: string,
    effortCtrl: string,
    offersReasonerTargets: boolean,
    conn: string,
    modelValue: string,
    inheritSummary: string,
  ): RoleRowVm {
    const isProviderConn = PROVIDER_CONNECTION_IDS.includes(conn);
    const models = isProviderConn
      ? (this.store.modelCatalogs()[conn] ?? [])
      : [];
    return {
      role,
      title,
      what,
      connCtrl,
      modelCtrl,
      effortCtrl,
      offersReasonerTargets,
      conn,
      isProviderConn,
      models,
      modelsLoading: isProviderConn && this.store.modelsLoading().has(conn),
      modelValue,
      modelIsCustom: !!modelValue && !models.includes(modelValue),
      inheritSummary,
    };
  }

  onConnectionChange(role: RoleRowVm["role"], e: Event): void {
    this.store.setRoleConnection(role, (e.target as HTMLSelectElement).value);
  }

  refreshModels(connection: string): void {
    void this.store.refreshModels(connection);
  }

  refreshBrainModels(): void {
    void this.store.refreshBrainModels();
  }

  useBrainModel(id: string): void {
    void this.store.useBrainModel(id);
  }

  setCustomGguf(v: string): void {
    this.store.setCustomGguf(v);
  }

  downloadBrainModel(id: string): void {
    void this.store.downloadBrainModel(id);
  }
}
