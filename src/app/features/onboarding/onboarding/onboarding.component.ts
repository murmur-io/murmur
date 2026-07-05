import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  OnInit,
  computed,
  inject,
  signal,
} from "@angular/core";
import { Router } from "@angular/router";
import { open } from "@tauri-apps/plugin-dialog";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "../../../core/ipc.service";
import { hostIsLoopback } from "../../../core/loopback";
import type { AppConfigDto, ProviderStatus } from "../../../core/models";

/** The wizard steps, in order. Drives the dot indicator + progress copy. */
type Step = "welcome" | "model" | "provider" | "vault" | "done";
const STEPS: readonly Step[] = [
  "welcome",
  "model",
  "provider",
  "vault",
  "done",
];

/** Human-readable provider names for the AI-provider step. */
const PROVIDER_LABELS: Record<string, string> = {
  claude_code: "Claude Code",
  anthropic: "Anthropic API",
  ollama: "Ollama",
  gateway: "Kong AI Gateway (OpenAI-compatible)",
};

/**
 * The privacy posture the user consciously chooses at the provider step:
 *  - `"local"` — fully on-device (Ollama on this Mac + local brain); NOTHING
 *    leaves the Mac.
 *  - `"cloud"` — a cloud provider behind the redaction firewall; only REDACTED
 *    text ever leaves. Cloud is the DEFAULT (product decision) but the choice is
 *    explicit + visible, never silent.
 */
type Posture = "local" | "cloud";

/**
 * The cloud-posture providers (behind the redaction firewall), in display order.
 * FE-pinned because `provider_statuses` omits the gateway until a base URL is
 * configured — the wizard must still offer it (with a setup well below).
 */
const CLOUD_PROVIDER_IDS: readonly string[] = [
  "claude_code",
  "anthropic",
  "gateway",
];

/** The local-posture provider — Ollama running on THIS Mac (loopback). */
const LOCAL_PROVIDER_ID = "ollama";

/** Approx download size per Whisper quality (mirrors Settings). */
const SIZE_HINTS: Record<string, string> = {
  tiny: "~75 MB",
  base: "~150 MB",
  small: "~470 MB",
  medium: "~1.5 GB",
  "large-v3": "~3 GB",
};

/**
 * First-run wizard — a full-bleed, focused glassmorphism flow that gets a fresh
 * macOS user from launch to a working recorder in five calm steps:
 * Welcome → Transcription model → AI provider → Vault (optional) → Done.
 *
 * State lives entirely in signals; config is persisted to the backend as the
 * user makes choices (so the model can download for the chosen language/size,
 * and so re-running setup later picks up where they left off). The final step
 * flips `onboarded` and routes to /record.
 */
@Component({
  selector: "app-onboarding",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./onboarding.component.html",
  styleUrl: "./onboarding.component.scss",
})
export class OnboardingComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly router = inject(Router);
  private readonly destroyRef = inject(DestroyRef);

  readonly steps = STEPS;
  readonly currentStep = signal<Step>("welcome");
  readonly stepIndex = computed(() => STEPS.indexOf(this.currentStep()));

  /** Loaded config snapshot — preserved so finishing never drops other settings. */
  private loadedConfig: AppConfigDto | null = null;

  /** Model step. */
  readonly language = signal("");
  readonly modelSize = signal("small");
  readonly modelPresent = signal<boolean | null>(null);
  readonly downloading = signal(false);
  readonly downloadError = signal<string | null>(null);
  readonly sizeHint = computed(() => SIZE_HINTS[this.modelSize()] ?? "");
  /** 0..1 download progress for the in-flight model (best-effort from events). */
  readonly downloadFrac = signal(0);
  /** Whole-percent label for the in-flight model download. */
  readonly downloadPct = computed(
    () => Math.round(this.downloadFrac() * 100) + "%",
  );
  /** Release handle for the EVENT_MODEL_DOWNLOAD subscription. */
  private unlistenModelDownload: UnlistenFn | null = null;

  /** Provider step. */
  readonly providers = signal<ProviderStatus[]>([]);
  readonly provider = signal("claude_code");
  /**
   * The consciously-chosen privacy POSTURE (A3). Cloud is the DEFAULT (product
   * decision) but the two-way choice is explicit + visible — never silent. It is
   * seeded from the loaded provider on mount (ollama → local, else cloud) so
   * re-running setup reflects the real state.
   */
  readonly posture = signal<Posture>("cloud");
  readonly checking = signal(false);
  readonly apiKey = signal("");
  readonly savingKey = signal(false);
  readonly keyError = signal<string | null>(null);
  /** Gateway base URL (editable on the gateway tile; round-tripped on save). */
  readonly gatewayBaseUrl = signal("");
  /** Ollama base URL from the loaded config — read-only here, drives pickIsCloud. */
  private readonly ollamaBaseUrl = signal("http://localhost:11434");
  /** Whether cloud egress is already consented (from config; flips on finish). */
  readonly cloudConsented = signal(false);
  /**
   * True after "I'll set this up later" — the user explicitly DEFERRED the
   * provider choice, so finish() must NOT grant cloud consent for the
   * still-selected default (deferring is not consenting; lock-security
   * review). Cleared by an explicit tile pick.
   */
  readonly skippedProvider = signal(false);

  /**
   * The provider tiles for the CHOSEN posture, FE-pinned (CLOUD_PROVIDER_IDS /
   * LOCAL_PROVIDER_ID) and merged with the availability fan-out — an unconfigured
   * gateway has NO status row, so it renders as needing setup instead of
   * vanishing. The local posture shows only Ollama; the cloud posture shows the
   * redaction-firewall providers.
   */
  readonly providerTiles = computed<ProviderStatus[]>(() => {
    const statuses = this.providers();
    const ids =
      this.posture() === "local" ? [LOCAL_PROVIDER_ID] : CLOUD_PROVIDER_IDS;
    return ids.map(
      (id) =>
        statuses.find((p) => p.id === id) ?? {
          id,
          available: false,
          reason:
            id === "gateway" ? "Add your gateway's base URL below" : undefined,
        },
    );
  });

  /**
   * The visible privacy-posture badge (A3): a short, honest one-liner of what
   * leaves the Mac under the current choice. Derived, not stored — always mirrors
   * `posture()`.
   */
  readonly privacyBadge = computed(() =>
    this.posture() === "local"
      ? {
          label: "Fully local",
          detail: "Nothing leaves your Mac",
        }
      : {
          label: "Cloud via redaction firewall",
          detail: "Only redacted text ever leaves",
        },
  );

  /**
   * FE mirror of the backend's egress classification for the picked provider
   * (same rules as SettingsStore.providerIsCloud / egress_is_cloud):
   * claude_code / anthropic / gateway are cloud; ollama only when its base
   * URL host is non-loopback (`hostIsLoopback` — backend-parity, incl. the
   * full 127.0.0.0/8 range); unparseable fails safe as cloud.
   */
  readonly pickIsCloud = computed(() => {
    const id = this.provider();
    if (id === "ollama") {
      try {
        return !hostIsLoopback(new URL(this.ollamaBaseUrl()).hostname);
      } catch {
        return true; // unparseable → fail safe (treat as cloud)
      }
    }
    return true;
  });

  /** Vault step. */
  readonly vaultPath = signal<string | null>(null);

  /** Done step. */
  readonly finishing = signal(false);

  /** Gate for the per-step Continue button. */
  readonly canAdvance = computed(() => {
    switch (this.currentStep()) {
      case "model":
        // Must have a ready model before transcription can work.
        return this.modelPresent() === true;
      case "provider":
        // A gateway pick needs its base URL (the explicit "I'll set this up
        // later" skip stays available as the escape hatch).
        return (
          this.provider() !== "gateway" ||
          this.gatewayBaseUrl().trim().length > 0
        );
      case "vault":
      default:
        return true;
    }
  });

  async ngOnInit(): Promise<void> {
    // Whisper model-download progress stream (best-effort; drives the progress bar).
    try {
      this.unlistenModelDownload = await this.ipc.onModelDownload((p) => {
        if (!this.downloading()) return;
        if (p.total && p.total > 0) {
          this.downloadFrac.set(Math.min(1, p.downloaded / p.total));
        }
        if (p.done) this.downloadFrac.set(1);
      });
      this.destroyRef.onDestroy(() => this.unlistenModelDownload?.());
    } catch {
      // No model-download stream — progress stays inert; the download still resolves.
    }
    try {
      const cfg = await this.ipc.getConfig();
      this.loadedConfig = cfg;
      this.language.set(cfg.language ?? "");
      this.modelSize.set(cfg.modelSize ?? "small");
      this.provider.set(cfg.providerId ?? "claude_code");
      // Seed the posture from the loaded provider so re-running setup reflects
      // reality: only the local provider (Ollama) implies the local posture;
      // everything else (incl. a fresh install's claude_code default) is cloud.
      this.posture.set(
        (cfg.providerId ?? "claude_code") === LOCAL_PROVIDER_ID
          ? "local"
          : "cloud",
      );
      this.vaultPath.set(cfg.vaultPath ?? null);
      this.gatewayBaseUrl.set(cfg.gatewayBaseUrl ?? "");
      this.ollamaBaseUrl.set(cfg.ollamaBaseUrl ?? "http://localhost:11434");
      this.cloudConsented.set(cfg.cloudEgressConsented ?? false);
    } catch {
      // Fresh install with no config yet — defaults already cover us.
    }
  }

  labelFor(id: string): string {
    return PROVIDER_LABELS[id] ?? id;
  }

  isProviderAvailable(id: string): boolean {
    return this.providers().some((p) => p.id === id && p.available);
  }

  // ── Navigation ──────────────────────────────────────────────────────────

  async next(): Promise<void> {
    const i = this.stepIndex();
    if (i >= STEPS.length - 1) return;
    const target = STEPS[i + 1];
    this.currentStep.set(target);
    await this.onEnterStep(target);
  }

  async back(): Promise<void> {
    const i = this.stepIndex();
    if (i <= 0) return;
    const target = STEPS[i - 1];
    this.currentStep.set(target);
    await this.onEnterStep(target);
  }

  /** Side-effects run when a step becomes visible (probe model / providers). */
  private async onEnterStep(step: Step): Promise<void> {
    if (step === "model") {
      await this.persistConfig();
      this.modelPresent.set(await this.ipc.modelPresent());
    } else if (step === "provider") {
      await this.recheckProviders();
    }
  }

  // ── Model step ────────────────────────────────────────────────────────────

  async onLanguage(event: Event): Promise<void> {
    this.language.set((event.target as HTMLSelectElement).value);
    await this.refreshModelPresence();
  }

  async onModelSize(event: Event): Promise<void> {
    this.modelSize.set((event.target as HTMLSelectElement).value);
    await this.refreshModelPresence();
  }

  /** Persist the chosen language + size, then re-check what's on disk. */
  private async refreshModelPresence(): Promise<void> {
    this.modelPresent.set(null);
    await this.persistConfig();
    this.modelPresent.set(await this.ipc.modelPresent());
  }

  async downloadModel(): Promise<void> {
    this.downloadError.set(null);
    this.downloadFrac.set(0);
    this.downloading.set(true);
    try {
      // The model is fetched for the SAVED language + size — persist first.
      await this.persistConfig();
      await this.ipc.downloadModel();
      this.modelPresent.set(await this.ipc.modelPresent());
    } catch (e) {
      this.downloadError.set(String(e));
    } finally {
      this.downloading.set(false);
    }
  }

  // ── Provider step ───────────────────────────────────────────────────────

  /**
   * A3 — the conscious privacy-posture choice. Switching posture also picks a
   * sensible provider so the choice is never left in an inconsistent state:
   *  - local  → Ollama on this Mac (fully on-device).
   *  - cloud  → keep the current cloud pick, or default to claude_code if the
   *    current selection is the local provider.
   * An explicit posture pick supersedes an earlier "set this up later".
   */
  selectPosture(p: Posture): void {
    if (this.posture() === p) return;
    this.skippedProvider.set(false);
    this.posture.set(p);
    this.keyError.set(null);
    if (p === "local") {
      this.provider.set(LOCAL_PROVIDER_ID);
    } else if (this.provider() === LOCAL_PROVIDER_ID) {
      this.provider.set("claude_code");
    }
    void this.persistConfig();
  }

  selectProvider(id: string): void {
    // An explicit pick supersedes an earlier "set this up later" — consent
    // may be granted for it on finish again.
    this.skippedProvider.set(false);
    this.provider.set(id);
    this.keyError.set(null);
    void this.persistConfig();
  }

  /**
   * "I'll set this up later" — the user explicitly DEFERRED the provider
   * choice, so finish() must not grant cloud consent for the still-selected
   * default (lock-security review): deferring is not consenting. The
   * record-screen consent banner remains the recovery path.
   */
  skipProvider(): void {
    this.skippedProvider.set(true);
    void this.next();
  }

  async recheckProviders(): Promise<void> {
    this.checking.set(true);
    try {
      this.providers.set(await this.ipc.providerStatuses());
    } finally {
      this.checking.set(false);
    }
  }

  onApiKey(event: Event): void {
    this.apiKey.set((event.target as HTMLInputElement).value);
  }

  onGatewayUrl(event: Event): void {
    this.gatewayBaseUrl.set((event.target as HTMLInputElement).value);
  }

  /**
   * Persist the typed gateway base URL, then re-probe: once the URL is saved
   * the backend's fan-out includes the gateway, so the tile's pill goes live.
   */
  async persistGatewayUrl(): Promise<void> {
    this.checking.set(true);
    try {
      await this.persistConfig();
      this.providers.set(await this.ipc.providerStatuses());
    } finally {
      this.checking.set(false);
    }
  }

  async saveAnthropicKey(): Promise<void> {
    const key = this.apiKey().trim();
    if (!key) return;
    this.keyError.set(null);
    this.savingKey.set(true);
    try {
      await this.ipc.setAnthropicKey(key);
      this.apiKey.set("");
      // Re-probe so the "Available" pill updates once the key takes effect.
      await this.recheckProviders();
    } catch (e) {
      this.keyError.set(String(e));
    } finally {
      this.savingKey.set(false);
    }
  }

  // ── Vault step ────────────────────────────────────────────────────────────

  async pickVault(): Promise<void> {
    const dir = await open({ directory: true, multiple: false });
    if (typeof dir === "string") {
      this.vaultPath.set(dir);
      await this.persistConfig();
    }
  }

  // ── Finish ──────────────────────────────────────────────────────────────

  async finish(): Promise<void> {
    this.finishing.set(true);
    try {
      await this.persistConfig(true);
      // Consent-at-selection (research trap 11): an EXPLICIT cloud-classified
      // pick grants the one-time cloud-egress consent HERE so the first note
      // doesn't fail by design — but never for a deferred choice
      // (skippedProvider): deferring is not consenting. Ordered after
      // persistConfig for save-then-grant clarity; NOTE the ordering is not
      // load-bearing — the backend's save merge preserves consent regardless
      // of the DTO value (dto_to_config, commands.rs; test
      // save_config_merge_never_clobbers_or_grants_consent). Best-effort: if
      // the grant rejects, the record screen's consent banner remains the
      // recovery path — never trap the user in the wizard for it.
      if (this.pickIsCloud() && !this.cloudConsented() && !this.skippedProvider()) {
        try {
          await this.ipc.consentToCloudEgress();
          this.cloudConsented.set(true);
          if (this.loadedConfig) {
            this.loadedConfig = {
              ...this.loadedConfig,
              cloudEgressConsented: true,
            };
          }
        } catch {
          // Grant failed — recoverable post-wizard; don't block finishing.
        }
      }
      // Hand off to the first-run SHARING gateway when its gate is still open,
      // else straight to /record — reusing the SAME condition as app.component
      // (`!sharingChoiceMade && !accountStatus.loggedIn`). So a fresh install
      // flows /onboarding → (finish) → /welcome → pick → /record.
      let dest = "/record";
      if (!this.loadedConfig?.sharingChoiceMade) {
        const st = await this.ipc.accountStatus().catch(() => null);
        if (!st?.loggedIn) {
          dest = "/welcome";
        }
      }
      await this.router.navigate([dest]);
    } catch {
      // If saving the final flag fails, let them retry rather than trap them.
      this.finishing.set(false);
    }
  }

  /**
   * Save the current wizard choices, preserving every other config field from
   * the loaded snapshot. `markOnboarded` flips the first-run gate on the final
   * step. A tracked timeout is NOT needed here — these are awaited one-shots.
   */
  private async persistConfig(markOnboarded = false): Promise<void> {
    const base = this.loadedConfig;
    const cfg: AppConfigDto = {
      providerId: this.provider(),
      vaultPath: this.vaultPath(),
      vaultSubfolder: base?.vaultSubfolder ?? null,
      whisperModelPath: base?.whisperModelPath ?? null,
      language: this.language() || null,
      anthropicModel: base?.anthropicModel ?? "claude-opus-4-8",
      // Brain/AI model + effort overrides — preserve-only here (the Settings panel owns
      // the pickers); send the snapshot back unchanged so onboarding never clobbers them.
      providerModel: base?.providerModel ?? "",
      providerEffort: base?.providerEffort ?? "",
      ollamaBaseUrl: this.ollamaBaseUrl(),
      ollamaModel: base?.ollamaModel ?? "llama3.1",
      claudeBinary: base?.claudeBinary ?? "claude",
      inputDevice: base?.inputDevice ?? null,
      // Mirrors backend default capture_system_audio = true (settings/config.rs; #167).
      captureSystemAudio: base?.captureSystemAudio ?? true,
      vadEnabled: base?.vadEnabled ?? true,
      keepHiresMasters: base?.keepHiresMasters ?? false,
      diarizeOthers: base?.diarizeOthers ?? false,
      voiceprintEnabled: base?.voiceprintEnabled ?? false,
      aecEnabled: base?.aecEnabled ?? false,
      postAecEnabled: base?.postAecEnabled ?? false,
      // Recording-storage cap + opt-in auto-prune — preserve-only here (the Settings
      // Storage section owns them); round-trip the snapshot, defaults = no cap / off.
      audioStorageLimitGb: base?.audioStorageLimitGb ?? null,
      audioAutoPrune: base?.audioAutoPrune ?? false,
      modelSize: this.modelSize(),
      voiceTrigger: base?.voiceTrigger ?? false,
      onboarded: markOnboarded ? true : (base?.onboarded ?? false),
      // First-run sharing latch — preserve-only here (the /welcome gateway owns
      // it via mark_sharing_choice_made). Round-trip the snapshot so onboarding
      // never clears it; the backend's dto_to_config preserves it regardless.
      sharingChoiceMade: base?.sharingChoiceMade ?? false,
      noteStyle: base?.noteStyle ?? "standard",
      notesMode: base?.notesMode ?? "enhance",
      autoOrganize: base?.autoOrganize ?? false,
      noteLanguage: base?.noteLanguage ?? "auto",
      // Stage E security flags — read the current values from the snapshot and send
      // them back unchanged so onboarding never resets them (the backend's serde
      // defaults would otherwise clobber mcpRequireToken / cloudEgressConsented to
      // false). Defaults here mirror AppConfig::default() for a truly fresh install.
      mcpRequireToken: base?.mcpRequireToken ?? true,
      lockRequireBiometric: base?.lockRequireBiometric ?? true,
      relockOnScreenshare: base?.relockOnScreenshare ?? true,
      // The consent signal is seeded from the snapshot and flips true only via
      // the dedicated grant in finish() — carrying it back preserves it.
      cloudEgressConsented: this.cloudConsented(),
      // Phase H — brain / in-meeting voice assistant. Round-trip the snapshot so
      // onboarding never resets a user's brain choices; defaults mirror a fresh install.
      brainBackend: base?.brainBackend ?? "cloud",
      realtimeReactions: base?.realtimeReactions ?? false,
      brainModelId: base?.brainModelId ?? null,
      // Custom GGUF file path — preserve-only here (the AI & Models hub owns it);
      // round-trip the snapshot so onboarding never clears a user's custom path.
      brainModelPath: base?.brainModelPath ?? null,
      // Proactive brain hints — round-trip the snapshot, default ON (fresh install).
      proactiveHintsEnabled: base?.proactiveHintsEnabled ?? true,
      // Cross-meeting user memory — round-trip the snapshot, default ON (fresh install).
      userMemoryEnabled: base?.userMemoryEnabled ?? true,
      // brain2 RAG — semantic-search master flag; round-trip the snapshot.
      // Mirrors backend default semantic_search_enabled = true (settings/config.rs; #159/#160).
      semanticSearchEnabled: base?.semanticSearchEnabled ?? true,
      // brain2 connectors — web-search toggle + its preserve-only consent; round-trip
      // the snapshot so onboarding never resets them, both default off (no egress).
      webSearchEnabled: base?.webSearchEnabled ?? false,
      webSearchConsented: base?.webSearchConsented ?? false,
      // Opt-in claude-CLI env inheritance — round-trip the snapshot so onboarding never resets it.
      claudeCodeInheritEnv: base?.claudeCodeInheritEnv ?? false,
      // AI Gateway — the base URL is now wizard-editable (the gateway tile);
      // the model still round-trips from the snapshot untouched.
      gatewayBaseUrl: this.gatewayBaseUrl().trim(),
      gatewayModel: base?.gatewayModel ?? "",
      // Stage 4 — per-feature role overrides are preserve-only here (the
      // AI & Models hub owns the rows); round-trip the snapshot so onboarding
      // never clears an override, "" (inherit) on a fresh install.
      roleNotesConnection: base?.roleNotesConnection ?? "",
      roleNotesModel: base?.roleNotesModel ?? "",
      roleNotesEffort: base?.roleNotesEffort ?? "",
      roleAskConnection: base?.roleAskConnection ?? "",
      roleAskModel: base?.roleAskModel ?? "",
      roleAskEffort: base?.roleAskEffort ?? "",
      roleLiveConnection: base?.roleLiveConnection ?? "",
      roleLiveModel: base?.roleLiveModel ?? "",
      roleLiveEffort: base?.roleLiveEffort ?? "",
      // M3-CLIENT sharing — preserve-only here (the Settings → Account section
      // owns them); round-trip the snapshot so onboarding never clears a set
      // sharing server or the preserve-only share-egress consent. Defaults
      // mirror a fresh install (no server, no consent → no egress).
      shareBaseUrl: base?.shareBaseUrl ?? "",
      shareEgressConsented: base?.shareEgressConsented ?? false,
    };
    await this.ipc.saveConfig(cfg);
    // Keep the snapshot current so successive saves don't clobber fresh choices.
    this.loadedConfig = cfg;
  }
}
