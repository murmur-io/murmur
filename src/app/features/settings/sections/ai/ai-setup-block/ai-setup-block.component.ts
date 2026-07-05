import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
} from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
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
  standalone: true,
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
      { role: "Notes & Ask", model: models.find((m) => m.selectedHeavy) ?? null },
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

  /** Prefetch the newly-picked engine's model catalog (claude_code/anthropic). */
  onEngineChanged(e: Event): void {
    const id = (e.target as HTMLSelectElement).value;
    if (id === "claude_code" || id === "anthropic") {
      void this.store.ensureModels(id);
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
