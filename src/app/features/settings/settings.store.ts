import { DestroyRef, Injectable, computed, inject, signal } from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { FormBuilder, FormControl } from "@angular/forms";
import { startWith } from "rxjs";
import { Router } from "@angular/router";
import { open } from "@tauri-apps/plugin-dialog";
import { IpcService } from "../../core/ipc.service";
import { hostIsLoopback } from "../../core/loopback";
import type {
  AppConfigDto,
  AppInfo,
  BrainBackend,
  BrainModelDto,
  GatewayHealth,
  GatewayModel,
  InputDeviceInfo,
  ProviderStatus,
  ReindexResult,
} from "../../core/models";
import type { UnlistenFn } from "@tauri-apps/api/event";

/**
 * The four provider-backed connection ids — the ones `list_models` serves a
 * catalog for and a per-role model select makes sense on. `local`/`off` are
 * reasoner-only targets (no SummarizerProvider, no per-role model select — the
 * local model is the global GGUF registry selection).
 */
const PROVIDER_CONNECTION_IDS: readonly string[] = [
  "claude_code",
  "anthropic",
  "ollama",
  "gateway",
];

/**
 * Heuristic: does this string look like a filesystem PATH to a `.gguf` file
 * (vs a bare registry id like `qwen3-14b`)? True when it contains a path
 * separator, ends `.gguf` (case-insensitive), or starts with `~`. Drives which
 * of the two mutually-exclusive brain-model controls a typed custom value fills
 * (`brainModelPath` for a path, else `brainModelId`).
 */
export function looksLikeGgufPath(v: string): boolean {
  const s = v.trim();
  if (!s) return false;
  return (
    s.includes("/") ||
    s.includes("\\") ||
    s.startsWith("~") ||
    s.toLowerCase().endsWith(".gguf")
  );
}

/** Display names for the connection ids (matches the connection cards). */
const CONNECTION_LABELS: Readonly<Record<string, string>> = {
  claude_code: "Claude Code",
  anthropic: "Anthropic API",
  ollama: "Ollama",
  gateway: "Kong AI Gateway",
};

/**
 * Shared state + IPC orchestration for the Settings page (Stage-1 split of the
 * former settings.component.ts monolith — moved here VERBATIM, no behavior
 * change).
 *
 * Provided BY THE SHELL (`SettingsComponent`'s `providers`), NOT in root, so
 * its lifetime is exactly the settings route's — created on enter, destroyed
 * on leave (DestroyRef releases the event-stream unlistens then, same as the
 * pre-split component). The single reactive `form` and every cross-section
 * signal live here, so switching sidebar sections (which destroys/recreates
 * the section child) never loses unsaved edits or in-flight download/consent
 * state. Writable signals are private (`_x`) and published `.asReadonly()`
 * (RecorderStore pattern); section children mutate state only through the
 * store's methods.
 */
@Injectable()
export class SettingsStore {
  private readonly ipc = inject(IpcService);
  private readonly fb = inject(FormBuilder);
  private readonly router = inject(Router);
  private readonly destroyRef = inject(DestroyRef);

  // ── About — product identity + shared update-check state ────────────────

  /** Static product identity (name/version/description), loaded once in load(). */
  private readonly _appInfo = signal<AppInfo | null>(null);
  readonly appInfo = this._appInfo.asReadonly();

  /** Tracked so we can cancel the pending "Copied" reset on destroy (no leaks). */
  private copyResetTimer: ReturnType<typeof setTimeout> | null = null;

  /** Same as copyResetTimer, but for the MCP "Copied" flash — cancelled on destroy. */
  private mcpCopyResetTimer: ReturnType<typeof setTimeout> | null = null;

  /**
   * Built eagerly with defaults so the panel always renders (no "stuck on loading"),
   * then `patchValue`d from the loaded config in load(). SF-6: reactive forms.
   */
  readonly form = this.fb.nonNullable.group({
    providerId: "claude_code",
    vaultPath: "",
    vaultSubfolder: "",
    whisperModelPath: "",
    language: "",
    // UNRENDERED on purpose (Stage 2): the "Anthropic model" free-text control was
    // removed from the UI — the Default model picker (providerModel) is THE model
    // control now. The FormControl MUST stay in the group anyway: it is loaded from
    // config and round-tripped on save; dropping it would make save() wipe the
    // user's stored anthropic_model fallback (mod.rs reads it when providerModel
    // is empty).
    anthropicModel: "claude-opus-4-8",
    // Brain/AI model + reasoning-effort overrides ("" = provider default). Effort is
    // honored only by the anthropic provider; the picker is gated on providerId below.
    providerModel: "",
    providerEffort: "",
    ollamaBaseUrl: "http://localhost:11434",
    ollamaModel: "llama3.1",
    claudeBinary: "claude",
    // Opt-in: pass the shell env to the `claude` CLI (restores env ANTHROPIC_API_KEY auth).
    claudeCodeInheritEnv: false,
    inputDevice: "",
    captureSystemAudio: false,
    vadEnabled: true,
    keepHiresMasters: false,
    diarizeOthers: false,
    aecEnabled: false,
    postAecEnabled: true,
    modelSize: "large-v3",
    voiceTrigger: false,
    noteStyle: "standard",
    notesMode: "enhance",
    autoOrganize: false,
    noteLanguage: "auto",
    // Phase H — brain / in-meeting voice assistant.
    brainBackend: "cloud" as BrainBackend,
    realtimeReactions: false,
    // Proactive brain (P2) — zero-egress recall cards while recording; default ON.
    proactiveHintsEnabled: true,
    // Cross-meeting USER MEMORY master gate; default ON. Off turns memory off entirely (backend).
    userMemoryEnabled: true,
    /** Selected registry brain-model id. Empty → null on save. */
    brainModelId: "",
    /** Explicit custom GGUF file PATH (wins over brainModelId). Empty → null on save. */
    brainModelPath: "",
    // brain2 RAG — semantic-search master flag (round-tripped on save).
    semanticSearchEnabled: false,
    // brain2 connectors — web-search master toggle (NEW EGRESS; round-tripped).
    webSearchEnabled: false,
    // AI Gateway (Phase 1) — base URL and model, round-tripped on save.
    gatewayBaseUrl: "",
    gatewayModel: "",
    // Stage 4 — per-feature model-role overrides ("" = inherit the legacy
    // mapping). The connection key is the override switch: the backend ignores
    // a lone model/effort when the connection is empty. Always in the group so
    // every save() includes all 9 — the role keys are settable by design, and
    // with the FE always sending them the stage-3 review's "an older FE could
    // clear them" concern is moot going forward.
    roleNotesConnection: "",
    roleNotesModel: "",
    roleNotesEffort: "",
    roleAskConnection: "",
    roleAskModel: "",
    roleAskEffort: "",
    roleLiveConnection: "",
    roleLiveModel: "",
    roleLiveEffort: "",
  });
  readonly keyControl = new FormControl("", { nonNullable: true });
  /** BYO Brave Search API key input (web-search connector). Cleared after save. */
  readonly webKeyControl = new FormControl("", { nonNullable: true });

  private readonly _providers = signal<ProviderStatus[]>([]);
  readonly providers = this._providers.asReadonly();
  /** Available mic input devices for the picker (loaded best-effort in load()). */
  private readonly _inputDevices = signal<InputDeviceInfo[]>([]);
  readonly inputDevices = this._inputDevices.asReadonly();
  private readonly _hasKey = signal(false);
  readonly hasKey = this._hasKey.asReadonly();
  private readonly _saved = signal(false);
  readonly saved = this._saved.asReadonly();
  private readonly _loadError = signal<string | null>(null);
  readonly loadError = this._loadError.asReadonly();

  /** The Obsidian homepage — shown as copyable text (no in-webview navigation). */
  readonly obsidianUrl = "obsidian.md";

  /** Flips true for ~1.6s after copying the Obsidian URL — drives the button's confirmed state. */
  private readonly _urlCopied = signal(false);
  readonly urlCopied = this._urlCopied.asReadonly();

  /** The localhost MCP server address — shown inline and embedded in the config. */
  readonly mcpUrl = "http://127.0.0.1:8765";

  /** Exact JSON to drop into the Claude Desktop config — copied verbatim. */
  readonly mcpConfig = `{
  "mcpServers": {
    "murmur": {
      "url": "${this.mcpUrl}"
    }
  }
}`;

  /** Flips true for ~1.6s after copying the MCP config — drives the button's confirmed state. */
  private readonly _configCopied = signal(false);
  readonly configCopied = this._configCopied.asReadonly();

  /**
   * Real Whisper-model presence (same UX as the record screen).
   * `null` = not yet checked, `true`/`false` = detected via ipc.modelPresent().
   */
  private readonly _modelPresent = signal<boolean | null>(null);
  readonly modelPresent = this._modelPresent.asReadonly();

  /** True while a download is in-flight — disables the download button. */
  private readonly _downloadingModel = signal(false);
  readonly downloadingModel = this._downloadingModel.asReadonly();

  /** Surfaced if ipc.downloadModel() rejects. */
  private readonly _modelDownloadError = signal<string | null>(null);
  readonly modelDownloadError = this._modelDownloadError.asReadonly();

  /** 0..1 download progress for the in-flight Whisper model (best-effort from events). */
  private readonly _modelDownloadFrac = signal(0);
  readonly modelDownloadFrac = this._modelDownloadFrac.asReadonly();
  /** Whole-percent label for the in-flight Whisper-model download. */
  readonly modelPct = computed(
    () => Math.round(this.modelDownloadFrac() * 100) + "%",
  );
  /** Release handle for the EVENT_MODEL_DOWNLOAD subscription. */
  private unlistenModelDownload: UnlistenFn | null = null;

  /** Approx download size for the selected quality (shown on the Download button). */
  private readonly _downloadHint = signal("~3 GB");
  readonly downloadHint = this._downloadHint.asReadonly();

  /** Preserved from the loaded config (not a form field) so saving never un-onboards. */
  private loadedOnboarded = true;

  /**
   * Stage E security flags — preserved from the loaded config (not form-edited)
   * so save() round-trips them instead of letting the backend default them off.
   */
  private loadedMcpRequireToken = true;
  private loadedLockRequireBiometric = true;
  private loadedRelockOnScreenshare = true;

  /** Cloud-egress consent state — drives the "Cloud processing" section; round-tripped on save. */
  private readonly _cloudConsented = signal(false);
  readonly cloudConsented = this._cloudConsented.asReadonly();
  /** True while the one-time consent command is in flight. */
  private readonly _consenting = signal(false);
  readonly consenting = this._consenting.asReadonly();
  /** Surfaced if granting consent rejects. */
  private readonly _consentError = signal<string | null>(null);
  readonly consentError = this._consentError.asReadonly();
  /** True while the revoke-consent command is in flight. */
  private readonly _revoking = signal(false);
  readonly revoking = this._revoking.asReadonly();
  /** Surfaced if revoking consent rejects. */
  private readonly _revokeError = signal<string | null>(null);
  readonly revokeError = this._revokeError.asReadonly();

  // ── brain2 connectors — web search (NEW EGRESS) ────────────────────────

  /** Web-search egress consent state — drives the "Allow web search" section; round-tripped on save. */
  private readonly _webConsented = signal(false);
  readonly webConsented = this._webConsented.asReadonly();
  /** True while the one-time web-search consent command is in flight. */
  private readonly _webConsenting = signal(false);
  readonly webConsenting = this._webConsenting.asReadonly();
  /** Surfaced if granting web-search consent rejects. */
  private readonly _webConsentError = signal<string | null>(null);
  readonly webConsentError = this._webConsentError.asReadonly();
  /** Whether a Brave Search API key is stored (has-key check; never the value). */
  private readonly _hasWebKey = signal(false);
  readonly hasWebKey = this._hasWebKey.asReadonly();
  /** True while the BYO key is being saved. */
  private readonly _savingWebKey = signal(false);
  readonly savingWebKey = this._savingWebKey.asReadonly();
  /** Surfaced if storing the web-search key rejects. */
  private readonly _webKeyError = signal<string | null>(null);
  readonly webKeyError = this._webKeyError.asReadonly();

  // ── AI Gateway (Phase 1) — key management + destination computed signals ──

  /** Gateway API key input. Cleared after save; value never sent back. */
  readonly gatewayKeyControl = new FormControl("", { nonNullable: true });
  /** Whether a gateway API key is currently stored (has-key probe; never the value). */
  private readonly _hasGatewayKey = signal(false);
  readonly hasGatewayKey = this._hasGatewayKey.asReadonly();
  /** Surfaced if storing or clearing the gateway key rejects. */
  private readonly _gatewayKeyError = signal<string | null>(null);
  readonly gatewayKeyError = this._gatewayKeyError.asReadonly();

  // ── AI Gateway (Phase 3) — live model picker ────────────────────────────

  /** Models fetched from the gateway's `/v1/models` endpoint. Empty = use text fallback. */
  private readonly _gatewayModels = signal<GatewayModel[]>([]);
  readonly gatewayModels = this._gatewayModels.asReadonly();
  /** True while list_gateway_models is in-flight — disables the Refresh button. */
  private readonly _gatewayModelsLoading = signal(false);
  readonly gatewayModelsLoading = this._gatewayModelsLoading.asReadonly();
  /** Non-null when the last refreshGatewayModels() call failed — surfaces a fallback hint. */
  private readonly _gatewayModelError = signal<string | null>(null);
  readonly gatewayModelError = this._gatewayModelError.asReadonly();

  // ── AI Gateway (Phase 4) — health probe ─────────────────────────────────

  /** Last health-probe result; null = not yet checked. */
  private readonly _gatewayHealth = signal<GatewayHealth | null>(null);
  readonly gatewayHealth = this._gatewayHealth.asReadonly();
  /** True while gateway_health is in-flight — disables the Check button. */
  private readonly _gatewayHealthChecking = signal(false);
  readonly gatewayHealthChecking = this._gatewayHealthChecking.asReadonly();

  /**
   * Live signal of the gatewayModel form control's value. Mirrors the pattern used
   * for `_gatewayBaseUrlValue` below so `gatewayModelIsCustom` is reactive.
   */
  private readonly _gatewayModelValue = toSignal(
    this.form.controls.gatewayModel.valueChanges.pipe(startWith("")),
    { initialValue: "" },
  );

  /**
   * True when a model is currently saved in the form AND that model is NOT present
   * in the fetched `gatewayModels` catalog. In that case the template adds it as a
   * "(custom)" option so the manually-typed value is never silently lost.
   *
   * Implemented as a `computed` rather than an inline arrow function in the template
   * to satisfy the Angular template parser (arrow functions are banned in expressions).
   */
  readonly gatewayModelIsCustom = computed(() => {
    const current = this._gatewayModelValue();
    if (!current) return false;
    return !this.gatewayModels().some((m) => m.id === current);
  });

  /**
   * Live signal of the gatewayBaseUrl form control's value — built from
   * `valueChanges` so computed() signals can track it reactively. `startWith`
   * seeds the initial value (the form control starts as `""`).
   */
  private readonly _gatewayBaseUrlValue = toSignal(
    // valueChanges not available until the form is fully constructed, but because
    // this field initialiser runs after the `form` field above, the form group
    // (and its controls) already exist at this point.
    this.form.controls.gatewayBaseUrl.valueChanges.pipe(startWith("")),
    { initialValue: "" },
  );

  /**
   * Computed URL validation warning: true when the URL is non-empty AND is not
   * a valid https:// URL AND is not an http:// loopback (`hostIsLoopback` —
   * the backend-parity classification). Derived from `_gatewayBaseUrlValue`
   * so it updates on every keystroke.
   */
  readonly gatewayUrlWarning = computed(() => {
    const url = this._gatewayBaseUrlValue();
    if (!url) return false;
    if (url.startsWith("https://")) return false;
    if (url.startsWith("http://")) {
      try {
        return !hostIsLoopback(new URL(url).hostname);
      } catch {
        return true; // unparseable → warn
      }
    }
    return true;
  });

  /**
   * Computed destination info from the gateway base URL:
   * - `null` when the URL is empty or unparseable (no banner shown)
   * - `{ isRemote: true, host }` for https:// non-loopback → shows the warning banner
   * - `{ isRemote: false, host }` for loopback http:// → shows the calmer note
   */
  readonly gatewayDestination = computed((): { isRemote: boolean; host: string } | null => {
    const url = this._gatewayBaseUrlValue();
    if (!url) return null;
    try {
      const parsed = new URL(url);
      return { isRemote: !hostIsLoopback(parsed.hostname), host: parsed.host };
    } catch {
      return null;
    }
  });

  /**
   * Live signals of the providerId / ollamaBaseUrl form controls — same
   * `valueChanges` bridge as `_gatewayBaseUrlValue` above, seeded with the
   * form defaults so `providerIsCloud` is correct before the config loads.
   */
  private readonly _providerIdValue = toSignal(
    this.form.controls.providerId.valueChanges.pipe(startWith("claude_code")),
    { initialValue: "claude_code" },
  );
  private readonly _ollamaBaseUrlValue = toSignal(
    this.form.controls.ollamaBaseUrl.valueChanges.pipe(
      startWith("http://localhost:11434"),
    ),
    { initialValue: "http://localhost:11434" },
  );

  /**
   * Whether the Ollama connection points off this Mac — true when its base URL
   * host is NOT loopback per `hostIsLoopback` (backend parity: localhost
   * case-insensitive, [::1]/::1, the full 127.0.0.0/8 range), or is
   * unparseable (failing safe as remote/cloud). The per-connection half of
   * the egress classification: drives both the Local-vs-Cloud grouping of the
   * Ollama card in AI & Models and `providerIsCloud` below, so the two can't
   * diverge.
   */
  readonly ollamaIsRemote = computed(() => {
    try {
      return !hostIsLoopback(new URL(this._ollamaBaseUrlValue()).hostname);
    } catch {
      return true; // unparseable → fail safe (treat as remote/cloud)
    }
  });

  /**
   * FE mirror of the backend's egress classification (`egress_is_cloud`,
   * summarize/mod.rs): claude_code / anthropic / gateway always send content
   * off-device (gateway even on loopback — it can forward to the cloud);
   * ollama is local ONLY when its base URL host is loopback (see
   * `ollamaIsRemote`). Reuse this wherever the FE decides "is this cloud" so
   * the two classifications can't diverge.
   */
  readonly providerIsCloud = computed(() => {
    const id = this._providerIdValue();
    if (id === "ollama") return this.ollamaIsRemote();
    return true; // claude_code | anthropic | gateway | any future id
  });

  /**
   * Where the DEFAULT provider sends text, for the "Where your text goes"
   * privacy strip: `null` when the default is a local (loopback) Ollama —
   * nothing leaves, the line is hidden — otherwise the connection's display
   * name and its destination host/service.
   */
  readonly defaultEgressDestination = computed(
    (): { connection: string; destination: string } | null => {
      const id = this._providerIdValue();
      switch (id) {
        case "claude_code":
          return {
            connection: "Claude Code",
            destination: "Anthropic (via the claude CLI)",
          };
        case "anthropic":
          return { connection: "Anthropic API", destination: "api.anthropic.com" };
        case "gateway": {
          const dest = this.gatewayDestination();
          return {
            connection: "Kong AI Gateway",
            destination: dest ? dest.host : "your gateway (base URL not set)",
          };
        }
        case "ollama": {
          if (!this.ollamaIsRemote()) return null; // local — nothing leaves
          let host = this._ollamaBaseUrlValue();
          try {
            host = new URL(host).host;
          } catch {
            // unparseable → show the raw value rather than nothing
          }
          return { connection: "Ollama (remote)", destination: host };
        }
        default:
          // Unknown id — fail safe: cloud, destination unknown.
          return { connection: id, destination: "unknown destination" };
      }
    },
  );

  // ── Stage 4 — per-feature model roles (Notes / Ask / Live) ─────────────

  /**
   * Live signals of the role-connection controls + the model/effort context the
   * role rows and the consent banner derive from — the same `valueChanges`
   * bridge as `_gatewayBaseUrlValue` above, seeded with the form defaults.
   */
  readonly roleNotesConnValue = toSignal(
    this.form.controls.roleNotesConnection.valueChanges.pipe(startWith("")),
    { initialValue: "" },
  );
  readonly roleAskConnValue = toSignal(
    this.form.controls.roleAskConnection.valueChanges.pipe(startWith("")),
    { initialValue: "" },
  );
  readonly roleLiveConnValue = toSignal(
    this.form.controls.roleLiveConnection.valueChanges.pipe(startWith("")),
    { initialValue: "" },
  );
  readonly roleNotesModelValue = toSignal(
    this.form.controls.roleNotesModel.valueChanges.pipe(startWith("")),
    { initialValue: "" },
  );
  readonly roleAskModelValue = toSignal(
    this.form.controls.roleAskModel.valueChanges.pipe(startWith("")),
    { initialValue: "" },
  );
  readonly roleLiveModelValue = toSignal(
    this.form.controls.roleLiveModel.valueChanges.pipe(startWith("")),
    { initialValue: "" },
  );
  private readonly _providerModelValue = toSignal(
    this.form.controls.providerModel.valueChanges.pipe(startWith("")),
    { initialValue: "" },
  );
  private readonly _ollamaModelValue = toSignal(
    this.form.controls.ollamaModel.valueChanges.pipe(startWith("llama3.1")),
    { initialValue: "llama3.1" },
  );
  private readonly _brainBackendValue = toSignal(
    this.form.controls.brainBackend.valueChanges.pipe(
      startWith("cloud" as BrainBackend),
    ),
    { initialValue: "cloud" as BrainBackend },
  );
  /**
   * Public mirror of the legacy fallback value — the role rows need it to keep
   * the shared GGUF registry reachable for a legacy `brainBackend=local`
   * install that has no explicit role keys (its Notes pre-analysis + Ask/Live
   * fallback still run on the local model).
   */
  readonly brainBackendValue = this._brainBackendValue;

  /**
   * `brainBackend` as last LOADED — restored when the Ask row returns to
   * "Inherit" so Ask → anthropic → Inherit round-trips a legacy local/off
   * fallback instead of ratcheting it to "cloud" (lock-security stage-4 nit).
   */
  private _loadedBrainBackend: BrainBackend = "cloud";

  /**
   * Per-connection model catalogs from `list_models` (backend constant for
   * claude_code/anthropic, live endpoints for ollama/gateway). A key that is
   * PRESENT with an empty array = "fetch tried, no catalog" → the pickers fall
   * back to a free-text input (the gateway keep-manually-typed pattern).
   */
  private readonly _modelCatalogs = signal<
    Readonly<Record<string, readonly string[]>>
  >({});
  readonly modelCatalogs = this._modelCatalogs.asReadonly();
  /** Connections with a `list_models` fetch currently in flight. */
  private readonly _modelsLoading = signal<ReadonlySet<string>>(new Set());
  readonly modelsLoading = this._modelsLoading.asReadonly();

  /** Fetch a connection's catalog once; later calls are no-ops (Refresh re-fetches). */
  async ensureModels(connection: string): Promise<void> {
    if (this._modelCatalogs()[connection] !== undefined) return;
    await this.refreshModels(connection);
  }

  /**
   * (Re-)fetch one connection's model catalog. A rejection (endpoint down, or
   * a backend without `list_models` yet) records an EMPTY catalog so the UI
   * shows the free-text fallback instead of a dead select — same contract as
   * `refreshGatewayModels`. Button-driven or load-driven, never an effect.
   */
  async refreshModels(connection: string): Promise<void> {
    if (!PROVIDER_CONNECTION_IDS.includes(connection)) return;
    if (this._modelsLoading().has(connection)) return;
    this._modelsLoading.set(new Set(this._modelsLoading()).add(connection));
    let models: string[] = [];
    try {
      models = await this.ipc.listModels(connection);
    } catch {
      // Fall through with [] — free-text fallback.
    }
    this._modelCatalogs.set({ ...this._modelCatalogs(), [connection]: models });
    const next = new Set(this._modelsLoading());
    next.delete(connection);
    this._modelsLoading.set(next);
  }

  /** The Default-model picker's catalog (claude_code/anthropic Default AI only). */
  readonly defaultModelCatalog = computed(
    () => this.modelCatalogs()[this._providerIdValue()] ?? [],
  );
  /** True while the Default-model picker's catalog fetch is in flight. */
  readonly defaultModelsLoading = computed(() =>
    this.modelsLoading().has(this._providerIdValue()),
  );
  /** Keep a manually-typed default model selectable when absent from the catalog. */
  readonly defaultModelIsCustom = computed(() => {
    const current = this._providerModelValue();
    if (!current) return false;
    return !this.defaultModelCatalog().includes(current);
  });

  /** "Claude Code · claude-opus-4-8"-style summary of the Default AI row. */
  readonly defaultAiSummary = computed(() => {
    const id = this._providerIdValue();
    const label = CONNECTION_LABELS[id] ?? id;
    let model: string;
    switch (id) {
      case "ollama":
        model = this._ollamaModelValue() || "default model";
        break;
      case "gateway":
        model = this._gatewayModelValue() || "gateway default";
        break;
      default:
        model = this._providerModelValue() || "default model";
    }
    return `${label} · ${model}`;
  });

  /** Notes-row Inherit summary — Notes always falls back to the Default AI triple. */
  readonly notesInheritSummary = computed(
    () => `Follows Default AI: ${this.defaultAiSummary()}`,
  );

  /**
   * Ask/Live-row Inherit summary — an honest mirror of the backend resolver:
   * with the role key empty, Ask/Live fall back to the legacy `brainBackend`
   * mapping, NOT unconditionally to the Default AI. Showing "Follows Default
   * AI" to a legacy `brain_backend=local` install would be a lie.
   */
  readonly assistantInheritSummary = computed(() => {
    switch (this._brainBackendValue()) {
      case "local":
        return "Follows the assistant fallback: Local model — on-device";
      case "off":
        return "Follows the assistant fallback: Off — retrieval only";
      default:
        return `Follows Default AI: ${this.defaultAiSummary()}`;
    }
  });

  /**
   * Whether the LIVE role's resolved connection is cloud-classified — the
   * in-meeting-assistant consent banner keys on this (it egresses live meeting
   * context to the LIVE target, which since Stage 4 need not be `brainBackend`).
   * Mirrors the backend resolver: explicit role key wins; "" falls back to the
   * `brainBackend` mapping (cloud → the default provider, local/off → no cloud).
   */
  readonly liveTargetIsCloud = computed(() => {
    let conn = this.roleLiveConnValue();
    if (!conn) {
      const bb = this._brainBackendValue();
      if (bb === "local" || bb === "off") return false;
      conn = ""; // cloud → inherit the default provider
    }
    if (conn === "local" || conn === "off") return false;
    if (conn === "") return this.providerIsCloud();
    if (conn === "ollama") return this.ollamaIsRemote();
    return true; // claude_code | anthropic | gateway | unknown → fail safe
  });

  /**
   * Change one role's connection from the UI. Resets that role's model/effort
   * ("" = connection default — a model id is meaningless across connections)
   * and prefetches the new connection's catalog.
   *
   * COMPAT WRITE (Ask row only, REQUIRED): the Ask row is the successor of the
   * old "Assistant backend" select, so it also writes the legacy `brainBackend`
   * field — local→"local", off→"off", anything else (incl. Inherit)→"cloud".
   * Rationale: note pre-analysis + fact extraction ride `reasoner_target(Notes)`,
   * whose ONLY steering is the `brain_backend` fallback — without the compat
   * write those paths would stay permanently stuck on the user's pre-stage-4
   * value. Role keys win for Ask/Live; `brain_backend` remains the reasoner
   * fallback for Notes. Deliberately a user-driven method, NOT a valueChanges
   * subscription: load() must be able to patch role keys without clobbering a
   * legacy `brainBackend`.
   */
  setRoleConnection(role: "notes" | "ask" | "live", connection: string): void {
    switch (role) {
      case "notes":
        this.form.patchValue({
          roleNotesConnection: connection,
          roleNotesModel: "",
          roleNotesEffort: "",
        });
        break;
      case "ask":
        this.form.patchValue({
          roleAskConnection: connection,
          roleAskModel: "",
          roleAskEffort: "",
          // "" (Inherit) restores the LOADED fallback rather than forcing
          // "cloud" — a legacy local/off user who wanders to a cloud pick and
          // back must round-trip, not ratchet (lock-security stage-4 nit).
          brainBackend:
            connection === "local"
              ? "local"
              : connection === "off"
                ? "off"
                : connection === ""
                  ? this._loadedBrainBackend
                  : "cloud",
        });
        break;
      case "live":
        this.form.patchValue({
          roleLiveConnection: connection,
          roleLiveModel: "",
          roleLiveEffort: "",
        });
        break;
    }
    if (PROVIDER_CONNECTION_IDS.includes(connection)) {
      void this.ensureModels(connection);
    }
  }

  // ── Phase H — brain (AI assistant) model registry ──────────────────────

  /** The selectable local brain models (from list_brain_models). */
  private readonly _brainModels = signal<BrainModelDto[]>([]);
  readonly brainModels = this._brainModels.asReadonly();
  /** True while the model list is loading (best-effort). */
  private readonly _brainModelsLoading = signal(false);
  readonly brainModelsLoading = this._brainModelsLoading.asReadonly();
  /** Surfaced if loading / selecting / downloading a brain model rejects. */
  private readonly _brainError = signal<string | null>(null);
  readonly brainError = this._brainError.asReadonly();
  /** Model id currently downloading, or null. Drives the per-row progress UI. */
  private readonly _brainDownloadingId = signal<string | null>(null);
  readonly brainDownloadingId = this._brainDownloadingId.asReadonly();
  /** 0..1 download progress for the in-flight model (best-effort from events). */
  private readonly _brainDownloadFrac = signal(0);
  readonly brainDownloadFrac = this._brainDownloadFrac.asReadonly();
  /** Whole-percent label for the in-flight brain-model download. */
  readonly brainPct = computed(
    () => Math.round(this.brainDownloadFrac() * 100) + "%",
  );
  /** Release handle for the EVENT_BRAIN_DOWNLOAD subscription. */
  private unlistenBrainDownload: UnlistenFn | null = null;

  /**
   * Live signals of the two custom-GGUF controls — the same `valueChanges`
   * bridge as the role controls above — so `customGgufValue` re-derives when
   * either is patched (from load(), a registry pick, or the shared input).
   */
  private readonly _brainModelIdValue = toSignal(
    this.form.controls.brainModelId.valueChanges.pipe(startWith("")),
    { initialValue: "" },
  );
  private readonly _brainModelPathValue = toSignal(
    this.form.controls.brainModelPath.valueChanges.pipe(startWith("")),
    { initialValue: "" },
  );

  /**
   * The single value shown in the shared "Custom GGUF model" input: the explicit
   * custom PATH when one is set, otherwise the registry id. The input drives BOTH
   * `brainModelPath` and `brainModelId` via `setCustomGguf`, so it can't be a
   * plain `formControlName` — this computed is its `[value]`.
   */
  readonly customGgufValue = computed(
    () => this._brainModelPathValue() || this._brainModelIdValue(),
  );

  /**
   * Route a typed custom value to the RIGHT control by shape and clear the other,
   * keeping `brainModelPath` (a file path, honored verbatim) and `brainModelId`
   * (a validated registry id) mutually exclusive. A path-shaped value fills
   * `brainModelPath`; anything else fills `brainModelId`.
   */
  setCustomGguf(v: string): void {
    const isPath = looksLikeGgufPath(v);
    this.form.patchValue({
      brainModelPath: isPath ? v : "",
      brainModelId: isPath ? "" : v,
    });
  }

  // ── brain2 RAG — semantic search (embedding model + reindex) ────────────

  /**
   * Whether the on-device embedding model is present.
   * `null` = not yet checked, `true`/`false` = detected via ipc.embedModelPresent().
   */
  private readonly _embedModelPresent = signal<boolean | null>(null);
  readonly embedModelPresent = this._embedModelPresent.asReadonly();
  /** True while the embedding model is downloading — disables its button. */
  private readonly _downloadingEmbedModel = signal(false);
  readonly downloadingEmbedModel = this._downloadingEmbedModel.asReadonly();
  /** 0..1 download progress for the in-flight embed-model download. */
  private readonly _embedDownloadFrac = signal(0);
  readonly embedDownloadFrac = this._embedDownloadFrac.asReadonly();
  /** Whole-percent label for the in-flight embed-model download. */
  readonly embedPct = computed(
    () => Math.round(this.embedDownloadFrac() * 100) + "%",
  );
  /** Surfaced if ipc.downloadEmbedModel() rejects. */
  private readonly _embedDownloadError = signal<string | null>(null);
  readonly embedDownloadError = this._embedDownloadError.asReadonly();

  /** True while a reindex backfill is running — disables the button + shows progress. */
  private readonly _reindexing = signal(false);
  readonly reindexing = this._reindexing.asReadonly();
  /** 0..1 progress for the in-flight reindex backfill. */
  private readonly _reindexFrac = signal(0);
  readonly reindexFrac = this._reindexFrac.asReadonly();
  /** Whole-percent label for the in-flight reindex backfill. */
  readonly reindexPct = computed(
    () => Math.round(this.reindexFrac() * 100) + "%",
  );
  /** Last reindex outcome — drives the "model_missing" nudge / "indexed" confirmation. */
  private readonly _reindexResult = signal<ReindexResult | null>(null);
  readonly reindexResult = this._reindexResult.asReadonly();
  /** Surfaced if ipc.reindexEmbeddings() rejects. */
  private readonly _reindexError = signal<string | null>(null);
  readonly reindexError = this._reindexError.asReadonly();

  /** Release handles for the embed-download + reindex event streams. */
  private unlistenEmbedDownload: UnlistenFn | null = null;
  private unlistenReindex: UnlistenFn | null = null;

  // ── Phase D — on-device PERSON-name redaction (NER) model ──────────────

  /**
   * Whether the on-device PERSON-name NER model is present (the redaction
   * firewall additionally masks NAMES before cloud egress). `null` = not yet
   * checked. Drives the honest Privacy copy: when false the copy must NOT claim
   * names are redacted — only emails/cards/phones are by default.
   */
  private readonly _nerModelPresent = signal<boolean | null>(null);
  readonly nerModelPresent = this._nerModelPresent.asReadonly();
  /** True while the NER model is downloading — disables its button. */
  private readonly _downloadingNerModel = signal(false);
  readonly downloadingNerModel = this._downloadingNerModel.asReadonly();
  /** 0..1 download progress for the in-flight NER-model download. */
  private readonly _nerDownloadFrac = signal(0);
  readonly nerDownloadFrac = this._nerDownloadFrac.asReadonly();
  /** Whole-percent label for the in-flight NER-model download. */
  readonly nerPct = computed(
    () => Math.round(this.nerDownloadFrac() * 100) + "%",
  );
  /** Surfaced if ipc.downloadNerModel() rejects. */
  private readonly _nerDownloadError = signal<string | null>(null);
  readonly nerDownloadError = this._nerDownloadError.asReadonly();
  /** Release handle for the NER-download event stream. */
  private unlistenNerDownload: UnlistenFn | null = null;

  async load(): Promise<void> {
    try {
      const cfg = await this.ipc.getConfig();
      this.loadedOnboarded = cfg.onboarded ?? true;
      // Stage E security flags are not form-edited here — snapshot them so save()
      // round-trips them instead of letting the backend's serde defaults clobber
      // them (mcpRequireToken / cloudEgressConsented would otherwise reset to false).
      this.loadedMcpRequireToken = cfg.mcpRequireToken ?? true;
      this.loadedLockRequireBiometric = cfg.lockRequireBiometric ?? true;
      this.loadedRelockOnScreenshare = cfg.relockOnScreenshare ?? true;
      this._cloudConsented.set(cfg.cloudEgressConsented ?? false);
      // brain2 connectors — web-search consent is preserve-only (granted only via
      // consent_to_web_search); snapshot it so save() round-trips it unchanged.
      this._webConsented.set(cfg.webSearchConsented ?? false);
      this.form.patchValue({
        providerId: cfg.providerId,
        vaultPath: cfg.vaultPath ?? "",
        vaultSubfolder: cfg.vaultSubfolder ?? "",
        whisperModelPath: cfg.whisperModelPath ?? "",
        language: cfg.language ?? "",
        anthropicModel: cfg.anthropicModel,
        providerModel: cfg.providerModel ?? "",
        providerEffort: cfg.providerEffort ?? "",
        ollamaBaseUrl: cfg.ollamaBaseUrl,
        ollamaModel: cfg.ollamaModel,
        claudeBinary: cfg.claudeBinary,
        claudeCodeInheritEnv: cfg.claudeCodeInheritEnv ?? false,
        inputDevice: cfg.inputDevice ?? "",
        captureSystemAudio: cfg.captureSystemAudio ?? false,
        vadEnabled: cfg.vadEnabled ?? true,
        keepHiresMasters: cfg.keepHiresMasters ?? false,
        diarizeOthers: cfg.diarizeOthers ?? false,
        aecEnabled: cfg.aecEnabled ?? false,
        postAecEnabled: cfg.postAecEnabled ?? true,
        modelSize: cfg.modelSize ?? "large-v3",
        voiceTrigger: cfg.voiceTrigger ?? false,
        noteStyle: cfg.noteStyle ?? "standard",
        notesMode: cfg.notesMode ?? "enhance",
        autoOrganize: cfg.autoOrganize ?? false,
        noteLanguage: cfg.noteLanguage ?? "auto",
        brainBackend: cfg.brainBackend ?? "cloud",
        realtimeReactions: cfg.realtimeReactions ?? false,
        proactiveHintsEnabled: cfg.proactiveHintsEnabled ?? true,
        userMemoryEnabled: cfg.userMemoryEnabled ?? true,
        brainModelId: cfg.brainModelId ?? "",
        brainModelPath: cfg.brainModelPath ?? "",
        semanticSearchEnabled: cfg.semanticSearchEnabled ?? false,
        webSearchEnabled: cfg.webSearchEnabled ?? false,
        // AI Gateway (Phase 1) — base URL + model, default "" for pre-existing configs.
        gatewayBaseUrl: cfg.gatewayBaseUrl ?? "",
        gatewayModel: cfg.gatewayModel ?? "",
        // Stage 4 — role overrides load verbatim ("" = inherit). Deliberately NOT
        // seeded from a legacy brainBackend=local/off: materializing role keys on
        // the next save would flip Ask from the legacy "provider floor ignores
        // brain_backend" semantics to an explicit reasoner-only target (a real
        // behavior change). Inherit rows instead show an honest resolver-mirror
        // summary (assistantInheritSummary).
        roleNotesConnection: cfg.roleNotesConnection ?? "",
        roleNotesModel: cfg.roleNotesModel ?? "",
        roleNotesEffort: cfg.roleNotesEffort ?? "",
        roleAskConnection: cfg.roleAskConnection ?? "",
        roleAskModel: cfg.roleAskModel ?? "",
        roleAskEffort: cfg.roleAskEffort ?? "",
        roleLiveConnection: cfg.roleLiveConnection ?? "",
        roleLiveModel: cfg.roleLiveModel ?? "",
        roleLiveEffort: cfg.roleLiveEffort ?? "",
      });
      this._loadedBrainBackend = (cfg.brainBackend ?? "cloud") as BrainBackend;
      // Prefetch the model catalogs the loaded config already renders selects
      // for: the Default-model picker (claude_code/anthropic only — ollama/
      // gateway keep their model on the connection card, so prefetching their
      // REMOTE catalogs here would be network egress with zero UI payoff;
      // lock-security stage-4 boundary condition) and any concrete per-role
      // connection (those DO render a select). Best-effort: a failed fetch
      // leaves an empty catalog and the pickers fall back to free-text inputs.
      for (const conn of new Set(
        [
          cfg.providerId === "claude_code" || cfg.providerId === "anthropic"
            ? cfg.providerId
            : "",
          cfg.roleNotesConnection ?? "",
          cfg.roleAskConnection ?? "",
          cfg.roleLiveConnection ?? "",
        ].filter((c) => PROVIDER_CONNECTION_IDS.includes(c)),
      )) {
        void this.ensureModels(conn);
      }
      this.updateDownloadHint();
      this._inputDevices.set(await this.ipc.listInputDevices().catch(() => []));
      this._hasKey.set(await this.ipc.hasAnthropicKey());
      this._hasWebKey.set(await this.ipc.hasWebSearchKey().catch(() => false));
      this._hasGatewayKey.set(await this.ipc.hasGatewayKey().catch(() => false));
      this._modelPresent.set(await this.ipc.modelPresent());
      // Whisper transcribe-model download-progress stream (best-effort).
      await this.subscribeModelDownload();
      await this.refreshProviders();
      // Phase H — brain model registry + download-progress stream (best-effort).
      await this.subscribeBrainDownload();
      await this.refreshBrainModels();
      // brain2 RAG — embedding-model presence + reindex/download progress streams.
      await this.subscribeSemanticStreams();
      this._embedModelPresent.set(
        await this.ipc.embedModelPresent().catch(() => false),
      );
      // Phase D — PERSON-name NER (redaction) model presence + download stream.
      await this.subscribeNerDownload();
      this._nerModelPresent.set(
        await this.ipc.nerModelPresent().catch(() => false),
      );
      // About section — product identity (best-effort; null leaves a "loading" line).
      this._appInfo.set(await this.ipc.appInfo().catch(() => null));
    } catch (e) {
      this._loadError.set(String(e));
    }
  }

  /**
   * Subscribe ONCE to the Whisper model-download progress stream and store the
   * unlisten so DestroyRef can release it (no leaked listener). Best-effort: a
   * missing backend stream just leaves the progress bar inert (the download still
   * resolves via the command promise).
   */
  private async subscribeModelDownload(): Promise<void> {
    try {
      this.unlistenModelDownload = await this.ipc.onModelDownload((p) => {
        // Only meaningful while a download this component started is in-flight.
        if (!this.downloadingModel()) return;
        if (p.total && p.total > 0) {
          this._modelDownloadFrac.set(Math.min(1, p.downloaded / p.total));
        }
        if (p.done) this._modelDownloadFrac.set(1);
      });
      this.destroyRef.onDestroy(() => this.unlistenModelDownload?.());
    } catch {
      // No model-download stream available — progress stays inert.
    }
  }

  /**
   * Subscribe ONCE to the brain-download progress stream and store the unlisten
   * so DestroyRef can release it (no leaked listener). Best-effort: a missing
   * backend command just leaves the progress bar inert.
   */
  private async subscribeBrainDownload(): Promise<void> {
    try {
      this.unlistenBrainDownload = await this.ipc.onBrainDownload((p) => {
        // The backend emits one download at a time and the component already
        // tracks which model it started (brainDownloadingId), so every progress
        // event applies to it. (Download errors surface via the command promise.)
        if (this.brainDownloadingId() === null) return;
        if (p.total && p.total > 0) {
          this._brainDownloadFrac.set(Math.min(1, p.downloaded / p.total));
        }
        if (p.done) {
          this._brainDownloadingId.set(null);
          void this.refreshBrainModels();
        }
      });
      this.destroyRef.onDestroy(() => this.unlistenBrainDownload?.());
    } catch {
      // No brain-download stream available — progress stays inert; downloads
      // still resolve via the command promise.
    }
  }

  /** Reload the brain model registry (downloaded / fits-RAM / selected state). */
  async refreshBrainModels(): Promise<void> {
    this._brainModelsLoading.set(true);
    this._brainError.set(null);
    try {
      this._brainModels.set(await this.ipc.listBrainModels());
    } catch (e) {
      this._brainError.set(String(e));
    } finally {
      this._brainModelsLoading.set(false);
    }
  }

  /** Make a registry model the active local brain model, then refresh the list. */
  async useBrainModel(id: string): Promise<void> {
    this._brainError.set(null);
    try {
      await this.ipc.selectBrainModel(id);
      // Clear any custom PATH: it wins over the id in resolve_brain_model, so a
      // stale path would silently override the registry model just picked.
      this.form.patchValue({ brainModelId: id, brainModelPath: "" });
      await this.refreshBrainModels();
    } catch (e) {
      this._brainError.set(String(e));
    }
  }

  /**
   * Download a registry model. The promise resolves on completion; live
   * progress (when available) rides the EVENT_BRAIN_DOWNLOAD stream.
   */
  async downloadBrainModel(id: string): Promise<void> {
    this._brainError.set(null);
    this._brainDownloadFrac.set(0);
    this._brainDownloadingId.set(id);
    try {
      await this.ipc.downloadBrainModel(id);
      await this.refreshBrainModels();
    } catch (e) {
      this._brainError.set(String(e));
    } finally {
      this._brainDownloadingId.set(null);
    }
  }

  // ── brain2 RAG — semantic search (embedding model + reindex backfill) ───

  /**
   * Subscribe ONCE to the embed-download + reindex progress streams and store the
   * unlisten handles so DestroyRef can release them (no leaked listeners).
   * Best-effort: a missing backend stream just leaves the relevant bar inert.
   */
  private async subscribeSemanticStreams(): Promise<void> {
    try {
      this.unlistenEmbedDownload = await this.ipc.onEmbedDownload((p) => {
        // Per-file progress: blend the completed files + the current file's fraction
        // across the whole set so the single bar advances smoothly.
        if (p.fileCount > 0) {
          const cur = p.total && p.total > 0 ? p.downloaded / p.total : 0;
          this._embedDownloadFrac.set(
            Math.min(1, (p.fileIndex + cur) / p.fileCount),
          );
        }
        if (p.done) this._embedDownloadFrac.set(1);
      });
      this.unlistenReindex = await this.ipc.onReindex((p) => {
        if (p.total > 0) {
          this._reindexFrac.set(Math.min(1, p.done / p.total));
        }
      });
      this.destroyRef.onDestroy(() => {
        this.unlistenEmbedDownload?.();
        this.unlistenReindex?.();
      });
    } catch {
      // No stream available — progress bars stay inert; commands still resolve.
    }
  }

  /** Download the on-device embedding model, then re-check presence. */
  async downloadEmbedModel(): Promise<void> {
    this._embedDownloadError.set(null);
    this._embedDownloadFrac.set(0);
    this._downloadingEmbedModel.set(true);
    try {
      await this.ipc.downloadEmbedModel();
      this._embedModelPresent.set(await this.ipc.embedModelPresent());
    } catch (e) {
      this._embedDownloadError.set(String(e));
    } finally {
      this._downloadingEmbedModel.set(false);
    }
  }

  // ── Phase D — PERSON-name NER (redaction) model ────────────────────────

  /**
   * Subscribe ONCE to the NER-download progress stream and store the unlisten so
   * DestroyRef can release it (no leaked listener). Best-effort: a missing
   * backend stream just leaves the progress bar inert (the download still
   * resolves via the command promise). Mirrors {@link subscribeSemanticStreams}.
   */
  private async subscribeNerDownload(): Promise<void> {
    try {
      this.unlistenNerDownload = await this.ipc.onNerDownload((p) => {
        // Per-file progress: blend the completed files + the current file's
        // fraction across the whole set so the single bar advances smoothly.
        if (p.fileCount > 0) {
          const cur = p.total && p.total > 0 ? p.downloaded / p.total : 0;
          this._nerDownloadFrac.set(
            Math.min(1, (p.fileIndex + cur) / p.fileCount),
          );
        }
        if (p.done) this._nerDownloadFrac.set(1);
      });
      this.destroyRef.onDestroy(() => this.unlistenNerDownload?.());
    } catch {
      // No stream available — progress bar stays inert; the command still resolves.
    }
  }

  /** Download the on-device PERSON-name NER model, then re-check presence. */
  async downloadNerModel(): Promise<void> {
    this._nerDownloadError.set(null);
    this._nerDownloadFrac.set(0);
    this._downloadingNerModel.set(true);
    try {
      await this.ipc.downloadNerModel();
      this._nerModelPresent.set(await this.ipc.nerModelPresent());
    } catch (e) {
      this._nerDownloadError.set(String(e));
    } finally {
      this._downloadingNerModel.set(false);
    }
  }

  /**
   * Backfill the semantic vector index over all visible meetings. A
   * `"model_missing"` result means the e5 model isn't installed yet — surfaced as
   * a nudge to download it first (no indexing was attempted).
   */
  async reindexEmbeddings(): Promise<void> {
    this._reindexError.set(null);
    this._reindexResult.set(null);
    this._reindexFrac.set(0);
    this._reindexing.set(true);
    try {
      const res = await this.ipc.reindexEmbeddings();
      this._reindexResult.set(res);
      // The model could have been (un)installed between the presence probe and now.
      if (res.status === "model_missing") this._embedModelPresent.set(false);
    } catch (e) {
      this._reindexError.set(String(e));
    } finally {
      this._reindexing.set(false);
    }
  }

  async pickVault(): Promise<void> {
    const dir = await open({ directory: true, multiple: false });
    if (typeof dir === "string") this.form.patchValue({ vaultPath: dir });
  }

  async pickModel(): Promise<void> {
    const file = await open({ directory: false, multiple: false });
    if (typeof file === "string")
      this.form.patchValue({ whisperModelPath: file });
  }

  async save(): Promise<void> {
    const v = this.form.getRawValue();
    const cfg: AppConfigDto = {
      providerId: v.providerId,
      vaultPath: v.vaultPath || null,
      vaultSubfolder: v.vaultSubfolder || null,
      whisperModelPath: v.whisperModelPath || null,
      language: v.language || null,
      anthropicModel: v.anthropicModel,
      providerModel: v.providerModel,
      providerEffort: v.providerEffort,
      ollamaBaseUrl: v.ollamaBaseUrl,
      ollamaModel: v.ollamaModel,
      claudeBinary: v.claudeBinary,
      inputDevice: v.inputDevice || null,
      captureSystemAudio: v.captureSystemAudio,
      vadEnabled: v.vadEnabled,
      keepHiresMasters: v.keepHiresMasters,
      diarizeOthers: v.diarizeOthers,
      aecEnabled: v.aecEnabled,
      postAecEnabled: v.postAecEnabled,
      modelSize: v.modelSize,
      voiceTrigger: v.voiceTrigger,
      onboarded: this.loadedOnboarded,
      noteStyle: v.noteStyle,
      notesMode: v.notesMode,
      autoOrganize: v.autoOrganize,
      noteLanguage: v.noteLanguage,
      // Phase H — brain / in-meeting voice assistant.
      brainBackend: v.brainBackend,
      realtimeReactions: v.realtimeReactions,
      // Proactive brain hints — round-tripped so a save preserves the mute.
      proactiveHintsEnabled: v.proactiveHintsEnabled,
      // Cross-meeting user memory — round-tripped so a save preserves the choice.
      userMemoryEnabled: v.userMemoryEnabled,
      brainModelId: v.brainModelId || null,
      // A custom GGUF file path (settable; wins over brainModelId in the resolver).
      brainModelPath: v.brainModelPath || null,
      // brain2 RAG — semantic-search master flag (round-tripped so a save preserves it).
      semanticSearchEnabled: v.semanticSearchEnabled,
      // brain2 connectors — web-search toggle is settable from the form; its consent
      // is PRESERVE-ONLY (granted via allowWebSearch's dedicated command), so a save
      // just carries the current value back instead of letting the backend default it.
      webSearchEnabled: v.webSearchEnabled,
      webSearchConsented: this.webConsented(),
      // Round-trip the Stage E security flags so a settings save never silently
      // resets them. Cloud-egress consent is GRANTED only via the dedicated
      // command (allowCloudProcessing) — here we just carry the current value back.
      mcpRequireToken: this.loadedMcpRequireToken,
      lockRequireBiometric: this.loadedLockRequireBiometric,
      relockOnScreenshare: this.loadedRelockOnScreenshare,
      cloudEgressConsented: this.cloudConsented(),
      // Opt-in: pass the shell env to the `claude` CLI (restores env ANTHROPIC_API_KEY auth).
      claudeCodeInheritEnv: v.claudeCodeInheritEnv,
      // AI Gateway (Phase 1) — base URL + model, round-tripped so a settings save preserves them.
      gatewayBaseUrl: v.gatewayBaseUrl,
      gatewayModel: v.gatewayModel,
      // Stage 4 — ALL NINE role keys ride every save. They are settable by
      // design ("" = inherit), and always including them makes the stage-3
      // review's "an FE payload without the keys clears an override" concern
      // moot going forward.
      roleNotesConnection: v.roleNotesConnection,
      roleNotesModel: v.roleNotesModel,
      roleNotesEffort: v.roleNotesEffort,
      roleAskConnection: v.roleAskConnection,
      roleAskModel: v.roleAskModel,
      roleAskEffort: v.roleAskEffort,
      roleLiveConnection: v.roleLiveConnection,
      roleLiveModel: v.roleLiveModel,
      roleLiveEffort: v.roleLiveEffort,
    };
    try {
      await this.ipc.saveConfig(cfg);
      this._saved.set(true);
    } catch (e) {
      this._loadError.set("Save failed: " + String(e));
    }
  }

  async saveKey(): Promise<void> {
    const key = this.keyControl.value;
    if (!key) return;
    await this.ipc.setAnthropicKey(key);
    this.keyControl.setValue("");
    this._hasKey.set(await this.ipc.hasAnthropicKey());
  }

  /** Re-open the first-run wizard. Existing settings are preserved and prefilled. */
  rerunOnboarding(): void {
    void this.router.navigate(["/onboarding"]);
  }

  async refreshProviders(): Promise<void> {
    this._providers.set(await this.ipc.providerStatuses());
  }

  /**
   * E10 — grant the one-time cloud-egress consent via the dedicated command (an
   * explicit, auditable user act — NOT a side effect of a normal settings save).
   * After it resolves, cloud providers (Claude Code / Anthropic) can summarize, so
   * we re-probe provider availability. save() simply carries the current value
   * back so it isn't cleared; the only way to clear it is `revokeCloudProcessing`.
   */
  async allowCloudProcessing(): Promise<void> {
    this._consentError.set(null);
    this._consenting.set(true);
    try {
      await this.ipc.consentToCloudEgress();
      this._cloudConsented.set(true);
      await this.refreshProviders();
    } catch (e) {
      this._consentError.set(String(e));
    } finally {
      this._consenting.set(false);
    }
  }

  /**
   * Stage 2 — revoke the cloud-egress consent via the dedicated command (the
   * explicit inverse of `allowCloudProcessing`; NOT a side effect of a normal
   * save). On success the consent signal flips false — so save() round-trips
   * the revoked state — and providers are re-probed since cloud ones now
   * fail closed.
   */
  async revokeCloudProcessing(): Promise<void> {
    this._revokeError.set(null);
    this._revoking.set(true);
    try {
      await this.ipc.revokeCloudEgress();
      this._cloudConsented.set(false);
      await this.refreshProviders();
    } catch (e) {
      this._revokeError.set(String(e));
    } finally {
      this._revoking.set(false);
    }
  }

  /**
   * brain2 connectors — store/replace the BYO Brave Search API key in the
   * Keychain, then re-probe presence so the "Key set ✓" pill flips. The value is
   * cleared from the input after saving (it's never shown back). Mirrors saveKey().
   */
  async saveWebKey(): Promise<void> {
    const key = this.webKeyControl.value;
    if (!key.trim()) return;
    this._webKeyError.set(null);
    this._savingWebKey.set(true);
    try {
      await this.ipc.setWebSearchApiKey(key);
      this.webKeyControl.setValue("");
      this._hasWebKey.set(await this.ipc.hasWebSearchKey());
    } catch (e) {
      this._webKeyError.set(String(e));
    } finally {
      this._savingWebKey.set(false);
    }
  }

  /**
   * brain2 connectors — grant the one-time web-search egress consent via the
   * dedicated command (an explicit, auditable user act — NOT a side effect of a
   * normal settings save). After it resolves the brain may expose the web
   * connector (when web search is enabled AND a key is stored). There is no FE
   * "revoke": save() simply carries the current value back so it isn't cleared.
   * Mirrors allowCloudProcessing().
   */
  async allowWebSearch(): Promise<void> {
    this._webConsentError.set(null);
    this._webConsenting.set(true);
    try {
      await this.ipc.consentToWebSearch();
      this._webConsented.set(true);
    } catch (e) {
      this._webConsentError.set(String(e));
    } finally {
      this._webConsenting.set(false);
    }
  }

  /**
   * AI Gateway (Phase 1) — store/replace the gateway API key in Keychain, then
   * re-probe presence so the pill flips. The value is cleared from the input after
   * saving (it's never shown back). Mirrors saveKey() / saveWebKey().
   */
  async saveGatewayKey(): Promise<void> {
    const key = this.gatewayKeyControl.value;
    if (!key.trim()) return;
    this._gatewayKeyError.set(null);
    try {
      await this.ipc.setGatewayKey(key);
      this.gatewayKeyControl.setValue("");
      this._hasGatewayKey.set(await this.ipc.hasGatewayKey());
    } catch (e) {
      this._gatewayKeyError.set(String(e));
    }
  }

  /**
   * AI Gateway (Phase 1) — remove the stored gateway API key from Keychain.
   * Updates the pill afterward. No-op when no key is stored.
   */
  async removeGatewayKey(): Promise<void> {
    this._gatewayKeyError.set(null);
    try {
      await this.ipc.clearGatewayKey();
      this._hasGatewayKey.set(await this.ipc.hasGatewayKey());
    } catch (e) {
      this._gatewayKeyError.set(String(e));
    }
  }

  /**
   * AI Gateway (Phase 3) — fetch the model catalog from the configured gateway's
   * `/v1/models` endpoint and populate the model picker. Leaves the list empty on
   * error so the text-input fallback is shown instead — the user can still type the
   * model id manually. Not an effect: driven by the explicit "↻ Refresh models"
   * button click (no NG0600 risk, no unwanted network call on load).
   */
  async refreshGatewayModels(): Promise<void> {
    this._gatewayModelError.set(null);
    this._gatewayModelsLoading.set(true);
    try {
      this._gatewayModels.set(await this.ipc.listGatewayModels());
    } catch (e) {
      // Leave the existing list (may be empty) and show the fallback hint.
      this._gatewayModels.set([]);
      this._gatewayModelError.set(String(e));
    } finally {
      this._gatewayModelsLoading.set(false);
    }
  }

  /**
   * AI Gateway (Phase 4) — probe the configured gateway and update the health
   * indicator. Driven by the explicit "Check" button click (no NG0600 risk, no
   * unwanted network call on load). The backend never errors on this command but
   * we catch for safety.
   */
  async checkGatewayHealth(): Promise<void> {
    this._gatewayHealthChecking.set(true);
    try {
      this._gatewayHealth.set(
        await this.ipc
          .gatewayHealth()
          .catch(() => ({ reachable: false, modelCount: 0 })),
      );
    } finally {
      this._gatewayHealthChecking.set(false);
    }
  }

  /** Persist the chosen language + quality, then re-check which model is present. */
  async onModelChoiceChange(): Promise<void> {
    this.updateDownloadHint();
    await this.save();
    this._modelPresent.set(await this.ipc.modelPresent());
  }

  private updateDownloadHint(): void {
    const hints: Record<string, string> = {
      tiny: "~75 MB",
      base: "~150 MB",
      small: "~470 MB",
      medium: "~1.5 GB",
      "large-v3-turbo": "~1.6 GB",
      "large-v3": "~3 GB",
    };
    this._downloadHint.set(hints[this.form.getRawValue().modelSize] ?? "");
  }

  /**
   * Copy the Obsidian URL to the clipboard and briefly confirm.
   * No <a href> — opening an external URL would navigate the webview away.
   */
  async copyObsidianUrl(): Promise<void> {
    try {
      await navigator.clipboard.writeText(this.obsidianUrl);
      this._urlCopied.set(true);
      if (this.copyResetTimer) clearTimeout(this.copyResetTimer);
      this.copyResetTimer = setTimeout(() => this._urlCopied.set(false), 1600);
      this.destroyRef.onDestroy(() => {
        if (this.copyResetTimer) clearTimeout(this.copyResetTimer);
      });
    } catch {
      // Clipboard unavailable — the URL stays visible and selectable as a fallback.
    }
  }

  /**
   * Copy the MCP server config JSON to the clipboard and briefly confirm.
   * The <pre> block stays selectable as a fallback if the clipboard is blocked.
   */
  async copyMcpConfig(): Promise<void> {
    try {
      await navigator.clipboard.writeText(this.mcpConfig);
      this._configCopied.set(true);
      if (this.mcpCopyResetTimer) clearTimeout(this.mcpCopyResetTimer);
      this.mcpCopyResetTimer = setTimeout(
        () => this._configCopied.set(false),
        1600,
      );
      this.destroyRef.onDestroy(() => {
        if (this.mcpCopyResetTimer) clearTimeout(this.mcpCopyResetTimer);
      });
    } catch {
      // Clipboard unavailable — the config stays visible and selectable as a fallback.
    }
  }

  /** Download the model for the chosen language + quality, then re-check presence. */
  async downloadModel(): Promise<void> {
    this._modelDownloadError.set(null);
    this._modelDownloadFrac.set(0);
    this._downloadingModel.set(true);
    try {
      await this.save(); // ensure the chosen language + size are persisted first
      await this.ipc.downloadModel();
      this._modelPresent.set(await this.ipc.modelPresent());
    } catch (e) {
      this._modelDownloadError.set(String(e));
    } finally {
      this._downloadingModel.set(false);
    }
  }
}
