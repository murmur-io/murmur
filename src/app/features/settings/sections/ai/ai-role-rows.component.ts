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
 * global GGUF registry (owns `brainModelId`) now lives under Engines →
 * Murmur Brain → Configure, not in these rows — it is shared, not per-role.
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
                @if (row.offersReasonerTargets) {
                  <optgroup label="Built-in (on this Mac)">
                    <option value="local">Murmur Brain — on-device</option>
                    <option value="off">Off — retrieval only</option>
                  </optgroup>
                }
                <optgroup label="Your engines">
                  <option value="claude_code">Claude Code</option>
                  <option value="anthropic">Anthropic API</option>
                  <option value="ollama">Ollama</option>
                  <option value="gateway">Kong AI Gateway (OpenAI-compatible)</option>
                </optgroup>
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
            local default for every role). It now lives under Engines → Murmur
            Brain → Configure (Task 5); the row just points there.
          -->
          @if (anyLocal()) {
            <p class="field-help text-muted">
              On-device models are managed under Engines above (Murmur Brain →
              Configure).
            </p>
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

    `,
  ],
})
export class AiRoleRowsComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;

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

}
