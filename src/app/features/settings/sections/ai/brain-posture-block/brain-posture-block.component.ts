import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
} from "@angular/core";
import type { Posture } from "../../../../../core/models";
import { SettingsStore } from "../../../settings.store";

/**
 * AI & Models → Brain Posture block: the 3-way preset selector (Cloud /
 * Hybrid / Fully local), the installed-base retirement nudge, and the
 * contextual "right now" state area (idle state line + model pills, or
 * pending-download progress + Cancel).
 *
 * Moved from AiDefaultsBlockComponent so it forms its own card above
 * "What Murmur uses". The standalone "Enable Murmur Brain Live" card is
 * intentionally NOT included — posture-switching is now the only entry
 * point. Consumes Task-1 store signals.
 */
@Component({
  selector: "app-brain-posture-block",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [],
  templateUrl: "./brain-posture-block.component.html",
  styleUrl: "./brain-posture-block.component.scss",
})
export class BrainPostureBlockComponent {
  private readonly store = inject(SettingsStore);

  // ── Store signal wires ────────────────────────────────────────────────────
  readonly posture = this.store.posture;
  readonly postureBusy = this.store.postureBusy;
  readonly postureError = this.store.postureError;
  readonly pendingPosture = this.store.pendingPosture;
  readonly postureStateLine = this.store.postureStateLine;
  readonly neededModels = this.store.neededModels;
  readonly brainDownloadingId = this.store.brainDownloadingId;
  readonly brainDownloadFrac = this.store.brainDownloadFrac;
  readonly brainPct = this.store.brainPct;
  readonly brainLiveRamOk = this.store.brainLiveRamOk;
  readonly retirementNudge = this.store.retirementNudge;
  readonly applyingRetirement = this.store.applyingRetirement;

  /** Full model list — used to resolve the downloading model's display name. */
  private readonly brainModels = this.store.brainModels;

  /**
   * Display name of the model currently downloading, looked up from
   * brainModels() by brainDownloadingId(). Falls back to the raw id so the
   * progress label is never blank.
   *
   * Separate from neededModels() because during a setPosture download the
   * committed posture is still the OLD value → neededModelsFor(oldPosture)
   * returns [] → iterating neededModels() in the pending block would produce
   * no progress bar at all.
   */
  readonly downloadingModelName = computed((): string => {
    const id = this.brainDownloadingId();
    if (!id) return "";
    return this.brainModels().find((m) => m.id === id)?.name ?? id;
  });

  // ── Actions ───────────────────────────────────────────────────────────────

  /** Apply a Murmur Brain posture preset (`cloud` / `hybrid` / `fully_local`). */
  setPosture(p: Posture): void {
    void this.store.setPosture(p);
  }

  /** Abort an in-flight posture download; the committed posture is unchanged. */
  cancelPostureDownload(): void {
    this.store.cancelPostureDownload();
  }

  /** Download + select the Apache-licensed replacement for the retired model. */
  applyRetirement(): void {
    void this.store.applyRetirementReplacement();
  }

  /** Human-readable label for the pending posture (used in the progress copy). */
  pendingLabel(p: Posture): string {
    if (p === "hybrid") return "Hybrid";
    if (p === "fully_local") return "Fully local";
    return p;
  }
}
