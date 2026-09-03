import { DestroyRef, Injectable, computed, inject, signal } from "@angular/core";
import { takeUntilDestroyed, toSignal } from "@angular/core/rxjs-interop";
import { FormBuilder, FormControl } from "@angular/forms";
import { debounceTime, startWith } from "rxjs";
import { Router } from "@angular/router";
import { open } from "@tauri-apps/plugin-dialog";
import { IpcService } from "../../core/ipc.service";
import { hostIsLoopback } from "../../core/loopback";
import { modelSizeLabel } from "../../core/model-bytes";
// P3: the connection labels used to be declared here AND in `record.component.ts` AND in
// `summarize/roles.rs` — three copies of four strings. They now live in the ONE copy module, which
// is the documented mirror of the Rust half.
import { CONNECTION_LABELS } from "../../core/copy/labels";
import { MachineService } from "../../services/machine.service";
import {
  connectionConsumesRoleModel,
  connectionKeepsModelId,
  effectiveConnection,
} from "./model-id";
import type {
  AiMapRow,
  AppConfigDto,
  AppInfo,
  BrainBackend,
  BrainModelDto,
  GatewayHealth,
  GatewayModel,
  InputDeviceInfo,
  ModelClass,
  NoteTemplate,
  NoteTemplateSection,
  Posture,
  ProviderStatus,
  ReindexResult,
  RetiredModelNudge,
  StorageReport,
  VoiceprintInfo,
  ModelCatalog,
  McpStatus,
} from "../../core/models";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { NOTE_ASSIST_NEW_ACTION_IDS } from "../notes/note-brain-popover/note-assist-catalog";
import { ErrorCopyService } from "../../core/copy/error-copy.service";

/**
 * The provider-backed connection ids — the ones `list_models` serves a
 * catalog for and a per-role model select makes sense on. `local`/`off` are
 * reasoner-only targets (no SummarizerProvider, no per-role model select — the
 * local model is the global GGUF registry selection).
 */
/**
 * The connections whose catalog is FETCHED from a real endpoint. A static property of the
 * connection, not of any request.
 *
 * Refresh visibility was derived from the loaded catalog twice, and both directions were wrong:
 * `source === "live"` hid the button after a failed fetch — the one moment retrying is the point —
 * and `source !== "bundled"` showed it for Claude/Codex/Anthropic before their first response, and
 * permanently if that response failed. Whether an arm performs a live fetch does not depend on
 * whether a fetch has happened yet, so it must not be read from the catalog at all.
 */
const LIVE_CATALOG_CONNECTION_IDS: readonly string[] = ["ollama", "gateway"];

const PROVIDER_CONNECTION_IDS: readonly string[] = [
  "claude_code",
  "codex_cli",
  "anthropic",
  "ollama",
  "gateway",
];
// There is deliberately NO "curated, therefore authoritative" list here any more. A bundled
// catalog is a HINT: it exists so the picker has something to show, and a model id absent from it
// is a custom id, not corruption. The previous `CURATED_PROVIDER_CONNECTION_IDS` +
// `repairForeignRoleModels` pair cleared-and-persisted any unrecognised id, so every newly
// released model was not merely missing from the dropdown — it was actively erased from config
// the moment the catalog loaded. `ModelCatalog.source` now tells the UI which catalogs are live and
// which are bundled, which is what that distinction was really for.

/**
 * Coerce a form value (a number, a numeric string, or blank/null after clearing a
 * `<input type="number">`) to a finite POSITIVE integer, falling back to `fallback`
 * for anything blank/non-finite/≤ 0. Used for the brain-sidecar timeout fields so a
 * cleared/invalid control never persists a pathological 0-second window (the backend
 * re-defaults 0 too — belt and suspenders).
 */
function coercePositive(value: unknown, fallback: number): number {
  const n = Math.floor(Number(value));
  return Number.isFinite(n) && n > 0 ? n : fallback;
}

/**
 * Synthetic id for the built-in on-device engine (the brain-engine-card) in the
 * `inUseConnections` set — it is not a provider CONNECTION, so it needs its own
 * token to be markable "In use now" alongside the real connection ids.
 */
export const BRAIN_ENGINE_ID = "__brain__";

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
  private readonly errorCopy = inject(ErrorCopyService);
  /**
   * The ROOT-held whisper catalog + machine answer (P1). Root-scoped, so the store
   * reads the same cached snapshot the picker paints — never a second copy that can
   * disagree with it.
   */
  private readonly machine = inject(MachineService);

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
    // Local analysis default ON; voiceprints remain an independent privacy opt-in.
    diarizeOthers: true,
    // Speaker voiceprints — cross-meeting on-device re-identification. Opt-in, default off.
    voiceprintEnabled: false,
    aecEnabled: false,
    postAecEnabled: true,
    // Recording-storage cap (GB, string for an empty = "no cap") + opt-in auto-prune.
    // COMMITS ON BLUR: with auto-save, per-keystroke commits would persist
    // intermediate values ("1" while typing "15") — and a lowered cap can
    // trigger an in-session auto-prune. Blur = the typed value is final.
    audioStorageLimitGb: this.fb.nonNullable.control("", { updateOn: "blur" }),
    audioAutoPrune: false,
    modelSize: "large-v3",
    // OPTIONAL live-caption engine: "whisper" (default) | "parakeet".
    liveAsrEngine: "whisper",
    // Brain-sidecar lifecycle timeouts (seconds). COMMIT ON BLUR (like the storage cap): a
    // per-keystroke commit would persist intermediate values ("3" while typing "30"). The 0-guard
    // in the backend's `dto_to_config` falls a 0/blank back to the default, never a zero window.
    brainIdleTimeoutSecs: this.fb.nonNullable.control(300, { updateOn: "blur" }),
    brainReadyTimeoutSecs: this.fb.nonNullable.control(90, { updateOn: "blur" }),
    brainHardCapSecs: this.fb.nonNullable.control(180, { updateOn: "blur" }),
    voiceTrigger: false,
    noteStyle: "standard",
    notesMode: "enhance",
    autoOrganize: false,
    noteLanguage: "auto",
    groundSummary: true,
    // Workspace glossary is a larger text field: commit on blur so auto-save never writes every
    // intermediate keystroke. Empty is intentional and clears the backend value.
    glossary: this.fb.nonNullable.control("", { updateOn: "blur" }),
    // Phase H — brain / in-meeting voice assistant.
    brainBackend: "cloud" as BrainBackend,
    realtimeReactions: false,
    // Proactive brain (P2) — zero-egress recall cards while recording; default ON.
    proactiveHintsEnabled: true,
    // Cross-meeting USER MEMORY master gate; default ON. Off turns memory off entirely (backend).
    userMemoryEnabled: true,
    updateCheckEnabled: true,
    /** Selected registry brain-model id. Empty → null on save. */
    brainModelId: "",
    /** Explicit custom GGUF file PATH (wins over brainModelId). Empty → null on save. */
    brainModelPath: "",
    // brain2 RAG — semantic-search master flag (round-tripped on save).
    semanticSearchEnabled: false,
    // brain2 connectors — web-search master toggle (NEW EGRESS; round-tripped).
    webSearchEnabled: false,
    // brain2 connectors (Phase 2) — Jira master toggle (NEW EGRESS) + non-secret
    // base URL / email, round-tripped on save.
    jiraEnabled: false,
    jiraBaseUrl: "",
    jiraEmail: "",
    // brain2 connectors (Phase 3) — Slack master toggle (NEW EGRESS; round-tripped).
    slackEnabled: false,
    // brain2 connectors — Notion + ClickUp READ connectors (NEW EGRESS; round-tripped)
    // plus ClickUp's non-secret workspace ("team") id.
    notionEnabled: false,
    clickupEnabled: false,
    clickupTeamId: "",
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
    // Notes feature — the three LEGACY in-note selection-assistant actions. All
    // default TRUE and round-tripped on every save so a settings save never
    // silently disables one (mirrors the proactiveHintsEnabled/userMemoryEnabled
    // pattern). The 16 NEWER actions each get a boolean control too (all default
    // TRUE = enabled); load()/save() convert those booleans ↔ the single
    // `noteAssistActionsOff: string[]` config field (an action is OFF ⇒ its id is
    // in the list). Keeping one bool PER new action lets `mur-toggle
    // formControlName` bind directly and ride the existing auto-save, while the
    // DTO stays a single scalable field (contract).
    noteAssistRefine: true,
    noteAssistShorten: true,
    noteAssistEnhance: true,
    // NEW actions (control name === action id). TRUE = enabled.
    grammar: true,
    expand: true,
    simplify: true,
    tone: true,
    translate: true,
    bullets: true,
    table: true,
    keypoints: true,
    find_related: true,
    link_entities: true,
    fact_check: true,
    ask: true,
    action_items: true,
    decisions: true,
    draft_followup: true,
    spinoff_note: true,
  });
  readonly keyControl = new FormControl("", { nonNullable: true });
  /** BYO Brave Search API key input (web-search connector). Cleared after save. */
  readonly webKeyControl = new FormControl("", { nonNullable: true });
  /** BYO Jira API token input (Jira connector). Cleared after save. */
  readonly jiraTokenControl = new FormControl("", { nonNullable: true });
  /** BYO Slack user token input (Slack connector). Cleared after save. */
  readonly slackTokenControl = new FormControl("", { nonNullable: true });
  /** BYO Notion integration token input (Notion connector). Cleared after save. */
  readonly notionTokenControl = new FormControl("", { nonNullable: true });
  /** BYO ClickUp personal API token input (ClickUp connector). Cleared after save. */
  readonly clickupTokenControl = new FormControl("", { nonNullable: true });

  /**
   * PROTOTYPE auto-save (no Save button): every committed form change persists
   * after a short debounce. TWO gates:
   * - `autoSaveReady` — armed only by a SUCCESSFUL load(), so a failed load's
   *   pristine defaults can never be written over the user's stored config;
   * - `form.dirty` — UI interactions mark controls dirty, programmatic
   *   patchValue (the load() seeding storm) does not, so opening Settings
   *   never writes. Store methods that patch the form ON THE USER'S BEHALF
   *   (role/posture/model picks, vault pickers) call markAsDirty() so their
   *   change autosaves like any direct edit. Never reset to pristine — dirty
   *   simply means "the user has touched settings this session".
   * The subscription performs an action (save); all state stays in signals.
   */
  private autoSaveReady = false;
  /** True while a form change awaits its debounced save (drives the destroy flush). */
  private autoSavePending = false;
  private readonly _autoSaveMark = this.form.valueChanges
    .pipe(takeUntilDestroyed())
    .subscribe(() => {
      this.autoSavePending = true;
    });
  private readonly _autoSave = this.form.valueChanges
    .pipe(debounceTime(500), takeUntilDestroyed())
    .subscribe(() => {
      this.autoSavePending = false;
      if (!(this.autoSaveReady && this.form.dirty)) return;
      // A synchronous throw here would KILL the subscription and silently end
      // auto-save for the session — contain it and surface it in the banner.
      try {
        void this.save();
      } catch (e) {
        this._loadError.set(this.errorCopy.because("Save failed", e));
      }
    });

  constructor() {
    // Leaving /settings destroys this store (component-provided) — flush a
    // change still sitting in the 500ms debounce window instead of dropping
    // it (adversarial-verify finding: toggle → ⌘N within 500ms lost the edit).
    this.destroyRef.onDestroy(() => {
      if (this.autoSavePending && this.autoSaveReady && this.form.dirty) {
        this.autoSavePending = false;
        try {
          void this.save(); // fire-and-forget; the IPC completes after teardown
        } catch {
          // best-effort flush — nowhere left to surface an error
        }
      }
    });
  }

  private readonly _providers = signal<ProviderStatus[]>([]);
  readonly providers = this._providers.asReadonly();
  /** Available mic input devices for the picker (loaded best-effort in load()). */
  private readonly _inputDevices = signal<InputDeviceInfo[]>([]);
  readonly inputDevices = this._inputDevices.asReadonly();
  /**
   * Stored speaker voiceprints for the management list (opt-in voice biometrics).
   * GATED backend — a sealed-not-unlocked meeting's row is excluded. Populated
   * best-effort in load(); refreshed after a forget/clear. NEVER carries the raw
   * embedding (only label + provenance + dim).
   */
  private readonly _voiceprints = signal<VoiceprintInfo[]>([]);
  readonly voiceprints = this._voiceprints.asReadonly();
  /** True while a voiceprint forget/clear IPC call is in flight (debounces clicks). */
  private readonly _voiceprintBusy = signal(false);
  readonly voiceprintBusy = this._voiceprintBusy.asReadonly();

  // ── Recording storage — usage report + manual free-up (opt-in cap) ──────

  /** Live disk-usage report for the recordings dir (best-effort; null before first load). */
  private readonly _storageReport = signal<StorageReport | null>(null);
  readonly storageReport = this._storageReport.asReadonly();
  /** True while a manual "Free up space" prune is in flight (debounces the button). */
  private readonly _storageBusy = signal(false);
  readonly storageBusy = this._storageBusy.asReadonly();
  /** Bytes freed by the last manual "Free up space" (for a confirmation line). */
  private readonly _lastFreed = signal<number | null>(null);
  readonly lastFreed = this._lastFreed.asReadonly();

  /** Refresh the recording-storage usage report (best-effort; a failure leaves it null). */
  async loadStorageReport(): Promise<void> {
    this._storageReport.set(await this.ipc.getStorageReport().catch(() => null));
  }

  /** Manual prune to the cap NOW (no-op with no cap set), then refresh the report. */
  async freeUpSpace(): Promise<void> {
    this._storageBusy.set(true);
    this._lastFreed.set(null);
    try {
      const s = await this.ipc.freeUpSpace();
      this._lastFreed.set(s.freedBytes);
      await this.loadStorageReport();
    } finally {
      this._storageBusy.set(false);
    }
  }

  /** Reveal the recordings folder in Finder (best-effort; fire-and-forget). */
  revealAudioDir(): void {
    void this.ipc.revealAudioDir();
  }

  // ── Note templates (user-authored named sections) ───────────────────────

  /** The user's saved note templates (newest first). Loaded in load(); refreshed on save/delete. */
  private readonly _noteTemplates = signal<NoteTemplate[]>([]);
  readonly noteTemplates = this._noteTemplates.asReadonly();
  /** True while a note-template save/delete IPC call is in flight (debounces the buttons). */
  private readonly _noteTemplateBusy = signal(false);
  readonly noteTemplateBusy = this._noteTemplateBusy.asReadonly();
  /** Surfaced if a save/delete rejects — notably the backend's scripting-token rejection. */
  private readonly _noteTemplateError = signal<string | null>(null);
  readonly noteTemplateError = this._noteTemplateError.asReadonly();

  /** Refresh the saved note-template list (best-effort; a failure or non-array leaves it empty). */
  async loadNoteTemplates(): Promise<void> {
    const list = await this.ipc.listNoteTemplates().catch(() => []);
    this._noteTemplates.set(Array.isArray(list) ? list : []);
  }

  /**
   * Create or replace a note template, then refresh the list. Returns the stored row, or null on
   * rejection (the error — e.g. a forbidden scripting token — is surfaced in `noteTemplateError`).
   * Deliberately NOT tied to the settings auto-save: templates are their own persisted rows, not a
   * field of AppConfig.
   */
  async saveNoteTemplate(draft: {
    id: string | null;
    name: string;
    tone: string;
    sections: NoteTemplateSection[];
    extraFrontmatterKeys: string[];
  }): Promise<NoteTemplate | null> {
    this._noteTemplateBusy.set(true);
    this._noteTemplateError.set(null);
    try {
      const saved = await this.ipc.saveNoteTemplate(
        draft.id,
        draft.name,
        draft.tone,
        draft.sections,
        draft.extraFrontmatterKeys,
      );
      await this.loadNoteTemplates();
      return saved;
    } catch (e) {
      this._noteTemplateError.set(this.errorCopy.humanize(e));
      return null;
    } finally {
      this._noteTemplateBusy.set(false);
    }
  }

  /** Delete a saved note template; if it was the selected note-style, fall back to "standard". */
  async deleteNoteTemplate(id: string): Promise<void> {
    this._noteTemplateBusy.set(true);
    this._noteTemplateError.set(null);
    try {
      await this.ipc.deleteNoteTemplate(id);
      if (this.form.controls.noteStyle.value === id) {
        this.form.patchValue({ noteStyle: "standard" });
        this.form.markAsDirty(); // user-driven reset → auto-save persists it
      }
      await this.loadNoteTemplates();
    } catch (e) {
      this._noteTemplateError.set(this.errorCopy.humanize(e));
    } finally {
      this._noteTemplateBusy.set(false);
    }
  }

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

  /**
   * Exact JSON to drop into the Claude Code config — fetched from the backend
   * (`get_mcp_config`) because it carries the private bearer token needed for the
   * MCP handshake when `mcp_require_token` is on (the default). Populated by
   * `load()`; empty until then. `copyMcpConfig()` copies whatever is in the signal.
   */
  private readonly _mcpConfig = signal<string>("");
  readonly mcpConfig = this._mcpConfig.asReadonly();

  /**
   * Whether the local server actually came up. Starts optimistic ONLY until the first read
   * resolves — `refreshMcpStatus()` runs in `load()` alongside the config fetch.
   */
  private readonly _mcpStatus = signal<McpStatus>({
    state: "starting",
    port: 8765,
  });
  readonly mcpStatus = this._mcpStatus.asReadonly();
  /** True while the server is genuinely serving — the ONLY state the healthy copy may claim. */
  readonly mcpRunning = computed(() => this._mcpStatus().state === "listening");
  /**
   * True when another process on this Mac holds the port. Its own branch because the listener
   * retries this case on its own, so the copy can tell the user to just quit the other app —
   * no Murmur restart.
   */
  readonly mcpPortInUse = computed(() => this._mcpStatus().state === "portInUse");

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

  /**
   * Latched by {@link markModelSizeUserPick} when the user changes the quality in the
   * picker, consumed by the next `save()`. Without the latch, `modelSizeSource` would
   * have to be sent on EVERY save — which would relabel an automatically-chosen size
   * as a deliberate one the moment the user changed something unrelated.
   */
  private readonly _modelSizeUserPick = signal(false);

  /** The picker reports a DELIBERATE quality choice; the next save records it. */
  markModelSizeUserPick(): void {
    this._modelSizeUserPick.set(true);
  }

  /**
   * Live signal of the modelSize form control's value — same `toSignal(valueChanges)`
   * shape as `_gatewayModelValue` below, so {@link downloadHint} can track the pick
   * the user just made rather than the one the backend last saved.
   */
  private readonly _modelSizeValue = toSignal(
    this.form.controls.modelSize.valueChanges.pipe(
      startWith(this.form.controls.modelSize.value),
    ),
    { initialValue: "" },
  );

  /**
   * Approx download size for the selected quality (shown on the Download button),
   * read from the RUST CATALOG via {@link MachineService}.
   *
   * This used to be a nine-entry hardcoded `hints` map here, mirrored by a SIX-entry
   * `SIZE_HINTS` in the onboarding wizard and by the `<option>` labels in the
   * template — three copies that had already diverged. The catalog is now the single
   * source; a size the catalog states no figure for renders "size unknown", never a
   * blank that reads as "free" (see `core/model-bytes.ts`).
   */
  readonly downloadHint = computed(() => {
    const size = this._modelSizeValue() || this.machine.selectedId();
    const row = this.machine.models().find((m) => m.id === size);
    // No catalog yet (a cold first read, or a failed probe) ⇒ say nothing rather
    // than invent a figure; the button still works.
    if (!row) return "";
    return modelSizeLabel(row.approxDownloadBytes);
  });

  /**
   * OPTIONAL parakeet live-ASR engine model presence + download state (mirrors the Whisper
   * download UX, but for the ~600 MB parakeet int8 bundle). `null` = not yet checked.
   */
  private readonly _parakeetPresent = signal<boolean | null>(null);
  readonly parakeetPresent = this._parakeetPresent.asReadonly();
  private readonly _downloadingParakeet = signal(false);
  readonly downloadingParakeet = this._downloadingParakeet.asReadonly();
  private readonly _parakeetDownloadError = signal<string | null>(null);
  readonly parakeetDownloadError = this._parakeetDownloadError.asReadonly();
  private readonly _parakeetDownloadFrac = signal(0);
  readonly parakeetDownloadFrac = this._parakeetDownloadFrac.asReadonly();
  readonly parakeetPct = computed(
    () => Math.round(this.parakeetDownloadFrac() * 100) + "%",
  );

  /** Preserved from the loaded config (not a form field) so saving never un-onboards. */
  private loadedOnboarded = true;

  /**
   * Stage E security flags — preserved from the loaded config (not form-edited)
   * so save() round-trips them instead of letting the backend default them off.
   */
  private loadedMcpRequireToken = true;
  private loadedLockRequireBiometric = true;
  private loadedRelockOnScreenshare = true;

  /**
   * M3-CLIENT sharing — preserve-only here (the Settings → Account section owns
   * these). Snapshotted from the loaded config and round-tripped on save() so the
   * shell's "Save settings" button never clears a set sharing server or the
   * share-egress consent (the backend's serde defaults would otherwise reset
   * shareBaseUrl to "" / shareEgressConsented to false).
   */
  private loadedShareBaseUrl = "";
  private loadedShareEgressConsented = false;

  /**
   * First-run sharing-choice latch — preserve-only here (the /welcome gateway is
   * its sole writer via mark_sharing_choice_made). Snapshotted + round-tripped on
   * save() so the shell "Save settings" never clears it (the backend also
   * PRESERVES it in dto_to_config, so this is belt-and-braces).
   */
  private loadedSharingChoiceMade = false;

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

  // ── brain2 connectors — Jira (Phase 2, NEW EGRESS) ─────────────────────

  /** Jira egress consent state — drives the "Allow Jira access" row; round-tripped on save. */
  private readonly _jiraConsented = signal(false);
  readonly jiraConsented = this._jiraConsented.asReadonly();
  /** True while the one-time Jira consent command is in flight. */
  private readonly _jiraConsenting = signal(false);
  readonly jiraConsenting = this._jiraConsenting.asReadonly();
  /** Surfaced if granting Jira consent rejects. */
  private readonly _jiraConsentError = signal<string | null>(null);
  readonly jiraConsentError = this._jiraConsentError.asReadonly();
  /** Whether a Jira API token is stored (has-token check; never the value). */
  private readonly _hasJiraToken = signal(false);
  readonly hasJiraToken = this._hasJiraToken.asReadonly();
  /** True while the BYO token is being saved. */
  private readonly _savingJiraToken = signal(false);
  readonly savingJiraToken = this._savingJiraToken.asReadonly();
  /** Surfaced if storing the Jira token rejects. */
  private readonly _jiraTokenError = signal<string | null>(null);
  readonly jiraTokenError = this._jiraTokenError.asReadonly();

  // ── brain2 connectors — Slack (Phase 3, NEW EGRESS) ────────────────────

  /** Slack egress consent state — drives the "Allow Slack access" row; round-tripped on save. */
  private readonly _slackConsented = signal(false);
  readonly slackConsented = this._slackConsented.asReadonly();
  /** True while the one-time Slack consent command is in flight. */
  private readonly _slackConsenting = signal(false);
  readonly slackConsenting = this._slackConsenting.asReadonly();
  /** Surfaced if granting Slack consent rejects. */
  private readonly _slackConsentError = signal<string | null>(null);
  readonly slackConsentError = this._slackConsentError.asReadonly();
  /** Whether a Slack user token is stored (has-token check; never the value). */
  private readonly _hasSlackToken = signal(false);
  readonly hasSlackToken = this._hasSlackToken.asReadonly();
  /** True while the BYO token is being saved. */
  private readonly _savingSlackToken = signal(false);
  readonly savingSlackToken = this._savingSlackToken.asReadonly();
  /** Surfaced if storing the Slack token rejects. */
  private readonly _slackTokenError = signal<string | null>(null);
  readonly slackTokenError = this._slackTokenError.asReadonly();

  // ── brain2 connectors — Notion (READ connector, NEW EGRESS) ────────────

  /** Notion egress consent state — drives the "Allow Notion access" row; round-tripped on save. */
  private readonly _notionConsented = signal(false);
  readonly notionConsented = this._notionConsented.asReadonly();
  /** True while the one-time Notion consent command is in flight. */
  private readonly _notionConsenting = signal(false);
  readonly notionConsenting = this._notionConsenting.asReadonly();
  /** Surfaced if granting Notion consent rejects. */
  private readonly _notionConsentError = signal<string | null>(null);
  readonly notionConsentError = this._notionConsentError.asReadonly();
  /** Whether a Notion integration token is stored (has-token check; never the value). */
  private readonly _hasNotionToken = signal(false);
  readonly hasNotionToken = this._hasNotionToken.asReadonly();
  /** True while the BYO token is being saved. */
  private readonly _savingNotionToken = signal(false);
  readonly savingNotionToken = this._savingNotionToken.asReadonly();
  /** Surfaced if storing the Notion token rejects. */
  private readonly _notionTokenError = signal<string | null>(null);
  readonly notionTokenError = this._notionTokenError.asReadonly();

  // ── brain2 connectors — ClickUp (READ connector, NEW EGRESS) ───────────

  /** ClickUp egress consent state — drives the "Allow ClickUp access" row; round-tripped on save. */
  private readonly _clickupConsented = signal(false);
  readonly clickupConsented = this._clickupConsented.asReadonly();
  /** True while the one-time ClickUp consent command is in flight. */
  private readonly _clickupConsenting = signal(false);
  readonly clickupConsenting = this._clickupConsenting.asReadonly();
  /** Surfaced if granting ClickUp consent rejects. */
  private readonly _clickupConsentError = signal<string | null>(null);
  readonly clickupConsentError = this._clickupConsentError.asReadonly();
  /** Whether a ClickUp API token is stored (has-token check; never the value). */
  private readonly _hasClickupToken = signal(false);
  readonly hasClickupToken = this._hasClickupToken.asReadonly();
  /** True while the BYO token is being saved. */
  private readonly _savingClickupToken = signal(false);
  readonly savingClickupToken = this._savingClickupToken.asReadonly();
  /** Surfaced if storing the ClickUp token rejects. */
  private readonly _clickupTokenError = signal<string | null>(null);
  readonly clickupTokenError = this._clickupTokenError.asReadonly();

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
   * summarize/mod.rs): claude_code / codex_cli / anthropic / gateway always send content
   * off-device (gateway even on loopback — it can forward to the cloud);
   * ollama is local ONLY when its base URL host is loopback (see
   * `ollamaIsRemote`). Reuse this wherever the FE decides "is this cloud" so
   * the two classifications can't diverge.
   */
  readonly providerIsCloud = computed(() => {
    const id = this._providerIdValue();
    if (id === "ollama") return this.ollamaIsRemote();
    return true; // claude_code | codex_cli | anthropic | gateway | any future id
  });

  /**
   * Which engines the CURRENT posture actively routes work to right now — the
   * source for the "In use now" badge in Advanced → Engines. Derived from the
   * posture-preset semantics (NOT re-implementing the resolver row-by-row):
   * cloud/hybrid clear the role overrides so the DEFAULT engine writes
   * Notes/Ask/Live; hybrid/fully_local run the built-in on-device brain
   * (reactions / all local roles). It is a usage HINT, not a locality label —
   * the card's own group heading and the "What runs where" map carry the
   * cloud-vs-Mac truth. It never marks an engine a preset does not use; on the
   * derived `custom` posture it best-effort marks the default engine (which
   * always writes Notes) plus the built-in brain when `brain_backend=local`.
   */
  readonly inUseConnections = computed<ReadonlySet<string>>(() => {
    const p = this.posture();
    const provider = this._providerIdValue();
    const s = new Set<string>();
    if (provider && (p === "cloud" || p === "hybrid" || p === "custom"))
      s.add(provider);
    if (p === "hybrid" || p === "fully_local") s.add(BRAIN_ENGINE_ID);
    if (p === "custom" && this._brainBackendValue() === "local")
      s.add(BRAIN_ENGINE_ID);
    return s;
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
        case "codex_cli":
          return {
            connection: "Codex",
            destination: "OpenAI (via the Codex CLI)",
          };
        case "anthropic":
          return { connection: "Anthropic API", destination: "api.anthropic.com" };
        case "gateway": {
          const dest = this.gatewayDestination();
          return {
            connection: "Kong AI Gateway",
            destination: dest
              ? dest.host
              : "your gateway (server address not set)",
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
  /**
   * The Default-model field's LIVE value and its engine, so a view can react to TYPING and not only
   * to an engine switch. Exposed because the persistence boundary refuses some ids, and a field
   * that displays a value `save_config` is about to discard is the UI telling the user something
   * untrue.
   */
  readonly providerModelValue = this._providerModelValue;
  readonly providerIdValue = this._providerIdValue;
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
   * claude_code/codex_cli/anthropic, live endpoints for ollama/gateway). A key that is
   * PRESENT with an empty array = "fetch tried, no catalog" → the pickers fall
   * back to a free-text input (the gateway keep-manually-typed pattern).
   */
  private readonly _modelCatalogs = signal<
    Readonly<Record<string, ModelCatalog>>
  >({});
  readonly modelCatalogs = this._modelCatalogs.asReadonly();
  /** Connections with a `list_models` fetch currently in flight. */
  private readonly _modelsLoading = signal<ReadonlySet<string>>(new Set());
  readonly modelsLoading = this._modelsLoading.asReadonly();
  /** Last catalog attempts that failed (distinct from a successful empty catalog). */
  private readonly _modelCatalogFailures = signal<ReadonlySet<string>>(
    new Set(),
  );
  /** One shared promise per connection prevents a selection race from observing a fake failure. */
  private readonly _modelLoadPromises = new Map<string, Promise<boolean>>();
  /** Fetch a connection's catalog once; later calls are no-ops (Refresh re-fetches). */
  async ensureModels(connection: string): Promise<boolean> {
    if (this._modelCatalogs()[connection] !== undefined) {
      return !this._modelCatalogFailures().has(connection);
    }
    // A RECORDED FAILURE also counts as "already attempted".
    //
    // A failed fetch no longer stores an empty catalog (it must not fabricate `"bundled"`
    // provenance), and without this line the absent catalog made every later `ensureModels` re-issue
    // the request. On `ollama` and `gateway` that is a real outbound call to a user-configured host,
    // so an unreachable endpoint turned each engine/role selection into another attempt. The
    // explicit Refresh button calls `refreshModels` directly and is unaffected — retrying stays a
    // deliberate act rather than a side effect of touching a dropdown.
    if (this._modelCatalogFailures().has(connection)) return false;
    return this.refreshModels(connection);
  }

  /**
   * (Re-)fetch one connection's model catalog. A rejection (endpoint down, or
   * a backend without `list_models` yet) records an EMPTY catalog so the UI
   * shows the free-text fallback instead of a dead select — same contract as
   * `refreshGatewayModels`. Button-driven or load-driven, never an effect.
   */
  refreshModels(connection: string): Promise<boolean> {
    if (!PROVIDER_CONNECTION_IDS.includes(connection)) {
      return Promise.resolve(false);
    }
    const active = this._modelLoadPromises.get(connection);
    if (active) return active;
    const load = this.loadModels(connection);
    this._modelLoadPromises.set(connection, load);
    void load.then(
      () => this._modelLoadPromises.delete(connection),
      () => this._modelLoadPromises.delete(connection),
    );
    return load;
  }

  private async loadModels(connection: string): Promise<boolean> {
    this._modelsLoading.set(new Set(this._modelsLoading()).add(connection));
    let models: ModelCatalog | null = null;
    let loaded = true;
    try {
      models = await this.ipc.listModels(connection);
    } catch {
      // Fall through — free-text fallback.
      loaded = false;
    }
    // NEVER FABRICATE PROVENANCE. A failed fetch used to be stored as an empty `"bundled"`
    // catalog, so a transport error on `ollama`/`gateway` made a LIVE connection look compiled-in:
    // Refresh disappeared at the one moment retrying is the whole point, and the UI claimed the
    // list ships with the app. A failure leaves the previous catalog in place if there is one,
    // and otherwise records nothing at all — `_modelCatalogFailures` already carries the error
    // state, and an absent catalog is honestly "unknown" rather than dishonestly "bundled".
    if (models) {
      this._modelCatalogs.set({ ...this._modelCatalogs(), [connection]: models });
    }
    const failures = new Set(this._modelCatalogFailures());
    if (loaded) failures.delete(connection);
    else failures.add(connection);
    this._modelCatalogFailures.set(failures);
    const next = new Set(this._modelsLoading());
    next.delete(connection);
    this._modelsLoading.set(next);
    return loaded;
  }

  /** The Default-model picker's catalog (CLI/direct Default AI connections only). */
  readonly defaultModelCatalog = computed(
    () => this.modelCatalogs()[this._providerIdValue()]?.options ?? [],
  );
  /**
   * Whether the Default-model catalog was FETCHED rather than compiled in. Read from the catalog,
   * not from its options: an empty live catalog (a gateway answering with zero models) is exactly
   * when Refresh matters, and it has no option to carry a source.
   */
  /**
   * Whether to OFFER Refresh. Hidden only when the catalog is known to be `"bundled"` — compiled
   * into the binary, where the button could not change anything.
   *
   * Deliberately `!== "bundled"` rather than `=== "live"`, because a MISSING catalog is the state
   * after a failed fetch: `loadModels` no longer writes a fabricated bundled entry on error (that
   * made a live connection look compiled-in), so `=== "live"` would have hidden Refresh at the one
   * moment retrying is the entire point. Unknown provenance offers the button; only proven-bundled
   * withholds it.
   */
  /** Whether this connection fetches its catalog from a real endpoint. See the constant. */
  connectionHasLiveCatalog(connection: string): boolean {
    return LIVE_CATALOG_CONNECTION_IDS.includes(connection);
  }

  readonly defaultCatalogIsLive = computed(() =>
    this.connectionHasLiveCatalog(this._providerIdValue()),
  );
  /** True while the Default-model picker's catalog fetch is in flight. */
  readonly defaultModelsLoading = computed(() =>
    this.modelsLoading().has(this._providerIdValue()),
  );
  /** Keep a manually-typed default model selectable when absent from the catalog. */
  readonly defaultModelIsCustom = computed(() => {
    const current = this._providerModelValue();
    if (!current) return false;
    return !this.defaultModelCatalog().some((o) => o.id === current);
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
    () => `Follows the Default engine: ${this.defaultAiSummary()}`,
  );

  /**
   * The resolved Notes model label for the Note-assistant clarifying line —
   * in-note Brain actions ride `Role::Notes`, so this is the model that answers
   * them. Sourced from the backend-resolved "what runs where" map (the `notes`
   * row's `model`, the authoritative truth); null until the map loads so the
   * copy degrades to the plain sentence rather than showing a stale guess.
   */
  readonly noteAssistModelLabel = computed<string | null>(() => {
    const row = this.aiMap().find((r) => r.job === "notes");
    return row?.model?.trim() ? row.model : null;
  });

  /**
   * Ask/Live-row Inherit summary — an honest mirror of the backend resolver:
   * with the role key empty, Ask/Live fall back to the legacy `brainBackend`
   * mapping, NOT unconditionally to the Default AI. Showing "Follows Default
   * AI" to a legacy `brain_backend=local` install would be a lie.
   */
  readonly assistantInheritSummary = computed(() => {
    switch (this._brainBackendValue()) {
      case "local":
        return "Follows the assistant fallback: Murmur Brain — on-device";
      case "off":
        return "Follows the assistant fallback: Off — retrieval only";
      default:
        return `Follows the Default engine: ${this.defaultAiSummary()}`;
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
  /**
   * The model a role keeps after its connection changes.
   *
   * This used to be an unconditional `""`, on the reasoning that a model belongs to the arm it was
   * picked for. That is the same "the catalog is authoritative" instinct `repairForeignRoleModels`
   * was deleted for: it destroys a choice the new arm could have honoured, and the user has no way
   * to get it back. The Setup card already settled the rule — KEEP an id the new engine can send
   * and explain that it is unlisted; clear ONLY one it cannot send at all — and a role row is the
   * same question asked one row lower.
   *
   * Two different questions, and conflating them produced a bug in each direction.
   *
   * 1. INHERIT (`""`) keeps the model, always. `roles::is_explicit` keys on the connection key
   *    alone, so an inheriting role resolves through `legacy_default_target`, which never reads
   *    `role_*_model`. The value is genuinely inert, and clearing it would be a silent deletion on
   *    a row that renders no model control and therefore cannot explain itself.
   *
   * 2. Every other connection CONSUMES the model, `local` emphatically included — and an earlier
   *    version of this method claimed the opposite. `make_provider_resolved`'s local arm reads
   *    `if target.model.trim().is_empty() { config.brain_model_id } else { Some(target.model) }`
   *    and then `resolve_brain_model(...)?`, so a retained `llama3.1:8b` OVERRIDES the on-device
   *    model and turns every local note into `AppError::Unavailable`. Fail-closed, never a silent
   *    cloud fallback — but broken, on the surface a privacy-conscious user reaches for.
   *
   * So a consuming connection clears an id it cannot use, and the row SAYS SO. The notice for that
   * lives outside the provider-model block precisely because `local`/`off` render no model control:
   * a caption that only appears where a model field exists cannot describe the clears that happen
   * where one does not.
   */
  private roleModelAfterConnectionChange(previous: string, connection: string): string {
    const target = connection.trim();
    // Nothing reads the model here, so there is nothing to protect against and nothing to explain.
    if (!connectionConsumesRoleModel(target)) return previous;
    // `local` DOES read it — as a registry KEY. `resolve_brain_model` looks the id up in
    // `BRAIN_MODELS`, so a matching id is a working per-role on-device override and clearing it
    // destroys a legitimate choice; a non-matching one overrides `brain_model_id` and fails the
    // note with `Unavailable`, so it must go. Clearing BOTH — the previous version of this rule —
    // was over-broad in exactly the direction this whole change exists to stop.
    if (target === "local") {
      return this.brainModels().some((m) => m.id === previous) ? previous : "";
    }
    if (!PROVIDER_CONNECTION_IDS.includes(target)) return "";
    return connectionKeepsModelId(previous, effectiveConnection(target, this._providerIdValue() ?? ""))
      ? previous
      : "";
  }

  setRoleConnection(role: "notes" | "ask" | "live", connection: string): void {
    switch (role) {
      case "notes":
        this.form.patchValue({
          roleNotesConnection: connection,
          roleNotesModel: this.roleModelAfterConnectionChange(
            this.roleNotesModelValue(),
            connection,
          ),
          roleNotesEffort: "",
        });
        break;
      case "ask":
        this.form.patchValue({
          roleAskConnection: connection,
          roleAskModel: this.roleModelAfterConnectionChange(
            this.roleAskModelValue(),
            connection,
          ),
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
          roleLiveModel: this.roleModelAfterConnectionChange(
            this.roleLiveModelValue(),
            connection,
          ),
          roleLiveEffort: "",
        });
        break;
    }
    // User-driven form write → dirty, so the auto-save picks it up.
    this.form.markAsDirty();
    if (PROVIDER_CONNECTION_IDS.includes(connection)) {
      void this.ensureModels(connection);
    }
    // Keep the posture segment honest against backend truth (it derives from the
    // STORED config — an unsaved role edit doesn't change dispatch yet, so this
    // holds the last real posture rather than an optimistic guess, and it flips
    // to Custom once save() persists a role that breaks the preset).
    void this.refreshPosture();
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

  // ── Murmur Brain — posture (Cloud / Hybrid / Fully local) ──────────────

  /** The DERIVED posture (`cloud`/`hybrid`/`fully_local`/`custom`), or null before load. */
  private readonly _posture = signal<Posture | null>(null);
  readonly posture = this._posture.asReadonly();
  /** True while a `set_brain_posture` preset is being applied. */
  private readonly _postureBusy = signal(false);
  readonly postureBusy = this._postureBusy.asReadonly();
  /** Surfaced if reading/applying the posture rejects. */
  private readonly _postureError = signal<string | null>(null);
  readonly postureError = this._postureError.asReadonly();

  // ── "What runs where" resolved map ──────────────────────────────────────
  /** The backend-resolved per-job routing rows (resolved_ai_map). */
  private readonly _aiMap = signal<AiMapRow[]>([]);
  readonly aiMap = this._aiMap.asReadonly();

  /** Re-fetch the resolved map (load, after save, after a posture apply). Keeps last on failure. */
  async refreshAiMap(): Promise<void> {
    try {
      this._aiMap.set(await this.ipc.resolvedAiMap());
    } catch {
      // keep the last known map — the card renders its loading/emptiness state
    }
  }

  /**
   * Advanced-disclosure open state, HOISTED from AiAdvancedBlockComponent so the
   * map card's "Change" affordance can open it from outside.
   */
  readonly advancedExpanded = signal(false);
  expandAdvanced(): void {
    this.advancedExpanded.set(true);
  }

  /**
   * The per-feature role row the map's "Change" wants the Advanced block to
   * scroll to + flash. Written by `requestHighlightRole`, consumed by
   * `AiRoleRowsComponent` (which opens the disclosure, scrolls the row into
   * view, and flashes it), then cleared via `clearHighlightRole` once handled.
   */
  private readonly _highlightRole = signal<"notes" | "ask" | "live" | null>(
    null,
  );
  readonly highlightRole = this._highlightRole.asReadonly();

  /**
   * Ask the role rows to scroll to + flash `role`. The null-then-set makes a
   * REPEAT click on the same row's "Change" re-fire the highlight even when the
   * value is unchanged (a plain `.set(role)` would be a no-op for the effect).
   */
  requestHighlightRole(role: "notes" | "ask" | "live"): void {
    this._highlightRole.set(null);
    this._highlightRole.set(role);
  }

  /** Clear the highlight request once the role rows have handled it. */
  clearHighlightRole(): void {
    this._highlightRole.set(null);
  }

  /**
   * The posture mid-download (drives the target-card progress indicator).
   * Non-null only while `setPosture` is downloading absent needed models.
   */
  private readonly _pendingPosture = signal<Posture | null>(null);
  readonly pendingPosture = this._pendingPosture.asReadonly();

  /**
   * A posture the user PICKED that needs an on-device download, awaiting explicit
   * confirmation. Non-null shows the confirm card (what model, size, what it does
   * + a Download button) — nothing downloads until `confirmPostureDownload()`. This
   * is the deliberate opt-in step: a multi-GB download never starts on a single tap.
   */
  private readonly _pendingConfirm = signal<Posture | null>(null);
  readonly pendingConfirm = this._pendingConfirm.asReadonly();

  /** The models the pending-confirm posture would download (name/size for the card). */
  readonly confirmModels = computed(() => {
    const p = this._pendingConfirm();
    return p ? this.neededModelsFor(p) : [];
  });

  /** Total bytes the pending-confirm download would fetch (absent models only). */
  readonly confirmDownloadBytes = computed(() =>
    this.confirmModels()
      .map((n) => n.model)
      .filter((m): m is BrainModelDto => !!m && !m.downloaded)
      .reduce((sum, m) => sum + m.approxSizeBytes, 0),
  );

  /** Flip to true via `cancelPostureDownload()` to abort an in-flight download loop. */
  private _cancelDownload = false;

  /** True while the one-tap "Enable Murmur Brain Live" flow (hybrid + light model) runs. */
  private readonly _enablingBrainLive = signal(false);
  readonly enablingBrainLive = this._enablingBrainLive.asReadonly();

  /**
   * Whether this Mac has enough RAM to run Realtime Reactions alongside a live
   * recording (`brain_live_ram_ok`, the combined-residency guard). Drives a
   * NON-BLOCKING warning on the Brain Live enablement card — enablement is never
   * hard-blocked. Defaults TRUE (never warn behind an unread probe); set
   * best-effort in load().
   */
  private readonly _brainLiveRamOk = signal(true);
  readonly brainLiveRamOk = this._brainLiveRamOk.asReadonly();

  /**
   * What the ACTIVE posture needs on-device. Cloud → none; Hybrid → reactions
   * (light); Fully local → notes (heavy) + reactions (light). Re-derives whenever
   * `posture()` or `brainModels()` changes (the picker reads both).
   */
  readonly neededModels = computed(() =>
    this.neededModelsFor(this.posture()),
  );

  // (The old `postureStateLine` summary was replaced by the per-posture
  // `postureMeaning()` line in brain-posture-block — more accurate about which
  // jobs stay on-device, and not hardcoded to "Claude Code" as the writer.)

  /**
   * The installed-base retirement nudge (`brain_model_retirement_nudge`), or null.
   * Non-null → the persisted model is a retired non-commercial id and the FE
   * offers the Apache-licensed replacement.
   */
  private readonly _retirementNudge = signal<RetiredModelNudge | null>(null);
  readonly retirementNudge = this._retirementNudge.asReadonly();
  /** True while the retirement replacement is being downloaded + selected. */
  private readonly _applyingRetirement = signal(false);
  readonly applyingRetirement = this._applyingRetirement.asReadonly();

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
    this.form.markAsDirty(); // user-driven → auto-save
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
      // M3-CLIENT sharing — snapshot the sharing server + share-egress consent so
      // save() round-trips them unchanged (owned by Settings → Account).
      this.loadedShareBaseUrl = cfg.shareBaseUrl ?? "";
      this.loadedShareEgressConsented = cfg.shareEgressConsented ?? false;
      // First-run sharing latch — snapshot so save() round-trips it unchanged.
      this.loadedSharingChoiceMade = cfg.sharingChoiceMade ?? false;
      this._cloudConsented.set(cfg.cloudEgressConsented ?? false);
      // brain2 connectors — web-search consent is preserve-only (granted only via
      // consent_to_web_search); snapshot it so save() round-trips it unchanged.
      this._webConsented.set(cfg.webSearchConsented ?? false);
      // brain2 connectors (Phase 2) — Jira consent is preserve-only (granted only via
      // consent_to_jira); snapshot it so save() round-trips it unchanged.
      this._jiraConsented.set(cfg.jiraConsented ?? false);
      // brain2 connectors (Phase 3) — Slack consent is preserve-only (granted only via
      // consent_to_slack); snapshot it so save() round-trips it unchanged.
      this._slackConsented.set(cfg.slackConsented ?? false);
      // brain2 connectors — Notion / ClickUp consent is preserve-only (granted only via
      // consent_to_notion / consent_to_clickup); snapshot so save() round-trips it unchanged.
      this._notionConsented.set(cfg.notionConsented ?? false);
      this._clickupConsented.set(cfg.clickupConsented ?? false);
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
        // Mirrors backend default capture_system_audio = true (settings/config.rs, AppConfig::default; #167).
        captureSystemAudio: cfg.captureSystemAudio ?? true,
        vadEnabled: cfg.vadEnabled ?? true,
        keepHiresMasters: cfg.keepHiresMasters ?? false,
        // Mirrors backend default diarize_others = true; explicit false is preserved by ??.
        diarizeOthers: cfg.diarizeOthers ?? true,
        voiceprintEnabled: cfg.voiceprintEnabled ?? false,
        aecEnabled: cfg.aecEnabled ?? false,
        // Mirrors backend default post_aec_enabled = false (settings/config.rs, AppConfig::default).
        postAecEnabled: cfg.postAecEnabled ?? false,
        audioStorageLimitGb:
          cfg.audioStorageLimitGb != null ? String(cfg.audioStorageLimitGb) : "",
        audioAutoPrune: cfg.audioAutoPrune ?? false,
        // The backend default is machine-conditional (small, or large-v3-turbo-q8_0 when already
        // downloaded / on a fresh 12+ GB Mac — transcribe/model.rs default_model_size); "small"
        // here is only the nullish safety net.
        modelSize: cfg.modelSize ?? "small",
        liveAsrEngine: cfg.liveAsrEngine ?? "whisper",
        brainIdleTimeoutSecs: cfg.brainIdleTimeoutSecs ?? 300,
        brainReadyTimeoutSecs: cfg.brainReadyTimeoutSecs ?? 90,
        brainHardCapSecs: cfg.brainHardCapSecs ?? 180,
        voiceTrigger: cfg.voiceTrigger ?? false,
        noteStyle: cfg.noteStyle ?? "standard",
        notesMode: cfg.notesMode ?? "enhance",
        autoOrganize: cfg.autoOrganize ?? false,
        noteLanguage: cfg.noteLanguage ?? "auto",
        // Mirrors backend default ground_summary = true; explicit false remains OFF.
        groundSummary: cfg.groundSummary ?? true,
        glossary: cfg.glossary ?? "",
        brainBackend: cfg.brainBackend ?? "cloud",
        realtimeReactions: cfg.realtimeReactions ?? false,
        proactiveHintsEnabled: cfg.proactiveHintsEnabled ?? true,
        userMemoryEnabled: cfg.userMemoryEnabled ?? true,
        updateCheckEnabled: cfg.updateCheckEnabled ?? true,
        brainModelId: cfg.brainModelId ?? "",
        brainModelPath: cfg.brainModelPath ?? "",
        // Mirrors backend default semantic_search_enabled = true (settings/config.rs; #159/#160).
        semanticSearchEnabled: cfg.semanticSearchEnabled ?? true,
        webSearchEnabled: cfg.webSearchEnabled ?? false,
        // brain2 connectors (Phase 2) — Jira toggle + non-secret base URL/email.
        jiraEnabled: cfg.jiraEnabled ?? false,
        jiraBaseUrl: cfg.jiraBaseUrl ?? "",
        jiraEmail: cfg.jiraEmail ?? "",
        // brain2 connectors (Phase 3) — Slack toggle.
        slackEnabled: cfg.slackEnabled ?? false,
        // brain2 connectors — Notion / ClickUp toggles + ClickUp's non-secret workspace id.
        notionEnabled: cfg.notionEnabled ?? false,
        clickupEnabled: cfg.clickupEnabled ?? false,
        clickupTeamId: cfg.clickupTeamId ?? "",
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
        // Notes feature — note-assistant action toggles, default TRUE (an absent
        // value from a backend that hasn't shipped these yet reads as ON).
        noteAssistRefine: cfg.noteAssistRefine ?? true,
        noteAssistShorten: cfg.noteAssistShorten ?? true,
        noteAssistEnhance: cfg.noteAssistEnhance ?? true,
        // The 16 NEW actions: enabled ⇔ NOT in the off-list (absent list ⇒ all ON).
        ...this.noteAssistOffToToggles(cfg.noteAssistActionsOff ?? []),
      });
      this._loadedBrainBackend = (cfg.brainBackend ?? "cloud") as BrainBackend;
      // Prefetch the model catalogs the loaded config already renders selects
      // for: the Default-model picker (claude_code/codex_cli/anthropic — ollama/
      // gateway keep their model on the connection card, so prefetching their
      // REMOTE catalogs here would be network egress with zero UI payoff;
      // lock-security stage-4 boundary condition) and any concrete per-role
      // connection (those DO render a select). Best-effort: a failed fetch
      // leaves an empty catalog and the pickers fall back to free-text inputs.
      for (const conn of new Set(
        [
          cfg.providerId === "claude_code" ||
          cfg.providerId === "codex_cli" ||
          cfg.providerId === "anthropic"
            ? cfg.providerId
            : "",
          cfg.roleNotesConnection ?? "",
          cfg.roleAskConnection ?? "",
          cfg.roleLiveConnection ?? "",
        ].filter((c) => PROVIDER_CONNECTION_IDS.includes(c)),
      )) {
        void this.ensureModels(conn);
      }
      // The download hint is a `computed` over the Rust catalog now, so nothing to
      // push here — but the catalog itself has to be READ, and this is the load path.
      void this.machine.refresh();
      this._inputDevices.set(await this.ipc.listInputDevices().catch(() => []));
      // Speaker voiceprints — the gated management list (best-effort; failure leaves it empty).
      this._voiceprints.set(await this.ipc.listVoiceprints().catch(() => []));
      this._hasKey.set(await this.ipc.hasAnthropicKey());
      this._hasWebKey.set(await this.ipc.hasWebSearchKey().catch(() => false));
      this._hasJiraToken.set(await this.ipc.hasJiraToken().catch(() => false));
      this._hasSlackToken.set(await this.ipc.hasSlackToken().catch(() => false));
      this._hasNotionToken.set(await this.ipc.hasNotionToken().catch(() => false));
      this._hasClickupToken.set(
        await this.ipc.hasClickupToken().catch(() => false),
      );
      this._hasGatewayKey.set(await this.ipc.hasGatewayKey().catch(() => false));
      this._modelPresent.set(await this.ipc.modelPresent());
      // OPTIONAL parakeet live-ASR engine presence (best-effort — absent is the common case).
      this._parakeetPresent.set(
        await this.ipc.parakeetModelsPresent().catch(() => false),
      );
      // Whisper transcribe-model download-progress stream (best-effort).
      await this.subscribeModelDownload();
      await this.refreshProviders();
      // Phase H — brain model registry + download-progress stream (best-effort).
      await this.subscribeBrainDownload();
      await this.refreshBrainModels();
      // Murmur Brain — derived posture (Cloud/Hybrid/Fully local) + retirement
      // nudge. refreshPosture() also refreshes the resolved "what runs where"
      // AI map, so no separate refreshAiMap() call is needed here.
      await this.refreshPosture();
      // Brain Live RAM headroom (best-effort; true = never warn behind a failed probe).
      this._brainLiveRamOk.set(
        await this.ipc.brainLiveRamOk().catch(() => true),
      );
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
      // Recording-storage usage report (best-effort; drives the Storage section + Library bar).
      await this.loadStorageReport();
      // User-authored note templates (best-effort; feeds the note-style selector + editor).
      await this.loadNoteTemplates();
      // MCP config block for Claude Code — carries the private bearer token so the copied
      // snippet authenticates (fixes the `-32001 unauthorized` handshake). Best-effort: on a
      // keychain read failure it stays empty and the copy button no-ops rather than pasting a
      // tokenless config that would fail.
      await this.refreshMcpConfig();
      // Only a SUCCESSFUL load arms auto-save — arming after a failed load
      // would let the pristine defaults overwrite the user's stored config.
      this.autoSaveReady = true;
    } catch (e) {
      this._loadError.set(this.errorCopy.humanize(e));
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
        // EVENT_MODEL_DOWNLOAD is shared by the whisper AND parakeet downloads; route the
        // progress to whichever this component started (only one runs at a time).
        if (this.downloadingModel()) {
          if (p.total && p.total > 0) {
            this._modelDownloadFrac.set(Math.min(1, p.downloaded / p.total));
          }
          if (p.done) this._modelDownloadFrac.set(1);
        } else if (this.downloadingParakeet()) {
          if (p.total && p.total > 0) {
            this._parakeetDownloadFrac.set(Math.min(1, p.downloaded / p.total));
          }
          if (p.done) this._parakeetDownloadFrac.set(1);
        }
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
      this._brainError.set(this.errorCopy.humanize(e));
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
      this.form.markAsDirty(); // user-driven → auto-save
      await this.refreshBrainModels();
    } catch (e) {
      this._brainError.set(this.errorCopy.humanize(e));
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
      this._brainError.set(this.errorCopy.humanize(e));
    } finally {
      this._brainDownloadingId.set(null);
    }
  }

  // ── Murmur Brain — posture + Brain Live enablement + retirement nudge ───

  /**
   * Re-sync the reactive form's posture-OWNED controls (the nine role keys +
   * `brainBackend`) from FRESH backend config after a `set_brain_posture` /
   * `enableBrainLive` write.
   *
   * WHY (a silent zero-egress regression otherwise): a posture preset writes the
   * `role_*` + `brain_backend` DB keys directly (e.g. Fully-Local sets them all
   * to "local"), but the reactive form still holds the STALE values from the
   * one-time load() ("" = inherit-cloud). The very next ordinary save() serializes
   * `form.getRawValue()`, so it would send `roleNotesConnection:""` etc. and the
   * backend `dto_to_config` takes them verbatim — CLOBBERING the "local" keys the
   * posture just wrote and flipping the posture back to cloud egress. Re-patching
   * the form to backend truth here ends that clobber while keeping the per-feature
   * role editor fully editable (it legitimately writes these same keys via the
   * form, so they can't be preserve-only backend-side). Mirrors load()'s
   * config→form role mapping exactly.
   */
  private async syncPostureFormFromBackend(): Promise<void> {
    const cfg = await this.ipc.getConfig().catch(() => null);
    if (!cfg) return;
    this.form.patchValue({
      brainBackend: cfg.brainBackend ?? "cloud",
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
    // Keep the Ask-row "Inherit" restore baseline honest: a posture may have
    // changed brain_backend, so the value load() snapshotted is now stale.
    this._loadedBrainBackend = (cfg.brainBackend ?? "cloud") as BrainBackend;
  }

  /** Re-read the DERIVED posture + the retirement nudge (best-effort; failure is non-fatal). */
  async refreshPosture(): Promise<void> {
    try {
      this._posture.set(await this.ipc.brainPosture());
    } catch (e) {
      this._postureError.set(this.errorCopy.humanize(e));
    }
    this._retirementNudge.set(
      await this.ipc.brainModelRetirementNudge().catch(() => null),
    );
    // Posture/roles just changed → the resolved "what runs where" map may have
    // shifted. Refresh it here so every posture caller (setPosture, role/provider
    // saves, retirement apply, load) keeps the map in sync.
    void this.refreshAiMap();
  }

  /**
   * Apply a posture PRESET (`cloud` / `hybrid` / `fully_local`).
   *
   * Auto-download flow: if any model needed by the target posture is absent
   * (not yet downloaded), download each one first, select each newly-downloaded
   * model, THEN commit the posture. If nothing is absent, commits immediately
   * (today's fast path — no new IPC round-trips). On failure or cancel, clears
   * `pendingPosture`, surfaces `postureError` (cancel is silent), and refreshes
   * the displayed posture back to the unchanged backend value. Never commits an
   * incomplete state.
   *
   * NOTE: re-patches the reactive form from the backend after every commit to
   * prevent the posture-form-clobber regression (see `syncPostureFormFromBackend`).
   */
  async setPosture(p: Posture): Promise<void> {
    if (p === "custom") return; // derived-only label — never settable
    this._postureError.set(null);
    this._pendingConfirm.set(null);

    const needed = this.neededModelsFor(p);
    const absent = needed
      .map((n) => n.model)
      .filter((m): m is BrainModelDto => !!m && !m.downloaded);

    if (absent.length === 0) {
      // Fast path: all needed models already on disk (Cloud, or a local posture whose
      // models are present) — select them all, then commit immediately. Selecting
      // unconditionally pins the backend roles at the auto-picked models, not the
      // registry default (which may differ and be absent).
      this._postureBusy.set(true);
      try {
        await this.selectNeeded(needed);
        await this.commitPosture(p);
      } catch (e) {
        this._postureError.set(this.errorCopy.humanize(e));
      } finally {
        this._postureBusy.set(false);
      }
      return;
    }

    // Needs a download → ASK FIRST. Show the confirm card (model, size, what it does
    // + a Download button); nothing downloads until confirmPostureDownload(). A
    // multi-GB fetch must never start on a single tap.
    this._pendingConfirm.set(p);
  }

  /** Dismiss the pending-confirm card without downloading. The posture stays unchanged. */
  cancelPendingPosture(): void {
    this._pendingConfirm.set(null);
  }

  /**
   * Confirm the pending posture: download its absent on-device models (progress via
   * `pendingPosture`/`brainDownloadFrac`), then select all needed models and commit.
   * On failure/cancel, clears state, surfaces `postureError` (cancel is silent), and
   * refreshes the display back to the unchanged backend posture. Never commits an
   * incomplete state.
   */
  async confirmPostureDownload(): Promise<void> {
    const p = this._pendingConfirm();
    if (!p) return;
    this._pendingConfirm.set(null);
    this._postureError.set(null);
    this._postureBusy.set(true);

    const needed = this.neededModelsFor(p);
    const absent = needed
      .map((n) => n.model)
      .filter((m): m is BrainModelDto => !!m && !m.downloaded);

    this._pendingPosture.set(p);
    this._cancelDownload = false;
    try {
      for (const m of absent) {
        if (this._cancelDownload) throw new Error("cancelled");
        this._brainDownloadFrac.set(0);
        this._brainDownloadingId.set(m.id);
        try {
          await this.ipc.downloadBrainModel(m.id);
        } finally {
          this._brainDownloadingId.set(null);
        }
      }
      // Honor cancel between the last download and the select+commit, so a cancel
      // during the only/final download does not still commit the posture.
      if (this._cancelDownload) throw new Error("cancelled");
      // Select ALL needed models (not just the newly-downloaded subset) so a model
      // already on disk but unselected also gets pinned to the right role first.
      await this.selectNeeded(needed);
      await this.commitPosture(p);
    } catch (e) {
      // A cancel is a user action — silent; any real failure surfaces the message.
      this._postureError.set(
        this._cancelDownload ? null : this.errorCopy.humanize(e),
      );
      // Reflect the unchanged backend posture so the card stays honest.
      await this.refreshPosture();
    } finally {
      this._pendingPosture.set(null);
      this._postureBusy.set(false);
      this._cancelDownload = false;
      // Refresh downloaded / selected flags after every attempt (success or fail).
      await this.refreshBrainModels();
    }
  }

  /** Abort an in-flight download loop. The posture stays unchanged. */
  cancelPostureDownload(): void {
    this._cancelDownload = true;
  }

  /**
   * Select every model in `needed` (in `neededModelsFor` order: heavy first,
   * then light) so the backend's role pins point at the auto-picked models before
   * `set_brain_posture` commits. Called on BOTH the fast path (all on disk) and
   * the slow path (after downloads complete) — the contract is identical.
   */
  private async selectNeeded(
    needed: { role: "notes" | "reactions"; model: BrainModelDto | null }[],
  ): Promise<void> {
    for (const n of needed) {
      if (n.model) await this.ipc.selectBrainModel(n.model.id);
    }
  }

  /**
   * Commit a posture to the backend: write it, re-patch the reactive form (to
   * prevent the form-clobber regression), and refresh the derived posture display.
   * Also re-checks RAM fitness after a posture change.
   */
  private async commitPosture(p: Posture): Promise<void> {
    await this.ipc.setBrainPosture(p);
    // Re-patch the form to the role/brainBackend keys the preset just wrote, or
    // the next save() would clobber them back (silent zero-egress regression).
    await this.syncPostureFormFromBackend();
    await this.refreshPosture();
    this._brainLiveRamOk.set(await this.ipc.brainLiveRamOk().catch(() => true));
  }

  /**
   * Smallest model of `cls` that fits this Mac's RAM (family-agnostic).
   * Prefers an already-downloaded model so we avoid needless downloads.
   * Returns `null` when the registry has no fitting model of that class.
   */
  autoPickForClass(cls: ModelClass): BrainModelDto | null {
    const candidates = this.brainModels().filter(
      (m) => m.class === cls && m.fitsRam,
    );
    if (candidates.length === 0) return null;
    const downloaded = candidates.filter((m) => m.downloaded);
    const pool = downloaded.length ? downloaded : candidates;
    return pool.reduce((a, b) => (b.approxSizeBytes < a.approxSizeBytes ? b : a));
  }

  /**
   * What a given posture `p` needs on-device — the same logic as `neededModels`
   * but evaluated for an ARBITRARY posture (used by `setPosture` to determine
   * which models to download before committing).
   */
  private neededModelsFor(
    p: Posture | null,
  ): { role: "notes" | "reactions"; model: BrainModelDto | null }[] {
    const light = this.autoPickForClass("light");
    const heavy = this.autoPickForClass("heavy");
    if (p === "hybrid") return [{ role: "reactions", model: light }];
    if (p === "fully_local")
      return [
        { role: "notes", model: heavy },
        { role: "reactions", model: light },
      ];
    return [];
  }

  /**
   * One-tap "Enable Murmur Brain Live": switch to the Hybrid preset AND ensure the
   * smallest LIGHT on-device model is downloaded + selected (the ~1.1 GB engine
   * that runs realtime reactions + local fact extraction). Progress rides the
   * existing brain-download signals. Best-effort + honest on failure.
   */
  async enableBrainLive(): Promise<void> {
    this._postureError.set(null);
    this._brainError.set(null);
    this._enablingBrainLive.set(true);
    try {
      await this.ipc.setBrainPosture("hybrid");
      // Pick the smallest LIGHT-class model (the realtime engine).
      const models = await this.ipc.listBrainModels();
      const light = models
        .filter((m) => m.class === "light")
        .sort((a, b) => a.approxSizeBytes - b.approxSizeBytes)[0];
      if (light) {
        if (!light.downloaded) {
          // Reuse the shared download-progress UI (brainDownloadingId + frac).
          this._brainDownloadFrac.set(0);
          this._brainDownloadingId.set(light.id);
          try {
            await this.ipc.downloadBrainModel(light.id);
          } finally {
            this._brainDownloadingId.set(null);
          }
        }
        await this.ipc.selectBrainModel(light.id);
      }
      await this.refreshBrainModels();
      // The setBrainPosture("hybrid") above wrote the role/brainBackend keys —
      // re-patch the form so the next save() doesn't clobber them back to cloud.
      await this.syncPostureFormFromBackend();
      await this.refreshPosture();
    } catch (e) {
      this._postureError.set(this.errorCopy.humanize(e));
    } finally {
      this._enablingBrainLive.set(false);
    }
  }

  /**
   * The size (bytes) of the smallest LIGHT model — for the Brain Live card's
   * "~N GB one-time download" copy. Null before the model list has loaded.
   */
  readonly brainLiveModelBytes = computed<number | null>(() => {
    const light = this.brainModels()
      .filter((m) => m.class === "light")
      .sort((a, b) => a.approxSizeBytes - b.approxSizeBytes)[0];
    return light ? light.approxSizeBytes : null;
  });

  /**
   * Whether the light model the backend would ACTUALLY run for Brain Live is
   * already on this Mac (skips the download step). Mirrors the backend's
   * `class_model_id(Light)` (reason.rs): the light engine resolves to the
   * SELECTED light if the user picked one, else the registry DEFAULT light (the
   * first light in display order — `qwen3-1.7b`). Readiness = THAT specific model
   * is downloaded — NOT "any light is downloaded", which would be a false
   * positive when a different, unselected light happens to be on disk while
   * `light()` still resolves to the un-downloaded default → the stub.
   */
  readonly brainLiveModelReady = computed<boolean>(() => {
    const lights = this.brainModels().filter((m) => m.class === "light");
    if (lights.length === 0) return false;
    const effective = lights.find((m) => m.selected) ?? lights[0];
    return effective.downloaded;
  });

  /**
   * Apply the retirement nudge: download + select the Apache-licensed replacement
   * for a retired non-commercial model, then clear the nudge. Progress rides the
   * shared brain-download signals.
   */
  async applyRetirementReplacement(): Promise<void> {
    const nudge = this._retirementNudge();
    if (!nudge) return;
    this._brainError.set(null);
    this._applyingRetirement.set(true);
    try {
      this._brainDownloadFrac.set(0);
      this._brainDownloadingId.set(nudge.replacementId);
      try {
        await this.ipc.downloadBrainModel(nudge.replacementId);
      } finally {
        this._brainDownloadingId.set(null);
      }
      await this.ipc.selectBrainModel(nudge.replacementId);
      this.form.patchValue({
        brainModelId: nudge.replacementId,
        brainModelPath: "",
      });
      this.form.markAsDirty(); // user-driven → auto-save
      await this.refreshBrainModels();
      await this.refreshPosture();
    } catch (e) {
      this._brainError.set(this.errorCopy.humanize(e));
    } finally {
      this._applyingRetirement.set(false);
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
      this._embedDownloadError.set(this.errorCopy.humanize(e));
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
      this._nerDownloadError.set(this.errorCopy.humanize(e));
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
      this._reindexError.set(this.errorCopy.humanize(e));
    } finally {
      this._reindexing.set(false);
    }
  }

  async pickVault(): Promise<void> {
    const dir = await open({ directory: true, multiple: false });
    if (typeof dir === "string") {
      this.form.patchValue({ vaultPath: dir });
      this.form.markAsDirty(); // user-driven → auto-save
    }
  }

  async pickModel(): Promise<void> {
    const file = await open({ directory: false, multiple: false });
    if (typeof file === "string") {
      this.form.patchValue({ whisperModelPath: file });
      this.form.markAsDirty(); // user-driven → auto-save
    }
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
      voiceprintEnabled: v.voiceprintEnabled,
      aecEnabled: v.aecEnabled,
      postAecEnabled: v.postAecEnabled,
      // Recording-storage cap: the form control is a STRING. Clamp to ≥1 GB — blank /
      // 0 / negative / NaN → null (no cap). A 0 must NEVER be sent: an in-session prune
      // with limit 0 would delete every non-locked recording's audio, and AppConfig::load
      // filters n>0 so a persisted 0 silently becomes "no cap" on restart (asymmetric).
      // Opt-in auto-prune rides every save like the other flags.
      audioStorageLimitGb: (() => {
        // The rendered input is type="number": its accessor commits a NUMBER
        // (or null when cleared) into this string-typed control — normalize
        // to a string FIRST or `.trim()` throws and kills the auto-save
        // subscriber for the whole session (adversarial-verify finding).
        const raw: unknown = v.audioStorageLimitGb;
        const s = raw == null ? "" : String(raw).trim();
        const n = Math.floor(Number(s));
        return s !== "" && Number.isFinite(n) && n >= 1 ? n : null;
      })(),
      audioAutoPrune: v.audioAutoPrune,
      modelSize: v.modelSize,
      // Only a save that FOLLOWS an explicit pick in the picker claims `"user"`;
      // `null` is the wire spelling of "absent" and means PRESERVE, so saving the
      // vault path (or anything else on this form) can never relabel how the model
      // size got there. The flag is consumed below, once the save resolves.
      modelSizeSource: this._modelSizeUserPick() ? "user" : null,
      liveAsrEngine: v.liveAsrEngine,
      // Brain-sidecar timeouts (seconds). Coerce to a finite positive number; a blank/invalid value
      // falls back to the default here AND the backend's `dto_to_config` re-defaults a 0 — belt and
      // suspenders, never a pathological zero window.
      brainIdleTimeoutSecs: coercePositive(v.brainIdleTimeoutSecs, 300),
      brainReadyTimeoutSecs: coercePositive(v.brainReadyTimeoutSecs, 90),
      brainHardCapSecs: coercePositive(v.brainHardCapSecs, 180),
      voiceTrigger: v.voiceTrigger,
      onboarded: this.loadedOnboarded,
      noteStyle: v.noteStyle,
      notesMode: v.notesMode,
      autoOrganize: v.autoOrganize,
      noteLanguage: v.noteLanguage,
      groundSummary: v.groundSummary,
      // Always explicit from Settings: empty clears. Other/older clients can omit the optional
      // wire key and the Rust merge preserves the stored value.
      glossary: v.glossary,
      // Phase H — brain / in-meeting voice assistant.
      brainBackend: v.brainBackend,
      realtimeReactions: v.realtimeReactions,
      // Proactive brain hints — round-tripped so a save preserves the mute.
      proactiveHintsEnabled: v.proactiveHintsEnabled,
      // Cross-meeting user memory — round-tripped so a save preserves the choice.
      userMemoryEnabled: v.userMemoryEnabled,
      updateCheckEnabled: v.updateCheckEnabled,
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
      // brain2 connectors (Phase 2) — Jira toggle + non-secret base URL/email are
      // settable from the form; its consent is PRESERVE-ONLY (granted via allowJira's
      // dedicated command), so a save just carries the current value back.
      jiraEnabled: v.jiraEnabled,
      jiraConsented: this.jiraConsented(),
      jiraBaseUrl: v.jiraBaseUrl,
      jiraEmail: v.jiraEmail,
      // brain2 connectors (Phase 3) — Slack toggle is settable from the form; its
      // consent is PRESERVE-ONLY (granted via allowSlack's dedicated command), so a
      // save just carries the current value back.
      slackEnabled: v.slackEnabled,
      slackConsented: this.slackConsented(),
      // brain2 connectors — Notion / ClickUp toggles (+ ClickUp's non-secret workspace id) are
      // settable from the form; their consent is PRESERVE-ONLY (granted via allowNotion /
      // allowClickup's dedicated commands), so a save just carries the current value back.
      notionEnabled: v.notionEnabled,
      notionConsented: this.notionConsented(),
      clickupEnabled: v.clickupEnabled,
      clickupConsented: this.clickupConsented(),
      clickupTeamId: v.clickupTeamId,
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
      // Notes feature — the three legacy note-assistant action toggles ride every
      // save like the other feature flags, so a settings save never disables one.
      noteAssistRefine: v.noteAssistRefine,
      noteAssistShorten: v.noteAssistShorten,
      noteAssistEnhance: v.noteAssistEnhance,
      // The 16 NEW actions collapse to a single off-list: an action whose toggle
      // is FALSE has its id added here (scales without one column per action).
      noteAssistActionsOff: this.noteAssistTogglesToOff(v),
      // M3-CLIENT sharing — preserve-only: carry the snapshot back unchanged so
      // the shell Save never clears the sharing server / share-egress consent
      // (the Account section is the sole owner of these values).
      shareBaseUrl: this.loadedShareBaseUrl,
      shareEgressConsented: this.loadedShareEgressConsented,
      // First-run sharing latch — preserve-only (the /welcome gateway owns it);
      // carry the snapshot back so the shell Save never clears it.
      sharingChoiceMade: this.loadedSharingChoiceMade,
    };
    try {
      await this.ipc.saveConfig(cfg);
      this._saved.set(true);
      // A save may have triggered an auto-prune (limit lowered) — refresh the usage report.
      await this.loadStorageReport();
      // The saved role/brainBackend keys may have changed what derive_posture
      // reads — re-read it so the posture segment reflects backend truth (never a
      // stale "Fully local" label over a just-saved cloud role).
      await this.refreshPosture();
      // A saved language/quality choice changes which Whisper model is needed —
      // re-check presence so the download hint stays honest (was previously done
      // by onModelChoiceChange's direct save, now retired).
      this._modelPresent.set(await this.ipc.modelPresent().catch(() => false));
      // The `"user"` claim has now been persisted — clear it so the NEXT save (a
      // vault path, a toggle) goes back to preserving whatever is stored.
      this._modelSizeUserPick.set(false);
      // …and re-read the catalog for the SAVED selection: `pendingDownloadBytes`,
      // every row's `downloaded` flag and the live-caption state all describe what
      // is stored, not what is typed, so the picker would otherwise keep describing
      // the previous pick. Refreshed HERE (after the save resolves) rather than on
      // the change event, which fires before the debounced save has stored anything.
      await this.machine.refresh();
      // A save may have flipped `mcp_require_token` (or minted the token on first use) — re-fetch
      // the MCP config so the copy block never shows a stale token-bearing / tokenless snippet.
      await this.refreshMcpConfig();
    } catch (e) {
      this._loadError.set(this.errorCopy.because("Save failed", e));
    }
  }

  /**
   * Map the config's `noteAssistActionsOff` list → the per-action toggle booleans
   * the form patches on load: an action is ON (true) unless its id is in the list.
   * Only the NEW actions are handled here (the legacy trio ride their own bools).
   */
  private noteAssistOffToToggles(off: string[]): Record<string, boolean> {
    const offSet = new Set(off);
    const toggles: Record<string, boolean> = {};
    for (const id of NOTE_ASSIST_NEW_ACTION_IDS) {
      toggles[id] = !offSet.has(id);
    }
    return toggles;
  }

  /**
   * Collapse the per-action toggle booleans (from the form value) → the config's
   * `noteAssistActionsOff` list: an action whose toggle is FALSE contributes its
   * id. Order-stable (catalog order) so a save produces a deterministic list.
   */
  private noteAssistTogglesToOff(v: Record<string, unknown>): string[] {
    return NOTE_ASSIST_NEW_ACTION_IDS.filter((id) => v[id] === false);
  }

  /** Re-fetch the token-bearing MCP config into its signal (after load / config save). */
  private async refreshMcpConfig(): Promise<void> {
    this._mcpConfig.set(await this.ipc.getMcpConfig().catch(() => ""));
    await this.refreshMcpStatus();
  }

  /**
   * Re-read the listener's real state.
   *
   * A failed read degrades to `unavailable`, never to `listening`: the whole point of this signal
   * is that the screen must not assert a running server it has not confirmed.
   */
  private async refreshMcpStatus(): Promise<void> {
    // NORMALIZED, not trusted. A backend that predates this command (or a test harness that does
    // not stub it) resolves `undefined` rather than rejecting, and `mcpStatus().port` on an
    // undefined value throws while rendering — the T6 failure mode where one absent field takes
    // the whole view down. A shape we cannot read degrades to `unavailable`, never to running.
    const raw = await this.ipc.getMcpStatus().catch(() => null);
    const state = raw?.state;
    this._mcpStatus.set({
      state:
        state === "listening" ||
        state === "portInUse" ||
        state === "starting" ||
        state === "unavailable"
          ? state
          : "unavailable",
      port: typeof raw?.port === "number" ? raw.port : 8765,
    });
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
      this._consentError.set(this.errorCopy.humanize(e));
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
      this._revokeError.set(this.errorCopy.humanize(e));
    } finally {
      this._revoking.set(false);
    }
  }

  /** Re-fetch the gated voiceprint management list (after a forget/clear, or on demand). */
  async refreshVoiceprints(): Promise<void> {
    this._voiceprints.set(await this.ipc.listVoiceprints().catch(() => []));
  }

  /**
   * FORGET one stored voiceprint (hard-delete a captured voice biometric), then
   * re-fetch the gated list. Best-effort: a failure leaves the list unchanged.
   */
  async forgetVoiceprint(id: string): Promise<void> {
    if (this._voiceprintBusy()) return;
    this._voiceprintBusy.set(true);
    try {
      await this.ipc.forgetVoiceprint(id);
      await this.refreshVoiceprints();
    } catch {
      // Leave the list as-is; the delete simply didn't take.
    } finally {
      this._voiceprintBusy.set(false);
    }
  }

  /** CLEAR every stored voiceprint ("forget all captured voices"), then re-fetch the list. */
  async clearVoiceprints(): Promise<void> {
    if (this._voiceprintBusy()) return;
    this._voiceprintBusy.set(true);
    try {
      await this.ipc.clearVoiceprints();
      await this.refreshVoiceprints();
    } catch {
      // Leave the list as-is.
    } finally {
      this._voiceprintBusy.set(false);
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
      this._webKeyError.set(this.errorCopy.humanize(e));
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
      this._webConsentError.set(this.errorCopy.humanize(e));
    } finally {
      this._webConsenting.set(false);
    }
  }

  /**
   * brain2 connectors (Phase 2) — store/replace the BYO Jira API token in the
   * Keychain, then re-probe presence so the "Token set ✓" pill flips. The value is
   * cleared from the input after saving (it's never shown back). Mirrors saveWebKey().
   */
  async saveJiraToken(): Promise<void> {
    const key = this.jiraTokenControl.value;
    if (!key.trim()) return;
    this._jiraTokenError.set(null);
    this._savingJiraToken.set(true);
    try {
      await this.ipc.setJiraToken(key);
      this.jiraTokenControl.setValue("");
      this._hasJiraToken.set(await this.ipc.hasJiraToken());
    } catch (e) {
      this._jiraTokenError.set(this.errorCopy.humanize(e));
    } finally {
      this._savingJiraToken.set(false);
    }
  }

  /**
   * brain2 connectors (Phase 2) — grant the one-time Jira egress consent via the
   * dedicated command (an explicit, auditable user act — NOT a side effect of a normal
   * settings save). After it resolves the brain may expose the Jira connector (when Jira
   * is enabled AND configured AND a token is stored). Mirrors allowWebSearch().
   */
  async allowJira(): Promise<void> {
    this._jiraConsentError.set(null);
    this._jiraConsenting.set(true);
    try {
      await this.ipc.consentToJira();
      this._jiraConsented.set(true);
    } catch (e) {
      this._jiraConsentError.set(this.errorCopy.humanize(e));
    } finally {
      this._jiraConsenting.set(false);
    }
  }

  /**
   * brain2 connectors (Phase 3) — store/replace the BYO Slack user token in the
   * Keychain, then re-probe presence so the "Token set ✓" pill flips. The value is
   * cleared from the input after saving (it's never shown back). Mirrors saveJiraToken().
   */
  async saveSlackToken(): Promise<void> {
    const key = this.slackTokenControl.value;
    if (!key.trim()) return;
    this._slackTokenError.set(null);
    this._savingSlackToken.set(true);
    try {
      await this.ipc.setSlackToken(key);
      this.slackTokenControl.setValue("");
      this._hasSlackToken.set(await this.ipc.hasSlackToken());
    } catch (e) {
      this._slackTokenError.set(this.errorCopy.humanize(e));
    } finally {
      this._savingSlackToken.set(false);
    }
  }

  /**
   * brain2 connectors (Phase 3) — grant the one-time Slack egress consent via the
   * dedicated command (an explicit, auditable user act — NOT a side effect of a normal
   * settings save). After it resolves the brain may expose the Slack connector (when Slack
   * is enabled AND a token is stored). Mirrors allowJira().
   */
  async allowSlack(): Promise<void> {
    this._slackConsentError.set(null);
    this._slackConsenting.set(true);
    try {
      await this.ipc.consentToSlack();
      this._slackConsented.set(true);
    } catch (e) {
      this._slackConsentError.set(this.errorCopy.humanize(e));
    } finally {
      this._slackConsenting.set(false);
    }
  }

  /**
   * brain2 connectors — store/replace the BYO Notion integration token in the
   * Keychain, then re-probe presence so the "Token set ✓" pill flips. The value is
   * cleared from the input after saving (it's never shown back). Mirrors saveSlackToken().
   */
  async saveNotionToken(): Promise<void> {
    const key = this.notionTokenControl.value;
    if (!key.trim()) return;
    this._notionTokenError.set(null);
    this._savingNotionToken.set(true);
    try {
      await this.ipc.setNotionToken(key);
      this.notionTokenControl.setValue("");
      this._hasNotionToken.set(await this.ipc.hasNotionToken());
    } catch (e) {
      this._notionTokenError.set(this.errorCopy.humanize(e));
    } finally {
      this._savingNotionToken.set(false);
    }
  }

  /**
   * brain2 connectors — grant the one-time Notion egress consent via the dedicated
   * command (an explicit, auditable user act — NOT a side effect of a normal settings
   * save). After it resolves the brain may expose the Notion connector (when Notion is
   * enabled AND a token is stored). Mirrors allowSlack().
   */
  async allowNotion(): Promise<void> {
    this._notionConsentError.set(null);
    this._notionConsenting.set(true);
    try {
      await this.ipc.consentToNotion();
      this._notionConsented.set(true);
    } catch (e) {
      this._notionConsentError.set(this.errorCopy.humanize(e));
    } finally {
      this._notionConsenting.set(false);
    }
  }

  /**
   * brain2 connectors — store/replace the BYO ClickUp personal API token in the
   * Keychain, then re-probe presence so the "Token set ✓" pill flips. The value is
   * cleared from the input after saving (it's never shown back). Mirrors saveNotionToken().
   */
  async saveClickupToken(): Promise<void> {
    const key = this.clickupTokenControl.value;
    if (!key.trim()) return;
    this._clickupTokenError.set(null);
    this._savingClickupToken.set(true);
    try {
      await this.ipc.setClickupToken(key);
      this.clickupTokenControl.setValue("");
      this._hasClickupToken.set(await this.ipc.hasClickupToken());
    } catch (e) {
      this._clickupTokenError.set(this.errorCopy.humanize(e));
    } finally {
      this._savingClickupToken.set(false);
    }
  }

  /**
   * brain2 connectors — grant the one-time ClickUp egress consent via the dedicated
   * command (an explicit, auditable user act — NOT a side effect of a normal settings
   * save). After it resolves the brain may expose the ClickUp connector (when ClickUp is
   * enabled AND a workspace id + token are configured). Mirrors allowNotion().
   */
  async allowClickup(): Promise<void> {
    this._clickupConsentError.set(null);
    this._clickupConsenting.set(true);
    try {
      await this.ipc.consentToClickup();
      this._clickupConsented.set(true);
    } catch (e) {
      this._clickupConsentError.set(this.errorCopy.humanize(e));
    } finally {
      this._clickupConsenting.set(false);
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
      this._gatewayKeyError.set(this.errorCopy.humanize(e));
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
      this._gatewayKeyError.set(this.errorCopy.humanize(e));
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
      this._gatewayModelError.set(this.errorCopy.humanize(e));
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
    // Guard the silent-empty-copy: if the config never loaded (keychain read failed → ""), do NOT
    // copy an empty string and flash a misleading "Copied". Re-fetch once; if still empty, surface
    // the error line and no-op so the user isn't handed a blank config that "does nothing".
    if (!this.mcpConfig().trim()) {
      await this.refreshMcpConfig();
      if (!this.mcpConfig().trim()) {
        return;
      }
    }
    try {
      await navigator.clipboard.writeText(this.mcpConfig());
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

  /**
   * Download the model for the chosen language + quality, then re-check presence.
   *
   * A CANCEL resolves normally (`status: "cancelled"`) and must NOT set the error
   * line — the user asked for it. Presence and the catalog are re-read either way,
   * because a cancel during the live-caption companion still leaves the batch model
   * on disk and the UI has to tell the truth about that.
   */
  async downloadModel(): Promise<void> {
    this._modelDownloadError.set(null);
    this._modelDownloadFrac.set(0);
    this._downloadingModel.set(true);
    try {
      await this.save(); // ensure the chosen language + size are persisted first
      await this.ipc.downloadModel();
      this._modelPresent.set(await this.ipc.modelPresent());
      await this.machine.refresh();
    } catch (e) {
      this._modelDownloadError.set(this.errorCopy.humanize(e));
    } finally {
      this._downloadingModel.set(false);
    }
  }

  /**
   * Cancel the in-flight model download. Best-effort and never surfaced as an error:
   * the download itself resolves with a "cancelled" outcome, which
   * {@link downloadModel} already handles.
   */
  async cancelModelDownload(): Promise<void> {
    try {
      await this.ipc.cancelModelDownload();
    } catch {
      // Nothing to cancel (or the command is unavailable) — the download either
      // finishes or fails on its own; there is nothing useful to tell the user here.
    }
  }

  /** Download the OPTIONAL parakeet live-ASR engine models (~600 MB), then re-check presence. */
  async downloadParakeet(): Promise<void> {
    this._parakeetDownloadError.set(null);
    this._parakeetDownloadFrac.set(0);
    this._downloadingParakeet.set(true);
    try {
      await this.ipc.downloadParakeetModels();
      this._parakeetPresent.set(
        await this.ipc.parakeetModelsPresent().catch(() => false),
      );
    } catch (e) {
      this._parakeetDownloadError.set(this.errorCopy.humanize(e));
    } finally {
      this._downloadingParakeet.set(false);
    }
  }
}
