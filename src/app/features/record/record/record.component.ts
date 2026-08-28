import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  OnInit,
  computed,
  effect,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { RecorderStore } from "../../../core/recorder.store";
import { IpcService } from "../../../core/ipc.service";
import type { Analytics, AppConfigDto } from "../../../core/models";
import { MicMuteToggleComponent } from "../mic-mute-toggle/mic-mute-toggle.component";
import { MeetingConversationComponent } from "../meeting-conversation/meeting-conversation.component";
import { ReTruthCardComponent } from "../re-truth-card/re-truth-card.component";
import { RecordingPlacementComponent } from "../recording-placement/recording-placement.component";
import { MeetingConversationStore } from "../../../core/meeting-conversation.store";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";
import {
  cloudDestinationLabel,
  connectionLabel,
} from "../../../core/copy/labels";

/** localStorage key for the permanent "no vault set" notice dismissal. */
const VAULT_NOTICE_DISMISSED_KEY = "murmur-vault-notice-dismissed";

/**
 * LIVE-CAPTION readiness, as reported by the backend in `get_config`
 * (Rust `AppConfigDto::live_captions` — a device/disk probe, NOT a persisted setting):
 *
 *  - `"ready"`        — the live tick has a live-safe whisper model to run.
 *  - `"modelMissing"` — nothing live-safe is downloaded. The heavy batch model (e.g. the turbo
 *                       default a fresh 12 GB+ Mac gets) is deliberately never run on the 3 s live
 *                       tick, so the small live-caption companion download simply never landed —
 *                       re-running the model download fixes it.
 *  - `"pinnedHeavy"`  — the configured live-model pin is itself a medium/large-class size that isn't
 *                       downloaded. A configuration outcome, not a failed download: Murmur never
 *                       fetches or runs a heavy model for live captions.
 *  - `"noModel"`      — no whisper model at all; the "Transcription model needed" banner owns it.
 *  - `""` / absent    — not probed (an older backend, or a mocked config): render nothing.
 */
type LiveCaptionsState =
  "ready" | "modelMissing" | "pinnedHeavy" | "noModel" | "";

@Component({
  selector: "app-record",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    RouterLink,
    MicMuteToggleComponent,
    MeetingConversationComponent,
    RecordingPlacementComponent,
    ReTruthCardComponent,
  ],
  host: { "(document:keydown)": "onKey($event)" },
  templateUrl: "./record.component.html",
  styleUrl: "./record.component.scss",
})
export class RecordComponent implements OnInit {
  readonly store = inject(RecorderStore);
  private readonly errorCopy = inject(ErrorCopyService);
  /** The in-meeting NOTES + @brain THREADS store. Injected + init()'d here (not only from
   * the surface) so it subscribes to the wake/result streams even before the surface shows. */
  readonly assistant = inject(MeetingConversationStore);
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);

  /**
   * Shadow-mode calibration (deliverable #5): once a recording finishes, read the
   * per-recording contradiction SHADOW count so the reactions rail can offer "the
   * brain would have flagged N — show them live?". The effect only CALLS an async
   * store method (the signal write happens inside the store, outside this effect).
   * Event-driven, no FE timer.
   */
  private readonly _shadowOnStop = effect(() => {
    if (this.store.stage() === "done") void this.assistant.refreshShadowCount();
  });

  // NOTE: "clear the conversation on a new recording" now lives in
  // MeetingConversationStore.setMeetingId (keyed on the meeting id, which survives
  // navigation), NOT in a per-component isRecording-edge effect. The old effect
  // wiped the thread when you left the record tab and came back mid-recording,
  // because its edge state (a plain field) reset to false on component re-mount.

  /** Name of a running meeting app (Zoom/Teams/Webex), or null if none detected. */
  readonly detectedApp = signal<string | null>(null);
  /** Once dismissed, the nudge stays hidden for the rest of this session. */
  private readonly nudgeDismissed = signal(false);
  /** Handle for the meeting-app poll — cleared on destroy (no leaked interval). */
  private meetingAppPoll: ReturnType<typeof setInterval> | null = null;

  /** Release handle for the model-download stream — dropped on destroy. */
  private unlistenModelDownload: UnlistenFn | null = null;

  /** The in-pill mic-mute toggle — its `muted()` signal drives the stage hint. */
  private readonly micToggle = viewChild(MicMuteToggleComponent);

  /** Latest partial transcript, trimmed — drives the ephemeral caption line. */
  readonly liveCaption = computed(() => this.store.liveCaption().trim());

  /** Latest settings snapshot, refreshed on entry — used for the readiness guard. */
  private readonly config = signal<AppConfigDto | null>(null);

  /** Best-effort: is the default output the built-in speakers? null/undetermined ⇒ assume yes. */
  private readonly onSpeakers = signal<boolean>(true);

  /** Headphones hint: system-audio capture + built-in speakers = echo into the mic (rec #5). */
  readonly headphonesHint = computed(
    () => (this.config()?.captureSystemAudio ?? false) && this.onSpeakers(),
  );

  /**
   * Proactive brain hints (the global mute, default ON). Gates the recall card
   * in the conversation surface; the backend mutes the event source too when
   * off — this is the render-side half of the belt and braces.
   */
  readonly hintsEnabled = computed(
    () => this.config()?.proactiveHintsEnabled ?? true,
  );

  /** ENHANCE-MY-NOTES: mode from config; missing/empty ⇒ enhance (the backend default). */
  readonly enhanceMode = computed(
    () => (this.config()?.notesMode ?? "enhance") === "enhance",
  );
  /** The hero trigger: summarizing a meeting whose notes will be the skeleton. */
  readonly enhancingNotes = computed(
    () =>
      this.enhanceMode() &&
      this.assistant.hasPersistedNotes() &&
      this.store.stage() === "summarizing",
  );
  /** Settled: done + the notes were enhanced — keeps the surface up through "Saved ✓". */
  readonly enhanceSettled = computed(
    () =>
      this.enhanceMode() &&
      this.assistant.hasPersistedNotes() &&
      this.store.stage() === "done",
  );

  /**
   * Show the conversation thread — the full-height main surface of the
   * conversation-first record screen. It is the home for BOTH note-taking AND
   * the agent, so it surfaces during ANY recording (notes always persist via
   * `save_manual_notes`, even when the brain backend is off — only the @brain
   * agent path is then unavailable), whenever realtime reactions are enabled,
   * and whenever a manual "Ask AI" is listening / in flight (so the answer has a
   * home). The thread itself subscribes to the wake/result streams regardless.
   */
  readonly showAssistant = computed(() => {
    if (this.store.stage() === "done") {
      return false;
    }
    const c = this.config();
    // Mirror the LIVE role resolver: an explicit roleLiveConnection wins over
    // the legacy brainBackend fallback (Ask=Off compat-writes brainBackend and
    // must not hide a Live surface that is explicitly a cloud provider).
    const liveConn = c ? c.roleLiveConnection || c.brainBackend : "";
    const enabled = !!c && c.realtimeReactions === true && liveConn !== "off";
    return (
      // D2 (idle redesign, 2026-07-18): `realtimeReactions` surfaces the companion
      // editor + Ask Brain ONLY when a live meeting exists. At pure idle (no
      // meeting) this term used to be true on the setting alone, mounting the
      // embedded companion editor with a null note id — which then spun
      // "Loading note…" forever. Every OTHER trigger below is already an active
      // state (recording / listening / processing / enhance), so gating just this
      // one leaves the idle screen as the launch hero (+ analytics) with no
      // purposeless, permanently-loading note pane.
      (enabled && !!this.store.meetingId()) ||
      this.store.isRecording() ||
      this.assistant.listening() ||
      this.assistant.processing() ||
      this.assistant.manualAskInFlight() ||
      (this.enhanceMode() &&
        this.isProcessing() &&
        this.assistant.hasPersistedNotes())
    );
  });

  /**
   * The backend's live-caption readiness for THIS machine. Read through a narrow structural cast:
   * the key is DISPLAY-ONLY (`get_config` fills it; a settings save can neither set nor clear it),
   * so it is not part of the settings-shaped `AppConfigDto` the FE round-trips — and a backend or
   * mock that doesn't send it must read as "not probed" (`""`) and render nothing.
   */
  readonly liveCaptions = computed<LiveCaptionsState>(
    () =>
      ((this.config() as (AppConfigDto & { liveCaptions?: string }) | null)
        ?.liveCaptions as LiveCaptionsState) || "",
  );

  /**
   * No live captions this recording — the live tick has no live-safe model. The heat policy is
   * deliberate (a medium/large encoder every 3 s saturates the shared Metal GPU for the whole
   * meeting), so the honest thing is to SAY captions are off rather than show "Listening…" forever.
   * `"noModel"` is excluded: the transcription-model download banner already owns that state.
   */
  readonly liveCaptionsOff = computed(
    () =>
      this.liveCaptions() === "modelMissing" ||
      this.liveCaptions() === "pinnedHeavy",
  );

  /**
   * Which cause — a HEAVY live-model pin (a configuration choice: nothing to download on the user's
   * behalf) vs a missing/failed live-safe companion download (retryable right here). Drives the
   * notice copy + whether a Download action is offered at all.
   */
  readonly liveCaptionsHeavyPin = computed(
    () => this.liveCaptions() === "pinnedHeavy",
  );

  /** Dismissed for this session only (like the meeting-app nudge). */
  private readonly liveCaptionsNoticeDismissed = signal(false);

  /**
   * The live-caption COMPANION fetch behind the notice's "Download it" — deliberately a SEPARATE
   * busy flag from {@link downloadingModel}. That one means "the model recording needs isn't here
   * yet", which is why {@link canRecord} gates on it; this one runs with the batch model already
   * present, so it must never disable Start — the notice's own copy promises recording is
   * unaffected, and it is.
   */
  readonly downloadingLiveCompanion = signal(false);
  /**
   * Why the companion retry didn't help — either the command rejected, or it returned but the
   * refreshed state still says no live captions (the backend swallows a companion failure so the
   * batch model still counts as downloaded). Never let a click look like it worked when it didn't.
   */
  readonly liveCompanionError = signal<string | null>(null);

  /**
   * Show the calm, non-blocking "live captions are off" notice: only when a transcription model IS
   * present (otherwise the download banner is the right thing to show), while NOT recording (the
   * footer's "Captions off" indicator carries it then), and not dismissed this session.
   */
  readonly showLiveCaptionsNotice = computed(
    () =>
      this.liveCaptionsOff() &&
      this.modelPresent() === true &&
      !this.store.isRecording() &&
      !this.liveCaptionsNoticeDismissed(),
  );

  /**
   * True when no Obsidian vault folder is configured. The vault is EXPORT-ONLY — every note
   * is always saved to Murmur's canonical DB — so this NO LONGER blocks recording. It only
   * drives the calm, dismissible "set a vault to also export" info notice + the "done" hint copy.
   */
  readonly vaultMissing = computed(() => {
    const c = this.config();
    return !c || !c.vaultPath || c.vaultPath.trim() === "";
  });

  /**
   * Dismissed permanently (localStorage — live-found bug, 2026-07-12: this used
   * to be a plain component-local signal, so it "forgot" the dismissal on the
   * very next remount, e.g. navigating away and back to /record within the
   * same session. Re-appears only if a vault gets configured then unset again.
   */
  private readonly vaultNoticeDismissed = signal(
    this.readVaultNoticeDismissed(),
  );

  /**
   * Show the calm, non-blocking "no vault set" info notice: only when no vault is configured
   * and the user hasn't dismissed it. It never gates recording.
   */
  readonly showVaultNotice = computed(
    () => this.vaultMissing() && !this.vaultNoticeDismissed(),
  );

  private readVaultNoticeDismissed(): boolean {
    try {
      return localStorage.getItem(VAULT_NOTICE_DISMISSED_KEY) === "1";
    } catch {
      return false;
    }
  }

  /**
   * True when the last failure was the backend's cloud-egress consent gate, so the surface can
   * offer "Allow" instead of an error banner — never a silent failure.
   *
   * This matches the STABLE `[cloud-consent]` CODE
   * (`src-tauri/src/errcode.rs::CLOUD_CONSENT`, emitted by
   * `summarize::make_provider_resolved`), never the prose. It used to regex-match the Rust
   * sentence "cloud egress not consented", which meant rewording that sentence — an ordinary
   * de-jargoning edit — silently broke the consent flow for every cloud user: the Allow banner
   * would stop rendering and the raw backend string would surface instead.
   *
   * `RecorderStore.errorCode` carries the code off the RAW wire string; `store.error()` is already
   * humanized and no longer contains it.
   */
  readonly needsCloudConsent = computed(
    () => this.store.errorCode() === "cloud-consent",
  );

  /** Human label for the configured provider (for the consent copy). */
  readonly providerLabel = computed(() => {
    const id = this.config()?.providerId;
    // "The Anthropic API" reads better than the bare label in this sentence position.
    return id === "anthropic" ? "The Anthropic API" : connectionLabel(id);
  });

  /**
   * Human name of the destination the redacted transcript goes to (for the
   * consent copy). This banner only shows after the backend's fail-closed
   * `egress_is_cloud` gate refused (`needsCloudConsent`), so the provider is
   * cloud-classified by definition — see `cloudDestinationLabel`.
   */
  readonly cloudDestination = computed(() =>
    cloudDestinationLabel(this.config()?.providerId),
  );

  /** True while the one-time consent command + retry are in flight. */
  readonly consenting = signal(false);

  /** Real Whisper-model presence (null = checking). */
  readonly modelPresent = signal<boolean | null>(null);
  readonly downloadingModel = signal(false);
  readonly modelDownloadError = signal<string | null>(null);

  /** Busy but not capturing audio → transcribing / summarizing / exporting / saved reconciliation. */
  readonly isProcessing = computed(
    () => this.store.isBusy() && !this.store.isRecording(),
  );

  /**
   * BOUND the record surface to the viewport (fixed height → the note scrolls
   * internally) only while the conversation/editor is mounted. The terminal
   * result intentionally stays unbounded so its card and optional Re-Truth can
   * scroll naturally on a short window.
   */
  readonly boundToViewport = computed(
    () => this.showAssistant() && !this.store.error(),
  );

  /**
   * The processing status line. Prefer the backend's status message — it is
   * already a clean, properly-cased sentence ("Summarizing with Claude Code…",
   * "Writing note to vault…") — and fall back to a Title-cased stage word only
   * when no message has been emitted yet. This replaces the old CSS
   * `text-transform: capitalize` on `.proc-label`, which mangled the full
   * sentence into "Summarizing With Provider 'Claude_code'…".
   */
  readonly procLabel = computed(() => {
    const msg = this.store.message().trim();
    if (msg) return msg;
    const stage = this.store.stage();
    const labels: Record<string, string> = {
      recording: "Recording",
      transcribing: "Transcribing",
      summarizing: "Summarizing",
      exporting: "Exporting",
      saved: "Saved",
      finalized: "Finalizing",
      done: "Done",
      error: "Error",
    };
    return labels[stage] ?? stage.charAt(0).toUpperCase() + stage.slice(1);
  });

  /**
   * Whether the Record action is allowed right now. A missing vault does NOT block recording
   * (the note is always saved to Murmur; the vault is export-only) — only a missing/downloading
   * model or an in-flight pipeline gates it.
   */
  readonly canRecord = computed(
    () =>
      this.modelPresent() !== false &&
      !this.downloadingModel() &&
      !this.store.isBusy(),
  );

  /**
   * Show the start-recording nudge only when a meeting app is running, we're
   * not already recording, the user hasn't dismissed it this session, and
   * recording is actually possible. A nudge — never blocks the screen.
   */
  readonly showNudge = computed(
    () =>
      this.detectedApp() !== null &&
      !this.store.isRecording() &&
      !this.nudgeDismissed() &&
      this.canRecord(),
  );

  /** Elapsed recording time as m:ss. */
  readonly elapsedLabel = computed(() => {
    const s = this.store.elapsed();
    const m = Math.floor(s / 60);
    const sec = (s % 60).toString().padStart(2, "0");
    return `${m}:${sec}`;
  });

  /** Context line beneath the bar. */
  readonly hint = computed(() => {
    if (this.store.isRecording()) {
      // When the mic is muted the recording continues from system audio only —
      // make that unmistakable in the prominent hint line beneath the pill.
      if (this.micToggle()?.muted())
        return "Mic muted — still capturing others. Press ⌘R or Stop when done.";
      return "Recording — press ⌘R or Stop when done.";
    }
    if (this.isProcessing())
      return this.enhanceMode() && this.assistant.hasPersistedNotes()
        ? "Transcribing on-device, then enhancing your notes…"
        : "Transcribing on-device, then summarizing…";
    if (this.modelPresent() === false) return "Download the model to start.";
    if (this.store.stage() === "done") {
      // A vault being CONFIGURED doesn't mean THIS note exported — the backend
      // legitimately skips export (locked folder, resummarize on an already-sealed
      // folder) and returns `exportedPath: null` even with a vault set. Only claim
      // "in the vault" when this note's own exportedPath says so.
      const exported =
        !this.vaultMissing() && !!this.store.lastNote()?.exportedPath;
      if (this.enhanceSettled())
        return exported
          ? "Saved ✓ — your enhanced note is in the vault."
          : "Saved ✓ — your enhanced note is in Murmur.";
      return exported
        ? "Saved ✓ — your note is in the vault."
        : "Saved ✓ — your note is in Murmur.";
    }
    return "On-device transcription · your audio never leaves this Mac.";
  });

  /** Aggregate stats for the minimal home strip (null = not yet loaded). */
  readonly analytics = signal<Analytics | null>(null);

  /**
   * Sparkline bars: per-day meeting counts scaled to a 0–100% height, padded to
   * a steady width so a single busy day doesn't render as one lonely bar.
   */
  readonly spark = computed<{ date: string; h: number }[]>(() => {
    const a = this.analytics();
    if (!a || a.perDay.length === 0) return [];
    const days = [...a.perDay].sort((x, y) => x.date.localeCompare(y.date));
    const max = Math.max(...days.map((d) => d.count), 1);
    const bars = days.map((d) => ({
      date: d.date,
      h: Math.round((d.count / max) * 100),
    }));
    // Left-pad with flat placeholders so the strip keeps a consistent rhythm.
    const minBars = 14;
    if (bars.length < minBars) {
      const pad = Array.from({ length: minBars - bars.length }, (_, i) => ({
        date: `pad-${i}`,
        h: 0,
      }));
      return [...pad, ...bars];
    }
    return bars;
  });

  async ngOnInit(): Promise<void> {
    this.store.enterRecordRoute();
    // `/record` owns only the terminal review presentation. Clear an old
    // done/error visit immediately on remount; active capture/finalization is
    // backend-owned and `resetRoutePresentation()` deliberately preserves it.
    this.resetRoutePresentation();

    // Register the teardown FIRST — before ANY await. This ngOnInit suspends
    // several times below, and the component can be destroyed mid-await (the
    // boot tab-restore navigation in `AppComponent.ngOnInit` replaces the
    // freshly-mounted /record with the persisted active tab's route).
    // Registering `DestroyRef.onDestroy` AFTER an await then throws NG0911
    // ("View has already been destroyed") from the resumed continuation — and,
    // worse, the meeting-app poll interval created just above that line was
    // never handed to a cleanup, so it kept polling `detect_meeting_app`
    // forever on a dead view. `destroyed` additionally gates the late
    // continuation so the poll is never even started after destruction.
    let destroyed = false;
    this.destroyRef.onDestroy(() => {
      destroyed = true;
      this.store.leaveRecordRoute();
      if (this.meetingAppPoll !== null) {
        clearInterval(this.meetingAppPoll);
        this.meetingAppPoll = null;
      }
      this.unlistenModelDownload?.();
      this.unlistenModelDownload = null;
      this.resetRoutePresentation();
    });
    // Live-caption readiness is a DEVICE/DISK fact, not a setting, so it can change while this
    // screen is open — a model download finishing anywhere (Settings, the onboarding wizard, the
    // notice below) can flip "Live captions are off" to on. Follow the download stream so the
    // notice + the footer's "Captions off" indicator clear on their own instead of contradicting
    // what `start_recording` would actually do until a remount.
    try {
      const unlisten = await this.ipc.onModelDownload((p) => {
        if (p.done) void this.onModelDownloadDone();
      });
      // Destroyed while the subscription was in flight — release it immediately; the onDestroy
      // above has already run and would never see this handle.
      if (destroyed) unlisten();
      else this.unlistenModelDownload = unlisten;
    } catch {
      // No download stream (older backend / a mock without event plumbing) — the state still
      // refreshes on the next mount and after this screen's own download action.
    }
    await this.store.init();
    // On the first app mount `init()` also refreshes the historical last note.
    // Scrub that idle-only snapshot after init so it cannot become a stale
    // terminal review; an active recording or pipeline still remains intact.
    this.resetRoutePresentation();
    // Subscribe the notes/threads store to the wake/result + BOTH tool-trace
    // streams now, regardless of whether the surface is visible yet — otherwise
    // events fired before it renders (or while the config snapshot is stale) drop.
    void this.assistant.init();
    await this.refreshConfig();
    void this.ipc
      .outputIsBuiltinSpeakers()
      .then((v) => this.onSpeakers.set(v ?? true));
    this.modelPresent.set(await this.ipc.modelPresent());
    // Stats are secondary — never let a failure here block the record screen.
    try {
      this.analytics.set(await this.ipc.getAnalytics());
    } catch {
      this.analytics.set(null);
    }

    // Meeting-app detection: check once now, then poll on a tracked interval
    // (cleared by the onDestroy registered at the top of this method). Skipped
    // entirely when the view died during the awaits above — the cleanup has
    // already run by then, so a poll started here could never be cleared.
    if (destroyed) {
      return;
    }
    void this.checkMeetingApp();
    this.meetingAppPoll = setInterval(
      () => void this.checkMeetingApp(),
      12_000,
    );
  }

  /**
   * Monotonic guard for {@link refreshConfig}: the snapshot is re-read from several places (mount,
   * the download stream, this screen's own actions), so a slow earlier read must never overwrite a
   * newer one — same shape as `entity-detail.component.ts`'s `_load`.
   */
  private configRequestId = 0;

  /** Re-read the settings/readiness snapshot into {@link config}, dropping superseded responses. */
  private async refreshConfig(): Promise<void> {
    const requestId = ++this.configRequestId;
    const cfg = await this.ipc.getConfig();
    if (requestId !== this.configRequestId) return;
    this.config.set(cfg);
  }

  /**
   * A model download finished SOMEWHERE (this screen, Settings, the wizard) — re-probe the two
   * facts it can change: whether a transcription model exists at all, and whether the live tick now
   * has a live-safe model. Best-effort: a failed probe leaves the last-known state rather than
   * claiming a state we didn't read.
   */
  private async onModelDownloadDone(): Promise<void> {
    try {
      this.modelPresent.set(await this.ipc.modelPresent());
      await this.refreshConfig();
    } catch {
      /* keep the last-known state */
    }
  }

  /** Best-effort poll for a running meeting app; failures leave the nudge hidden. */
  private async checkMeetingApp(): Promise<void> {
    try {
      this.detectedApp.set(await this.ipc.detectMeetingApp());
    } catch {
      this.detectedApp.set(null);
    }
  }

  /** Every main Record-screen start is explicit Unfiled. */
  async startRecording(): Promise<void> {
    if (this.canRecord()) {
      await this.store.start(null);
    }
  }

  /** Clear route-local terminal state and its matching assistant focus together. */
  private resetRoutePresentation(): void {
    if (this.store.resetRoutePresentation()) {
      // `setMeetingId(null)` clears companion identity/focus but deliberately
      // preserves the in-memory thread. A terminal route reset owns that
      // presentation too, so purge it before dropping the meeting pointer.
      this.assistant.clear();
      this.assistant.setMeetingId(null);
    }
  }

  /** Nudge ghost action — hide it for the rest of this session. */
  dismissNudge(): void {
    this.nudgeDismissed.set(true);
  }

  /** Live-captions notice dismiss — this session only (the state itself is unchanged). */
  dismissLiveCaptionsNotice(): void {
    this.liveCaptionsNoticeDismissed.set(true);
  }

  /** No-vault info-notice dismiss — permanent (localStorage), not just this session. */
  dismissVaultNotice(): void {
    this.vaultNoticeDismissed.set(true);
    try {
      localStorage.setItem(VAULT_NOTICE_DISMISSED_KEY, "1");
    } catch {
      /* private-mode / storage-disabled — session-only dismissal is fine. */
    }
  }

  /** ⌘R / Ctrl+R toggles recording. */
  onKey(e: KeyboardEvent): void {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "r") {
      e.preventDefault();
      if (this.store.isRecording()) {
        void this.store.stop();
      } else if (this.canRecord()) {
        void this.startRecording();
      }
    }
  }

  /**
   * Start a pointer-activated Stop before focus transfer blurs the companion editor. WebKit may
   * otherwise cancel the later `click` when a synchronous save-error toast changes the DOM between
   * pointerdown and pointerup. `RecorderStore.stop()` flips out of recording synchronously, so the
   * subsequent click path observes `isRecording() === false` and cannot double-submit.
   */
  onStopPointerDown(event: PointerEvent): void {
    if (event.button === 0 && this.store.isRecording()) {
      void this.store.stop();
    }
  }

  /** Keyboard/synthetic activation fallback; pointer activation is handled above. */
  onStopClick(): void {
    if (this.store.isRecording()) {
      void this.store.stop();
    }
  }

  /** Summon the floating always-on-top bar (also bound to ⌘⇧R globally). */
  popOut(): void {
    void this.ipc.toggleBar();
  }

  /**
   * Grant the one-time cloud-egress consent, then retry summarizing the meeting
   * that just failed the gate. The transcript is already captured + on disk, so a
   * `resummarize` finishes the note without re-recording. After consent we refresh
   * the config snapshot so `providerLabel` / readiness reflect the new state.
   */
  async allowCloudAndRetry(): Promise<void> {
    this.consenting.set(true);
    try {
      await this.ipc.consentToCloudEgress();
      await this.refreshConfig();
      const id = this.store.meetingId();
      if (id) {
        await this.store.resummarize(id);
      }
    } catch {
      // The store surfaces a fresh error banner on a failed retry; nothing to do here.
    } finally {
      this.consenting.set(false);
    }
  }

  /**
   * Download the Whisper model, then re-check presence. This is the BLOCKING download (the model
   * recording itself needs) — hence `downloadingModel`, which `canRecord` gates on. The backend
   * fetches the live-safe caption companion in the same command when the batch model can't serve
   * live captions, so the readiness snapshot is refreshed too.
   */
  async downloadModel(): Promise<void> {
    this.modelDownloadError.set(null);
    this.downloadingModel.set(true);
    try {
      await this.ipc.downloadModel();
      this.modelPresent.set(await this.ipc.modelPresent());
      await this.refreshConfig();
    } catch (e) {
      this.modelDownloadError.set(this.errorCopy.humanize(e));
    } finally {
      this.downloadingModel.set(false);
    }
  }

  /**
   * Retry the live-caption COMPANION fetch from the "live captions are off" notice. Same backend
   * command (`download_model` re-runs the companion decision against the model already on disk and
   * fetches only what's missing), but tracked in {@link downloadingLiveCompanion} so it does NOT
   * disable Start: the batch model is already present, recording is genuinely unaffected, and the
   * notice says so.
   *
   * The backend swallows a companion failure by design (the batch model is what gates recording),
   * so success is judged by the REFRESHED readiness state, not by the command resolving.
   */
  async downloadLiveCompanion(): Promise<void> {
    this.liveCompanionError.set(null);
    this.downloadingLiveCompanion.set(true);
    try {
      await this.ipc.downloadModel();
      await this.refreshConfig();
      if (this.liveCaptionsOff()) {
        this.liveCompanionError.set(
          "The live-caption model still isn't on this Mac — check your connection and try again.",
        );
      }
    } catch (e) {
      this.liveCompanionError.set(this.errorCopy.humanize(e));
    } finally {
      this.downloadingLiveCompanion.set(false);
    }
  }

  /** Presentational only: seconds → compact "1h 5m" / "12m" / "45s". */
  formatDuration(durationS: number): string {
    const total = Math.max(0, Math.round(durationS));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    if (h > 0) {
      return `${h}h ${m}m`;
    }
    if (m > 0) {
      return `${m}m`;
    }
    return `${s}s`;
  }
}
