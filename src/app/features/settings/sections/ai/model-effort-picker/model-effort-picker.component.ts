import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  input,
  signal,
} from "@angular/core";
import type { BrainModelDto } from "../../../../../core/models";
import { SettingsStore } from "../../../settings.store";

/** The two on-device model FAMILIES, split by model name (Bielik = Polish-native). */
type Family = "multi" | "pl";

/**
 * AI & Models → the Claude-style on-device MODEL picker (replaces the flat
 * "Local models" list + the on-device `<select>` dropdowns). Instead of a bare
 * catalog, the user picks:
 *
 *   - a LANGUAGE family (🌍 Multilingual / 🇵🇱 Polish-native), and
 *   - an EFFORT level via a slider over that family's HEAVY models
 *     (⚡ Faster · lighter … Smarter · heavier 🧠) — bigger = smarter, slower,
 *     more RAM.
 *
 * Everything is derived DYNAMICALLY from `store.brainModels()` (no hardcoded
 * ids). The slider selects among a family's HEAVY models (Notes & Ask brain);
 * the LIGHT (realtime-reactions) model is automatic — the family's single
 * smallest light model. Both slots are set through the ONE existing store method
 * `useBrainModel(id)` (it routes by the model's registry class inside
 * `select_brain_model`, so a heavy select fills the heavy slot and a light
 * select fills the light slot). No new IPC, no new config key — backend
 * untouched.
 *
 * `reactionsOnly` (Hybrid posture) collapses the UI to JUST the light model:
 * the language toggle + a resolved light-model card, no effort slider / RAM
 * heavy-warn / custom GGUF.
 *
 * In-flow card content, not an overlay (T3). Zoneless: all state is signals;
 * the slider / toggle handlers are template events (they may write the store
 * signals freely — no `effect()`, so no NG0600 flag needed). The range input
 * binds a NUMERIC `[value]="heavyIdx()"` and reads `$event.target.value` in the
 * handler (the reliable native-value pattern — a keyed-`@for` `[value]` select
 * mis-shows the first option).
 */
@Component({
  selector: "app-model-effort-picker",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [],
  templateUrl: "./model-effort-picker.component.html",
  styleUrl: "./model-effort-picker.component.scss",
})
export class ModelEffortPickerComponent {
  private readonly store = inject(SettingsStore);

  /** Collapse to just the LIGHT realtime-reactions model (Hybrid posture). */
  readonly reactionsOnly = input<boolean>(false);

  // ── store wires ────────────────────────────────────────────────────────
  readonly brainDownloadingId = this.store.brainDownloadingId;
  readonly brainDownloadFrac = this.store.brainDownloadFrac;
  readonly brainPct = this.store.brainPct;
  readonly brainError = this.store.brainError;
  readonly customGgufValue = this.store.customGgufValue;

  /**
   * The family the user explicitly toggled to THIS session — wins over the
   * derived-from-selection family. Needed because switching to a family whose
   * models aren't downloaded yet can't actually change the backend selection
   * (nothing gets selected), so without this override the toggle would visibly
   * snap back to the still-selected family.
   */
  private readonly _familyOverride = signal<Family | null>(null);

  /**
   * The slider position while it points at a model that isn't downloaded yet
   * (can't be selected in the backend, so the derived-from-selection index
   * wouldn't reflect the drag). Also keeps the thumb where the user dragged
   * during the async select round-trip. Reset on a family toggle.
   */
  private readonly _pendingHeavyIdx = signal<number | null>(null);

  /** Whether the custom-GGUF disclosure is open (full mode only). */
  readonly ggufOpen = signal(false);

  /**
   * The active language family. The user's session override wins; otherwise it
   * is derived from the EFFECTIVE per-class model — the light in reactions-only
   * mode, else the heavy (`selectedLight`/`selectedHeavy` reflect the true
   * `brain_light_model_id`/`brain_heavy_model_id`, or the registry default when
   * unset, so this is honest regardless of which class was picked LAST).
   * Defaults to Multilingual.
   */
  readonly family = computed<Family>(() => {
    const override = this._familyOverride();
    if (override) return override;
    const models = this.store.brainModels();
    const sel = this.reactionsOnly()
      ? models.find((m) => m.selectedLight)
      : models.find((m) => m.selectedHeavy);
    if (sel) return this.isPolish(sel) ? "pl" : "multi";
    return "multi";
  });

  /** "Multilingual" / "Polish-native" for the resolved-card meta line. */
  readonly familyLabel = computed(() =>
    this.family() === "pl" ? "Polish-native" : "Multilingual",
  );

  /** This family's HEAVY models, smallest → largest (the slider's stops). */
  readonly heavyModels = computed(() => this.heavyOfFamily(this.family()));

  /** This family's single smallest LIGHT model (realtime reactions), or null. */
  readonly lightModel = computed(() => this.lightOfFamily(this.family()));

  /**
   * The slider index: the pending drag position when set (and in range),
   * otherwise the index of the EFFECTIVE heavy model (`selectedHeavy` mirrors
   * `brain_heavy_model_id`, or the registry default) within this family — so a
   * pre-existing heavy config surfaces at its true position, not "smallest".
   * Falls to 0 only when the effective heavy is in a DIFFERENT family than the
   * one just toggled to (its models aren't the ones selected).
   */
  readonly heavyIdx = computed(() => {
    const models = this.heavyModels();
    if (models.length === 0) return 0;
    const pending = this._pendingHeavyIdx();
    if (pending !== null && pending >= 0 && pending < models.length)
      return pending;
    const selIdx = models.findIndex((m) => m.selectedHeavy);
    return selIdx >= 0 ? selIdx : 0;
  });

  /** The resolved heavy model the slider points at (for the card). */
  readonly selectedHeavy = computed<BrainModelDto | null>(
    () => this.heavyModels()[this.heavyIdx()] ?? null,
  );
  /** The resolved light model (for the reactions-only card). */
  readonly selectedLight = computed<BrainModelDto | null>(() =>
    this.lightModel(),
  );

  /** Accent-fill percentage left of the thumb (0..100). */
  readonly fillPct = computed(() => {
    const max = this.heavyModels().length - 1;
    if (max <= 0) return 0;
    return (this.heavyIdx() / max) * 100;
  });

  /** A relative effort word for the slider header (positional). */
  readonly effortLabel = computed(() => {
    const n = this.heavyModels().length;
    if (n <= 1) return "";
    const i = this.heavyIdx();
    if (i === 0) return "Faster · lighter";
    if (i === n - 1) return "Smarter · heavier";
    return "Balanced";
  });

  /**
   * Toggle the language family. Reflects the choice immediately (override), then
   * selects the family's smallest HEAVY (skipped in reactions-only mode) AND its
   * LIGHT model — both through `useBrainModel` (routes by class in the backend).
   * A target that isn't downloaded is left unselected: the resolved card then
   * shows a Download button (mirrors the existing pickers — no silent download).
   */
  async setFamily(f: Family): Promise<void> {
    this._familyOverride.set(f);
    this._pendingHeavyIdx.set(null);
    if (!this.reactionsOnly()) {
      const heavy = this.heavyOfFamily(f)[0] ?? null;
      if (heavy?.downloaded) await this.store.useBrainModel(heavy.id);
    }
    const light = this.lightOfFamily(f);
    if (light?.downloaded) await this.store.useBrainModel(light.id);
  }

  /**
   * Drag the effort slider. Keeps the thumb where dragged (pending index) and,
   * when the target is already downloaded, makes it the active heavy model. A
   * not-yet-downloaded target stays as a pending position so the resolved card
   * offers a Download for it.
   */
  onSlide(e: Event): void {
    const idx = Number((e.target as HTMLInputElement).value);
    this._pendingHeavyIdx.set(idx);
    const target = this.heavyModels()[idx];
    if (target?.downloaded) void this.store.useBrainModel(target.id);
  }

  /**
   * Download a not-yet-present model, then make it the active model (the slider
   * position / language choice IS the selection intent). `downloadBrainModel`
   * refreshes the registry, so `downloaded` is fresh before we select.
   */
  async downloadAndUse(id: string): Promise<void> {
    if (!id) return;
    await this.store.downloadBrainModel(id);
    if (this.store.brainModels().find((m) => m.id === id)?.downloaded) {
      await this.store.useBrainModel(id);
    }
  }

  toggleGguf(): void {
    this.ggufOpen.update((v) => !v);
  }

  /** Route a typed custom GGUF value to the right store control. */
  setCustomGguf(v: string): void {
    this.store.setCustomGguf(v);
  }

  /** Human "1.1 GB" / "620 MB" size label from a byte count (binary). */
  sizeLabel(bytes: number): string {
    const gb = 1024 * 1024 * 1024;
    return bytes >= gb
      ? (bytes / gb).toFixed(1) + " GB"
      : Math.round(bytes / (1024 * 1024)) + " MB";
  }

  /** Family split by NAME (Bielik = Polish-native), no hardcoded ids. */
  private isPolish(m: BrainModelDto): boolean {
    return m.name.toLowerCase().includes("bielik");
  }

  /** A family's HEAVY models, smallest → largest. */
  private heavyOfFamily(f: Family): BrainModelDto[] {
    return this.store
      .brainModels()
      .filter((m) => m.class === "heavy" && this.matchesFamily(m, f))
      .sort((a, b) => a.approxSizeBytes - b.approxSizeBytes);
  }

  /** A family's single smallest LIGHT model, or null. */
  private lightOfFamily(f: Family): BrainModelDto | null {
    return (
      this.store
        .brainModels()
        .filter((m) => m.class === "light" && this.matchesFamily(m, f))
        .sort((a, b) => a.approxSizeBytes - b.approxSizeBytes)[0] ?? null
    );
  }

  private matchesFamily(m: BrainModelDto, f: Family): boolean {
    return (this.isPolish(m) ? "pl" : "multi") === f;
  }
}
