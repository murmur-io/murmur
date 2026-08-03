import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  signal,
} from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { defaultEngineKeepsModelId } from "../../../model-id";
import { SettingsStore } from "../../../settings.store";

/**
 * AI & Models → "Your setup" (NEW, posture-adaptive) — the second section of
 * the posture-driven redesign (docs/superpowers/specs/2026-07-05-…). Posture
 * picks the lane; this block shows ONLY what that lane needs to configure:
 *
 *   - Cloud            → the Default AI engine card.
 *   - Hybrid           → engine card + the reactions-only on-device picker.
 *   - Fully local      → the full on-device effort/language picker.
 *   - Custom           → engine card + on-device picker + a "Custom mix" note.
 *
 * The Default-engine + Default-model controls are the SAME `providerId` /
 * `providerModel` / `providerEffort` FormControls the old Advanced → Default
 * engine block owned (moved here verbatim, one source of truth — no new second
 * writer of any config key). The on-device model choice is delegated to the
 * self-contained `<app-model-effort-picker>` (Claude-style effort slider +
 * language toggle), which drives the existing store `useBrainModel` /
 * `downloadBrainModel` actions. Backend untouched.
 *
 * Rendering is posture-driven via a `setupCards()` computed → `@for` + inner
 * `@switch` so each card's markup is authored exactly once (no `ng-template` —
 * this codebase has zero, and `@for`/`@switch` keep it DRY). In-flow cards, not
 * overlays (T3).
 */
@Component({
  selector: "app-ai-setup-block",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  templateUrl: "./ai-setup-block.component.html",
  styleUrl: "./ai-setup-block.component.scss",
})
export class AiSetupBlockComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;

  // ── posture / engine wires ────────────────────────────────────────────────
  readonly posture = this.store.posture;
  readonly providerIsCloud = this.store.providerIsCloud;
  readonly defaultModelCatalog = this.store.defaultModelCatalog;

  /**
   * Whether this catalog was fetched from a real endpoint (`ollama`, `gateway`) rather than
   * compiled into the binary. Drives the Refresh affordance: a bundled list cannot be refreshed,
   * so offering the button there was a false promise about how current the list was.
   */
  readonly defaultCatalogIsLive = this.store.defaultCatalogIsLive;
  readonly defaultModelsLoading = this.store.defaultModelsLoading;
  readonly defaultModelIsCustom = this.store.defaultModelIsCustom;

  // ── on-device status wires (the full picker lives under Advanced) ────────────
  readonly brainDownloadingId = this.store.brainDownloadingId;
  readonly brainPct = this.store.brainPct;

  /**
   * Fully-local status lines — the EFFECTIVE on-device models (`selectedHeavy` /
   * `selectedLight` mirror the true `brain_heavy_model_id` / `brain_light_model_id`,
   * else the registry default). A compact read-out; the effort slider + language +
   * GGUF live under Advanced → On this Mac → Configure.
   */
  readonly localLines = computed(() => {
    const models = this.store.brainModels();
    return [
      {
        role: "Notes & Ask",
        model: models.find((m) => m.selectedHeavy) ?? null,
      },
      {
        role: "Live reactions",
        model: models.find((m) => m.selectedLight) ?? null,
      },
    ];
  });

  /** Hybrid status — only the realtime-reactions (light) model runs on-device. */
  readonly reactionsLines = computed(() => [
    {
      role: "Live reactions",
      model: this.store.brainModels().find((m) => m.selectedLight) ?? null,
    },
  ]);

  /**
   * Which cards this posture shows, in order — the `@for`+`@switch` driver.
   * Cloud=engine; Hybrid=engine+reactions; Fully local=local; Custom=both+note.
   * Null (pre-load) → no cards (the map card below owns the loading state).
   */
  readonly setupCards = computed<readonly string[]>(() => {
    switch (this.posture()) {
      case "cloud":
        return ["engine"];
      case "hybrid":
        return ["engine", "reactions"];
      case "fully_local":
        return ["local"];
      case "custom":
        return ["engine", "local", "custom-note"];
      default:
        return [];
    }
  });

  /**
   * The current model is not listed by the newly-selected engine.
   *
   * Set instead of clearing the value: the UI explains the mismatch and the user decides. Blanking
   * it was the second half of the same defect as `repairForeignRoleModels` — a catalog treated as
   * authoritative silently destroying a choice it did not recognise — and it defeated the whole
   * point of making an unlisted id typeable.
   */
  readonly unlistedAfterEngineSwitch = signal<string>("");

  /**
   * An id the newly-selected engine cannot send at all, cleared here rather than left for the
   * backend to drop on save. Distinct from `unlistedAfterEngineSwitch`, which keeps a valid id the
   * catalog merely does not list.
   */
  readonly droppedAfterEngineSwitch = signal<string>("");

  /**
   * A TYPED id the persistence boundary will refuse, flagged live rather than at save time.
   *
   * `onEngineChanged` only sees the value at the moment the engine changes, but the free-text id is
   * always available now — so a user can type `-m` or `./model` into a settled form and have it
   * silently dropped by `dto_to_config` on autosave. This computed reacts to the value itself, so
   * the field says so while the value is still on screen.
   */
  readonly typedModelWillBeRefused = computed(
    () =>
      !defaultEngineKeepsModelId(
        this.store.providerModelValue() ?? "",
        this.store.providerIdValue() ?? "",
      ),
  );

  /** Prefetch the newly-picked engine's model catalog and flag (never erase) an unlisted id. */
  async onEngineChanged(e: Event): Promise<void> {
    const id = (e.target as HTMLSelectElement).value;
    this.unlistedAfterEngineSwitch.set("");
    this.droppedAfterEngineSwitch.set("");
    // BEFORE the await. The engine FormControl has already changed, so the store's autosave can
    // reach `dto_to_config` while a catalog fetch is still in flight — and that boundary drops an
    // over-long id for a CLI engine. Clearing it only after the await left a window where the
    // backend persisted empty while the form still showed the old value and the UI said it had
    // been kept. An id this engine cannot SEND is a different case from one the catalog merely
    // does not list, and it needs no catalog to detect.
    const current = this.form.controls.providerModel.value;
    if (current && !defaultEngineKeepsModelId(current, id)) {
      this.form.controls.providerModel.setValue("");
      this.droppedAfterEngineSwitch.set(current);
      // DO NOT RETURN HERE. Clearing the old id says nothing about the NEW engine's catalog, and
      // returning meant a first switch away from an unusable id left the engine with no options
      // loaded and no "this list ships with the app" provenance — the user was told what was
      // removed and then shown an empty picker with no explanation of why it was empty.
    }
    if (id === "claude_code" || id === "codex_cli" || id === "anthropic") {
      const catalogLoaded = await this.store.ensureModels(id);
      if (this.form.controls.providerId.value !== id) return;
      // A transport/IPC failure proves nothing about the id, so say nothing.
      if (!catalogLoaded) return;
      const model = this.form.controls.providerModel.value;
      const catalog = this.store.modelCatalogs()[id]?.options ?? [];
      if (model && !catalog.some((o) => o.id === model)) {
        // A bundled catalog is a hint, so "not listed" does not mean "not valid" — it commonly
        // just means the model shipped after this build. Keep the id and explain.
        this.unlistedAfterEngineSwitch.set(model);
      }
    }
  }

  /** Re-fetch the Default-model catalog for the current provider. */
  refreshDefaultModels(): void {
    void this.store.refreshModels(this.form.controls.providerId.value);
  }

  /** Open the Advanced disclosure (keys/URLs/Test + the on-device picker live there). */
  expandAdvanced(): void {
    this.store.expandAdvanced();
  }

  /** Download an on-device model that isn't present yet (posture usually pre-fetches it). */
  download(id: string): void {
    if (id) void this.store.downloadBrainModel(id);
  }

  /** Human "1.1 GB" / "620 MB" size label from a byte count (binary). */
  sizeLabel(bytes: number): string {
    const gb = 1024 * 1024 * 1024;
    return bytes >= gb
      ? (bytes / gb).toFixed(1) + " GB"
      : Math.round(bytes / (1024 * 1024)) + " MB";
  }
}
