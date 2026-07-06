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
  readonly pendingConfirm = this.store.pendingConfirm;
  readonly confirmModels = this.store.confirmModels;
  readonly confirmDownloadBytes = this.store.confirmDownloadBytes;
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
   * Plain-language meaning of the selected posture — a `{ lead, body }` pair so
   * the template can bold the lead. Null before the posture loads (no flash).
   */
  readonly postureMeaning = computed(
    (): { lead: string; body: string } | null => {
      switch (this.posture()) {
        case "cloud":
          return {
            lead: "Cloud.",
            body: "Your default engine writes your notes, answers and briefs — sent redacted after your consent. Recording, transcription, search and name-detection still happen only on this Mac.",
          };
        case "hybrid":
          return {
            lead: "Hybrid ⭐ — recommended.",
            body: "Same cloud quality for notes & answers, but realtime in-meeting reactions run on a small model on this Mac, so nothing leaves live.",
          };
        case "fully_local":
          return {
            lead: "Fully local.",
            body: "Every AI job runs on this Mac using built-in models. Nothing ever leaves. Bigger models are slower and need more RAM.",
          };
        case "custom":
          return {
            lead: "Custom mix.",
            body: "You've routed features individually — see What runs where below, tune under Advanced.",
          };
        default:
          return null;
      }
    },
  );

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

  /** Label of the posture currently downloading (the `@else if` block can't alias it). */
  readonly downloadingLabel = computed((): string => {
    const p = this.pendingPosture();
    return p ? this.pendingLabel(p) : "";
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

  /** Confirm the pending posture → start the on-device download, then commit. */
  confirmPostureDownload(): void {
    void this.store.confirmPostureDownload();
  }

  /** Dismiss the confirm card without downloading; the posture stays unchanged. */
  cancelPendingPosture(): void {
    this.store.cancelPendingPosture();
  }

  /** Format a byte count as a friendly "~1.1 GB" / "~620 MB" size label. */
  sizeLabel(bytes: number): string {
    const gb = 1024 * 1024 * 1024;
    return bytes >= gb
      ? "~" + (bytes / gb).toFixed(1) + " GB"
      : "~" + Math.round(bytes / (1024 * 1024)) + " MB";
  }

  /** One-line description of what a posture runs on-device (for the confirm card). */
  confirmDescription(p: Posture): string {
    if (p === "hybrid")
      return "Your Default AI keeps writing all notes; your Mac runs realtime reactions and keeps fact-extraction on-device.";
    if (p === "fully_local")
      return "Everything runs on this Mac — notes, answers, and reactions. Nothing leaves.";
    return "";
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
