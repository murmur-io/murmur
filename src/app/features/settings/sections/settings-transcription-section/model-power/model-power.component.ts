import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  input,
  output,
  signal,
} from "@angular/core";
import { IpcService } from "../../../../../core/ipc.service";
import {
  formatModelBytes,
  modelSizeLabel,
} from "../../../../../core/model-bytes";
import type { RecommendReason, WhisperModelDto } from "../../../../../core/models";
import { MachineService } from "../../../../../services/machine.service";
import { MurDisclosureComponent } from "../../../../../design-system/disclosure/disclosure.component";
import { MurMeterComponent } from "../../../../../design-system/meter/meter.component";
import { MurPillComponent } from "../../../../../design-system/pill/pill.component";
import {
  MurPowerSliderComponent,
  type PowerRung,
} from "../../../../../design-system/power-slider/power-slider.component";
import { ErrorCopyService } from "../../../../../core/copy/error-copy.service";

/**
 * The COARSE accuracy / speed rating per ladder rung.
 *
 * These are ORDINAL and deliberately blunt. Sharp and Maximum share the SAME
 * accuracy rating on purpose: the measured turbo-vs-large-v3 delta is unpublished
 * (see `transcribe/catalog.rs` — the registry's `power` is explicitly ranked by
 * COST, "never a claim that it transcribes better"), so inventing a difference
 * here would be exactly the dishonesty this workstream removes. Maximum still
 * differs where we DO have evidence: it is much slower and much heavier.
 */
const RATINGS: Record<string, { accuracy: number; speed: number }> = {
  Light: { accuracy: 1, speed: 4 },
  Balanced: { accuracy: 2, speed: 3 },
  Sharp: { accuracy: 4, speed: 3 },
  Maximum: { accuracy: 4, speed: 1 },
};

/**
 * The transcription-model picker, used by BOTH hosts (Settings → Transcription and
 * the onboarding wizard's model step). ONE component, two hosts, no duplicated
 * logic — the previous design had the ladder, the size table and the copy written
 * out twice, and they had already drifted.
 *
 * FULLY CONTROLLED: `size` in, `sizeChange` out. It stores no selection of its own,
 * so the reactive-form host (Settings) and the signals-only host (onboarding) drive
 * it identically and neither can fall out of sync with what it displays.
 *
 * Everything it renders comes from Rust via {@link MachineService}: the rungs, the
 * headlines, the byte figures, the recommendation and — critically — the REASON.
 * The FE maps a `reason` variant to a sentence and never assembles the reasoning,
 * because each variant is true about a DIFFERENT thing (see {@link reasonCopy}).
 */
@Component({
  selector: "app-model-power",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    MurDisclosureComponent,
    MurMeterComponent,
    MurPillComponent,
    MurPowerSliderComponent,
  ],
  templateUrl: "./model-power.component.html",
  styleUrl: "./model-power.component.scss",
})
export class ModelPowerComponent {
  private readonly ipc = inject(IpcService);
  readonly machine = inject(MachineService);
  private readonly errorCopy = inject(ErrorCopyService);

  /** The currently chosen size id. The host owns it; this component never writes it. */
  readonly size = input.required<string>();

  /** Disable every control (a download in flight, a recording in progress). */
  readonly disabled = input(false);

  /** A rung / long-tail row was chosen. The host persists it. */
  readonly sizeChange = output<string>();

  /**
   * "Live captions have nothing to run — fix it." Emitted rather than handled here
   * because both hosts ALREADY own a download flow with progress and error copy;
   * a second downloader inside this card would race them.
   */
  readonly repair = output<void>();

  /** Two-step delete confirmation — the id awaiting confirmation, or `null`. */
  private readonly _confirmDelete = signal<string | null>(null);
  readonly confirmDelete = this._confirmDelete.asReadonly();

  /** Surfaced verbatim from the backend when a delete is refused. */
  private readonly _deleteError = signal<string | null>(null);
  readonly deleteError = this._deleteError.asReadonly();

  private readonly _deleting = signal(false);
  readonly deleting = this._deleting.asReadonly();

  constructor() {
    // Stale-while-revalidate: the root service keeps the last catalog on screen, so
    // this refresh replaces it underneath rather than blanking the picker.
    void this.machine.refresh();
  }

  /** True once the backend catalog has arrived. Everything size-related is gated on it. */
  readonly catalogLoaded = computed(() => this.machine.models().length > 0);

  /** The four ladder rungs, ascending by cost, straight from the Rust catalog. */
  readonly ladder = computed<WhisperModelDto[]>(() =>
    this.machine
      .models()
      .filter((m) => m.tier !== null)
      .sort((a, b) => a.power - b.power),
  );

  /** The rungs as the slider wants them: the HUMAN name is what it announces. */
  readonly rungs = computed<PowerRung[]>(() =>
    this.ladder().map((m) => ({ id: m.id, name: m.tier ?? m.id })),
  );

  /**
   * EVERY catalog row, rungs included — this list is also the only place a downloaded
   * model can be DELETED.
   *
   * It deliberately no longer filters to `tier === null`. Doing so made the two
   * biggest downloads the ladder actively invites — Sharp (~875 MB) and Maximum
   * (~3 GB) — impossible to remove from any surface, so "reclaim disk" was
   * unreachable for exactly the sizes that cost disk. The backend's four refusals
   * (effective model, live-caption pin, non-registry id, never VAD/Parakeet) already
   * make surfacing it safe.
   */
  readonly longTail = computed<WhisperModelDto[]>(() =>
    [...this.machine.models()].sort((a, b) => a.power - b.power),
  );

  /** The catalog row for the current selection, or `null` for an unknown/custom id. */
  readonly selected = computed<WhisperModelDto | null>(
    () => this.machine.models().find((m) => m.id === this.size()) ?? null,
  );

  /** The selection is not one of the four rungs (a long-tail size, or something custom). */
  readonly selectionOffLadder = computed(
    () => this.selected()?.tier == null,
  );

  /** Accuracy / speed for the selected rung, or `null` off the ladder. */
  readonly rating = computed(() => {
    const tier = this.selected()?.tier;
    return tier ? (RATINGS[tier] ?? null) : null;
  });

  /** Sharp and Maximum share the accuracy rating — say so rather than let it look like a bug. */
  readonly accuracyDetail = computed(() => {
    const tier = this.selected()?.tier;
    return tier === "Sharp" || tier === "Maximum"
      ? "Sharp and Maximum share this rating — the measured difference between them is unpublished."
      : null;
  });

  /** The size this Mac's HARDWARE deserves (blind to what is on disk). */
  readonly recommendedId = this.machine.recommendedId;

  /** The recommended row's human tier label, when it has one. */
  readonly recommendedTier = computed(
    () =>
      this.machine.models().find((m) => m.id === this.recommendedId())?.tier ??
      null,
  );

  /** The current selection IS the hardware recommendation. */
  readonly isRecommended = computed(
    () => !!this.size() && this.size() === this.recommendedId(),
  );

  /**
   * WHY the default is what it is. Every branch says EXACTLY what is true about
   * itself and nothing a neighbouring branch could claim:
   *
   *  - `freshInstallAmpleRam` is the ONLY variant allowed to say "your Mac has N GB,
   *    so…" — every other branch is presence-first or capped, so a RAM-causal
   *    sentence there would be a fabricated explanation;
   *  - `notAppleSilicon` is the ONLY variant allowed to name a chip family. It is
   *    reached only when the arch probe ANSWERED and answered false;
   *  - `archUnknown` and `ramUnknown` claim NOTHING about the hardware. A failed
   *    probe is reached by a real Intel Mac AND by an Apple-Silicon Mac that could
   *    not answer, so naming either would be a coin flip presented as a fact;
   *  - `modestRam` is genuinely RAM-caused, but it is the "not enough" branch — it
   *    states the consequence without quoting a figure, so it can never read as the
   *    "your Mac has plenty" sentence;
   *  - `existingInstall` is the only variant that may mention an existing model.
   */
  readonly reasonCopy = computed(() => {
    const data = this.machine.data();
    if (!data) return "";
    const tier = this.recommendedTier() ?? "the lighter model";
    const reason: RecommendReason = data.reason;
    switch (reason) {
      case "freshInstallAmpleRam": {
        const ram = data.machine.totalRamBytes;
        // The causal sentence needs the figure it is causal about. Without a
        // measurement, fall back to a sentence that claims nothing.
        if (ram == null) {
          return `Murmur picked ${tier} for this Mac.`;
        }
        const gb = Math.round(ram / (1024 * 1024 * 1024));
        return `Your Mac has ${gb} GB of memory, so Murmur picked ${tier}.`;
      }
      case "notAppleSilicon":
        return "This is an Intel Mac, so Murmur stays with the lighter model — the heavier ones are tuned for Apple silicon.";
      case "archUnknown":
        return "Murmur couldn't read this Mac's processor, so it chose the lighter model rather than guess.";
      case "modestRam":
        return "This Mac's memory is below the threshold for the heavier model, so Murmur chose the lighter one.";
      case "ramUnknown":
        return "Murmur couldn't measure this Mac's memory, so it chose the lighter model rather than guess.";
      case "existingInstall":
        return "You already have a model downloaded, so Murmur kept using it instead of fetching another.";
      case "alreadyDownloaded":
        return "This model is already on this Mac, so Murmur kept it — nothing to download.";
      default:
        return "";
    }
  });

  /**
   * The download line. `0` and `null` mean DIFFERENT things and must never be
   * collapsed: `0` is "nothing to fetch", `null` is "a download IS pending but its
   * size is unknown". Rendering `null` as free would promise a free multi-GB
   * transfer, which is the exact dishonesty this workstream exists to remove.
   */
  readonly downloadCopy = computed(() => {
    const data = this.machine.data();
    if (!data) return "";
    const bytes = data.pendingDownloadBytes;
    if (bytes === null) return "Needs a download — size unknown.";
    if (bytes === 0) return "Already on this Mac — nothing to download.";
    return `About ${formatModelBytes(bytes)} to download, once.`;
  });

  /**
   * Live captions have nothing to run AND it is recoverable. `pinnedHeavy` is
   * deliberately excluded: that is a configuration the user chose, not a failed
   * download, so offering to "repair" it would be misleading.
   */
  readonly needsLiveRepair = computed(() => {
    const state = this.machine.data()?.liveCaptions;
    return state === "noModel" || state === "modelMissing";
  });

  /** A row's download size as SECONDARY detail. `null` renders as unknown, never free. */
  sizeLabel(m: WhisperModelDto): string {
    return modelSizeLabel(m.approxDownloadBytes);
  }

  pick(id: string): void {
    if (this.disabled() || !id || id === this.size()) return;
    this._deleteError.set(null);
    this.sizeChange.emit(id);
  }

  /** Jump straight to what this Mac deserves. */
  useRecommended(): void {
    this.pick(this.recommendedId());
  }

  askDelete(id: string): void {
    this._deleteError.set(null);
    this._confirmDelete.set(id);
  }

  cancelDelete(): void {
    this._confirmDelete.set(null);
  }

  /**
   * Delete a downloaded model. EVERY refusal (the effective model, the live-caption
   * pin, a non-registry id, a recording in progress) lives in the backend, so the
   * message shown here is the backend's own — the FE never re-implements the rules
   * and therefore cannot drift from them.
   */
  async remove(id: string): Promise<void> {
    this._confirmDelete.set(null);
    this._deleteError.set(null);
    this._deleting.set(true);
    try {
      await this.ipc.deleteWhisperModel(id);
    } catch (e) {
      this._deleteError.set(this.errorCopy.humanize(e));
    } finally {
      this._deleting.set(false);
      // Refresh either way: on success the row must stop claiming it is downloaded,
      // and on a refusal the catalog is the proof that nothing changed.
      await this.machine.refresh();
    }
  }
}
