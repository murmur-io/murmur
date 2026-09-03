import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  effect,
  inject,
  output,
  signal,
} from "@angular/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "../../../core/ipc.service";
import { SettingsStore } from "../../settings/settings.store";
import {
  ErrorCopyService,
  UserFacingError,
} from "../../../core/copy/error-copy.service";
import { subscribeUntilDestroyed } from "../../../core/subscribe-until-destroyed";

/**
 * "Enable the brain" — the one-click activation card for the two on-device
 * models a fresh install is missing: the heavy brain GGUF (powers user memory
 * extraction + on-device Ask) and the e5 embed model (powers semantic search;
 * until it exists, retrieval silently falls back to FTS).
 *
 * Backend untouched: this drives the EXISTING SettingsStore download actions
 * (downloadBrainModel / downloadEmbedModel) and the existing presence probes.
 * Everything stays on the user's Mac — the card copy says so loudly.
 *
 * Embedded in two places: the onboarding "brain" step and the Brain page
 * nudge banner (hidden once both models are present).
 *
 * NOTE: SettingsStore is component-scoped (provided only by the Settings shell),
 * so this card provides its OWN instance. It never calls the store's heavy
 * `load()` (which arms auto-save + patches the config form) — only the safe,
 * read-only registry loader + the download actions. The store's own download
 * progress stream is only armed by `load()`, so the live % is self-subscribed
 * here instead.
 */
@Component({
  selector: "app-brain-enable-card",
  changeDetection: ChangeDetectionStrategy.OnPush,
  providers: [SettingsStore],
  templateUrl: "./brain-enable-card.component.html",
  styleUrl: "./brain-enable-card.component.scss",
})
export class BrainEnableCardComponent {
  private readonly ipc = inject(IpcService);
  private readonly store = inject(SettingsStore);
  private readonly destroyRef = inject(DestroyRef);
  private readonly errorCopy = inject(ErrorCopyService);

  /** Fires once when both models land (parent may advance the wizard). */
  readonly enabled = output<void>();

  readonly brainPresent = signal<boolean | null>(null);
  readonly embedPresent = signal<boolean | null>(null);
  readonly running = signal(false);
  readonly stage = signal<"idle" | "brain" | "embed" | "done">("idle");
  readonly error = signal<string | null>(null);

  /** 0..1 progress for the in-flight brain GGUF (self-subscribed, best-effort). */
  private readonly _brainFrac = signal(0);
  /** Whole-percent label for the in-flight brain-model download. */
  readonly brainPct = computed(() => Math.round(this._brainFrac() * 100) + "%");
  private unlistenBrainDownload: UnlistenFn | null = null;

  readonly allReady = computed(
    () => this.brainPresent() === true && this.embedPresent() === true,
  );

  /** Total size hint from the registry DTOs (heavy model approx size). */
  readonly sizeHint = computed(() => {
    const heavy = this.store.brainModels().find((m) => m.selectedHeavy);
    if (!heavy?.approxSizeBytes) return "~2 GB";
    return `~${Math.round((heavy.approxSizeBytes / 1_000_000_000) * 10) / 10} GB`;
  });

  private readonly _probe = effect(
    () => {
      void this.refresh();
    },
  );

  constructor() {
    void this.subscribeBrainDownload();
  }

  /**
   * Subscribe directly to the brain-download progress stream (the isolated
   * store's own subscription is only armed by the heavy Settings `load()`,
   * which we intentionally never call). Best-effort: a missing stream just
   * leaves the bar inert — the download still resolves via the command promise.
   */
  private async subscribeBrainDownload(): Promise<void> {
    try {
      this.unlistenBrainDownload = await subscribeUntilDestroyed(
        this.destroyRef,
        () =>
          this.ipc.onBrainDownload((p) => {
            if (this.stage() !== "brain") return;
            if (p.total && p.total > 0) {
              this._brainFrac.set(Math.min(1, p.downloaded / p.total));
            }
            if (p.done) this._brainFrac.set(1);
          }),
      );
    } catch {
      // No stream available — progress stays inert.
    }
  }

  async refresh(): Promise<void> {
    try {
      // The registry is empty until something loads it; ensure it's populated so
      // the heavy-model lookup + size hint work outside the Settings page.
      if (this.store.brainModels().length === 0) {
        await this.store.refreshBrainModels();
      }
      const [brain, embed] = await Promise.all([
        this.ipc.brainModelPresent(),
        this.ipc.embedModelPresent(),
      ]);
      this.brainPresent.set(brain);
      this.embedPresent.set(embed);
      if (brain && embed) this.stage.set("done");
    } catch {
      // Presence probes never block the card; leave nulls (renders as unknown).
    }
  }

  /** The one click: heavy brain model first (big), then the e5 embed model. */
  async enable(): Promise<void> {
    if (this.running()) return;
    this.running.set(true);
    this.error.set(null);
    try {
      if (this.brainPresent() !== true) {
        this.stage.set("brain");
        this._brainFrac.set(0);
        const heavy = this.store.brainModels().find((m) => m.selectedHeavy);
        // Frontend-authored refusal, so it carries its own finished sentence (see
        // `UserFacingError`): the catalog has no model marked as the on-device choice, which is a
        // state the user can act on. A bare `Error` here would be denied to the generic sentence
        // and tell them nothing — and the old raw `e.message` ("no on-device model available") was
        // developer shorthand, not copy.
        if (!heavy) {
          throw new UserFacingError(
            "No on-device model is selected yet — choose one under AI & Models, then try again.",
          );
        }
        await this.store.downloadBrainModel(heavy.id);
        // Downloading the GGUF is not the same as SELECTING it: `brain_model_present`
        // resolves via the persisted `brain_model_id`, which download alone never
        // writes — a fresh install with no prior selection would download the file
        // successfully and then still report it "missing" forever (until something
        // else, e.g. a posture preset, happened to set the id). "Enable the brain"
        // means both fetch AND activate, so select explicitly here.
        if (this.store.brainError() === null) {
          await this.ipc.selectBrainModel(heavy.id);
        }
      }
      if (this.embedPresent() !== true) {
        this.stage.set("embed");
        await this.store.downloadEmbedModel();
      }
      await this.refresh();
      if (this.allReady()) {
        this.stage.set("done");
        this.enabled.emit();
      } else {
        // The store swallows download failures into its own signals — surface
        // them so a failed download never looks like a silent no-op. If NEITHER
        // signal is set, the download command itself reported success but the
        // presence probe still says missing — name which model and point at the
        // likely cause (this is a resolve/disk-state mismatch, not a network
        // failure, so "try again" alone is misleading).
        const specific = this.store.brainError() ?? this.store.embedDownloadError();
        this.error.set(
          specific ??
            (!this.brainPresent()
              ? "The brain model downloaded, but Murmur can't find it afterward. Check available disk space, then try again."
              : "The search-index model downloaded, but Murmur can't find it afterward. Check available disk space, then try again."),
        );
        this.stage.set("idle");
      }
    } catch (e) {
      this.error.set(this.errorCopy.humanize(e));
      this.stage.set("idle");
    } finally {
      this.running.set(false);
    }
  }
}
