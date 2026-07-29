import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  effect,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../../../settings.store";

/** Provider-backed connection ids (a per-role model select makes sense on these). */
const PROVIDER_CONNECTION_IDS: readonly string[] = [
  "claude_code",
  "codex_cli",
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
  /** Whether to offer the "Off — retrieval only" target (Ask/Live only). Notes can't:
   *  "off" builds no SummarizerProvider so provider_for refuses it. All roles offer "local". */
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
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  templateUrl: "./ai-role-rows.component.html",
  styleUrl: "./ai-role-rows.component.scss",
})
export class AiRoleRowsComponent {
  private readonly store = inject(SettingsStore);
  private readonly injector = inject(Injector);

  readonly form = this.store.form;

  /** Disclosure state — collapsed by default (overrides are the power path). */
  readonly expanded = signal(false);

  /** The `.role-rows` container — the anchor for the map's "Change" scroll. */
  private readonly rolesContainer =
    viewChild<ElementRef<HTMLElement>>("rolesContainer");

  toggleExpanded(): void {
    this.expanded.update((v) => !v);
  }

  /**
   * When the map's "Change" asks for a role (store.highlightRole()), open the
   * disclosure and, after the row renders, scroll it into view + flash it, then
   * clear the request. The synchronous `expanded` write is fine (signal writes
   * in effects are allowed since Angular 19; the store clear runs later, inside
   * afterNextRender, outside this effect's reactive context). The store's
   * null-then-set makes a repeat Change on the same row re-fire this effect.
   */
  private readonly _highlight = effect(
    () => {
      const role = this.store.highlightRole();
      if (!role) return;
      this.expanded.set(true);
      afterNextRender(
        () => {
          const el = this.rolesContainer()?.nativeElement.querySelector<HTMLElement>(
            `[data-role="${role}"]`,
          );
          if (el) {
            el.scrollIntoView({ behavior: "smooth", block: "center" });
            // Restart the flash even if the class is already present.
            el.classList.remove("hl");
            void el.offsetWidth; // force reflow
            el.classList.add("hl");
          }
          this.store.clearHighlightRole();
        },
        { injector: this.injector },
      );
    },
  );

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
  readonly rows = computed<RoleRowVm[]>(() => {
    const all: RoleRowVm[] = [
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
    ];
    // "Live during meetings" (@brain threads + the voice assistant) is HIDDEN under the Cloud posture:
    // the voice assistant can't run there (it needs the on-device light engine, which Cloud turns off),
    // and @brain threads just inherit the cloud default — so a per-feature override is moot. Shown in
    // Hybrid / Fully local / Custom.
    return this.store.posture() === "cloud"
      ? all.filter((r) => r.role !== "live")
      : all;
  });

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
