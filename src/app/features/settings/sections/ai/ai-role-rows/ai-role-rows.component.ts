import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../../../settings.store";
import { LocalModelsListComponent } from "../local-models-list/local-models-list.component";

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
  imports: [ReactiveFormsModule, LocalModelsListComponent],
  templateUrl: "./ai-role-rows.component.html",
  styleUrl: "./ai-role-rows.component.scss",
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
