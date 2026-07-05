import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  input,
  signal,
} from "@angular/core";
import type { BrainModelDto } from "../../../../core/models";
import { SettingsStore } from "../../settings.store";

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
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [],
  template: `
    <div class="picker">
      @if (reactionsOnly()) {
        <div class="head">
          <h3>On-device model — realtime reactions</h3>
          <p class="sub text-secondary">
            A small model runs live during meetings, on this Mac.
          </p>
        </div>

        <div class="block">
          <span class="label">Language</span>
          <div class="seg" role="group" aria-label="Model language">
            <button
              type="button"
              [class.on]="family() === 'multi'"
              (click)="setFamily('multi')"
            >
              🌍 Multilingual
            </button>
            <button
              type="button"
              [class.on]="family() === 'pl'"
              (click)="setFamily('pl')"
            >
              🇵🇱 Polish-native
            </button>
          </div>
        </div>

        <div class="block">
          @if (selectedLight(); as m) {
            <div class="resolved" [class.notready]="!m.downloaded">
              <div class="r-info">
                <div class="r-name">{{ m.name }}</div>
                <div class="r-meta">
                  {{ familyLabel() }} · {{ sizeLabel(m.approxSizeBytes) }} · needs
                  ≥{{ m.minRamGb }} GB RAM
                </div>
              </div>
              @if (brainDownloadingId() === m.id) {
                <div class="brain-progress" role="status">
                  <div class="brain-progress-track" aria-hidden="true">
                    <div
                      class="brain-progress-fill"
                      [style.width.%]="brainDownloadFrac() * 100"
                    ></div>
                  </div>
                  <span class="brain-progress-label text-muted">
                    {{ brainPct() }}
                  </span>
                </div>
              } @else if (m.downloaded) {
                <span class="pill is-success">
                  <span class="pill-dot"></span>
                  Ready
                </span>
              } @else {
                <button
                  type="button"
                  class="btn btn-primary btn-sm"
                  (click)="downloadAndUse(m.id)"
                  [disabled]="brainDownloadingId() !== null"
                >
                  Download {{ sizeLabel(m.approxSizeBytes) }}
                </button>
              }
            </div>
          } @else {
            <p class="r-meta text-muted">
              No realtime model available for this language yet.
            </p>
          }
        </div>
      } @else {
        <div class="head">
          <h3>On-device model</h3>
          <p class="sub text-secondary">
            Runs your notes &amp; answers on this Mac. More effort = a heavier,
            smarter model — slower and needs more RAM.
          </p>
        </div>

        <div class="block">
          <span class="label">Language</span>
          <div class="seg" role="group" aria-label="Model language">
            <button
              type="button"
              [class.on]="family() === 'multi'"
              (click)="setFamily('multi')"
            >
              🌍 Multilingual
            </button>
            <button
              type="button"
              [class.on]="family() === 'pl'"
              (click)="setFamily('pl')"
            >
              🇵🇱 Polish-native
            </button>
          </div>
        </div>

        <div class="block">
          @if (heavyModels().length > 1) {
            <div class="row-between">
              <span class="label">Effort</span>
              <span class="r-meta text-muted">{{ effortLabel() }}</span>
            </div>
            <div class="slider-wrap">
              <input
                type="range"
                min="0"
                [max]="heavyModels().length - 1"
                step="1"
                [value]="heavyIdx()"
                (input)="onSlide($event)"
                [style.--fill.%]="fillPct()"
                aria-label="On-device model effort"
              />
              <div class="ends">
                <span>⚡ Faster</span>
                <span>Smarter 🧠</span>
              </div>
              <div class="ticks">
                @for (m of heavyModels(); track m.id; let i = $index) {
                  <span class="tick" [class.on]="i === heavyIdx()">
                    {{ sizeLabel(m.approxSizeBytes) }}
                  </span>
                }
              </div>
            </div>
          }

          @if (selectedHeavy(); as m) {
            <div class="resolved" [class.notready]="!m.downloaded">
              <div class="r-info">
                <div class="r-name">{{ m.name }}</div>
                <div class="r-meta">
                  {{ familyLabel() }} · {{ sizeLabel(m.approxSizeBytes) }} · needs
                  ≥{{ m.minRamGb }} GB RAM
                </div>
              </div>
              @if (brainDownloadingId() === m.id) {
                <div class="brain-progress" role="status">
                  <div class="brain-progress-track" aria-hidden="true">
                    <div
                      class="brain-progress-fill"
                      [style.width.%]="brainDownloadFrac() * 100"
                    ></div>
                  </div>
                  <span class="brain-progress-label text-muted">
                    {{ brainPct() }}
                  </span>
                </div>
              } @else if (m.downloaded) {
                <span class="pill is-success">
                  <span class="pill-dot"></span>
                  Ready
                </span>
              } @else {
                <button
                  type="button"
                  class="btn btn-primary btn-sm"
                  (click)="downloadAndUse(m.id)"
                  [disabled]="brainDownloadingId() !== null"
                >
                  Download {{ sizeLabel(m.approxSizeBytes) }}
                </button>
              }
            </div>

            @if (m.fitsRam === false) {
              <p class="ram-warn">
                ⚠ {{ m.name }} needs ≥{{ m.minRamGb }} GB RAM — may be slow
                alongside recording on this Mac.
              </p>
            }
          } @else {
            <p class="r-meta text-muted">
              No on-device model available for this language yet.
            </p>
          }

          <p class="auto-note">
            <b>Live reactions</b> during the meeting use the smallest model of
            your family ({{ lightModel()?.name ?? "—" }}) — fast, low-RAM,
            automatic.
          </p>

          <button type="button" class="adv-link" (click)="toggleGguf()">
            ＋ Use a custom GGUF file instead
          </button>
          @if (ggufOpen()) {
            <label class="field gguf-field">
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
          }
        </div>
      }

      @if (brainError(); as berr) {
        <p class="text-danger picker-error">{{ berr }}</p>
      }
    </div>
  `,
  styles: [
    `
      :host {
        display: contents;
      }
      .picker {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .head {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .head h3 {
        margin: 0;
        font-size: 1rem;
        font-weight: 650;
      }
      .sub {
        margin: 0;
        font-size: 0.85rem;
        line-height: 1.55;
      }
      .block {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .label {
        font-size: 0.8rem;
        font-weight: 600;
        color: var(--text-secondary);
        text-transform: uppercase;
        letter-spacing: 0.02em;
      }
      .row-between {
        display: flex;
        justify-content: space-between;
        align-items: baseline;
      }

      /* Segmented language toggle. */
      .seg {
        display: inline-flex;
        gap: var(--space-1);
        padding: var(--space-1);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-md);
        align-self: flex-start;
      }
      .seg button {
        border: none;
        background: none;
        color: var(--text-secondary);
        font: inherit;
        font-size: 0.9rem;
        font-weight: 600;
        padding: var(--space-2) var(--space-4);
        border-radius: var(--radius-sm);
        cursor: pointer;
        transition:
          background var(--transition),
          color var(--transition),
          box-shadow var(--transition);
      }
      .seg button.on {
        background: var(--accent-soft);
        color: var(--text-primary);
        box-shadow: 0 0 0 1px var(--accent);
      }

      /* Effort slider — accent gradient fill left of a white thumb. */
      .slider-wrap {
        padding: var(--space-2) 0 0;
      }
      input[type="range"] {
        -webkit-appearance: none;
        appearance: none;
        width: 100%;
        height: 6px;
        border-radius: 3px;
        outline: none;
        margin: 0;
        cursor: pointer;
        background: linear-gradient(
          90deg,
          var(--accent) 0%,
          var(--accent) var(--fill, 50%),
          var(--surface-hover) var(--fill, 50%),
          var(--surface-hover) 100%
        );
      }
      input[type="range"]::-webkit-slider-thumb {
        -webkit-appearance: none;
        appearance: none;
        width: 22px;
        height: 22px;
        border-radius: 50%;
        background: var(--text-on-accent);
        box-shadow:
          var(--shadow-sm),
          0 0 0 1px var(--accent);
        cursor: pointer;
        transition: transform var(--transition);
      }
      input[type="range"]::-webkit-slider-thumb:hover {
        transform: scale(1.1);
      }
      input[type="range"]::-moz-range-thumb {
        width: 22px;
        height: 22px;
        border: none;
        border-radius: 50%;
        background: var(--text-on-accent);
        box-shadow:
          var(--shadow-sm),
          0 0 0 1px var(--accent);
        cursor: pointer;
      }
      input[type="range"]:focus-visible {
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .ends {
        display: flex;
        justify-content: space-between;
        font-size: 0.78rem;
        color: var(--text-muted);
        margin-top: var(--space-1);
      }
      .ticks {
        display: flex;
        justify-content: space-between;
        margin-top: var(--space-2);
      }
      .tick {
        flex: 1;
        text-align: center;
        font-size: 0.78rem;
        color: var(--text-muted);
      }
      .tick:first-child {
        text-align: left;
      }
      .tick:last-child {
        text-align: right;
      }
      .tick.on {
        color: var(--accent-hover);
        font-weight: 600;
      }

      /* Resolved model card — accent border + soft bg when ready. */
      .resolved {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
        padding: var(--space-4);
        border: 1px solid var(--accent);
        background: var(--accent-soft);
        border-radius: var(--radius-md);
      }
      .resolved.notready {
        border-color: var(--border-subtle);
        background: var(--surface-input);
      }
      .r-info {
        min-width: 0;
      }
      .r-name {
        font-size: 0.95rem;
        font-weight: 650;
        color: var(--text-primary);
      }
      .r-meta {
        margin: 3px 0 0;
        font-size: 0.82rem;
        color: var(--text-muted);
      }
      .ram-warn {
        margin: 0;
        font-size: 0.82rem;
        line-height: 1.5;
        color: var(--warning);
      }
      .auto-note {
        margin: 0;
        padding-top: var(--space-3);
        border-top: 1px solid var(--border-subtle);
        font-size: 0.82rem;
        line-height: 1.55;
        color: var(--text-muted);
      }
      .auto-note b {
        color: var(--text-secondary);
      }
      .adv-link {
        align-self: flex-start;
        background: none;
        border: none;
        padding: 0;
        cursor: pointer;
        font: inherit;
        font-size: 0.82rem;
        font-weight: 550;
        color: var(--accent-hover);
        transition: color var(--transition);
      }
      .adv-link:hover {
        color: var(--accent);
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
      .gguf-field {
        margin-top: var(--space-1);
      }
      .picker-error {
        margin: 0;
        font-size: 0.85rem;
      }

      /* Download progress (shared shape with the other AI blocks). */
      .brain-progress {
        display: flex;
        flex-direction: column;
        gap: 3px;
        min-width: 120px;
        flex: none;
      }
      .brain-progress-track {
        height: 6px;
        border-radius: 3px;
        background: var(--surface-raised);
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
      .btn-sm {
        height: 32px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
        white-space: nowrap;
      }

      @media (prefers-reduced-motion: reduce) {
        .seg button,
        input[type="range"]::-webkit-slider-thumb,
        .brain-progress-fill,
        .adv-link {
          transition: none;
        }
      }
    `,
  ],
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
