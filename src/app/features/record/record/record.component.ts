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
import { RecorderStore } from "../../../core/recorder.store";
import { IpcService } from "../../../core/ipc.service";
import type { Analytics, AppConfigDto } from "../../../core/models";
import { MicMuteToggleComponent } from "../mic-mute-toggle/mic-mute-toggle.component";
import { MeetingConversationComponent } from "../meeting-conversation/meeting-conversation.component";
import { BrainRevealCardComponent } from "../brain-reveal-card/brain-reveal-card.component";
import { ReTruthCardComponent } from "../re-truth-card/re-truth-card.component";
import { MeetingConversationStore } from "../../../core/meeting-conversation.store";

/** localStorage key for the permanent "no vault set" notice dismissal. */
const VAULT_NOTICE_DISMISSED_KEY = "murmur-vault-notice-dismissed";

@Component({
  selector: "app-record",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    RouterLink,
    MicMuteToggleComponent,
    MeetingConversationComponent,
    BrainRevealCardComponent,
    ReTruthCardComponent,
  ],
  host: { "(document:keydown)": "onKey($event)" },
  templateUrl: "./record.component.html",
  styleUrl: "./record.component.scss",
})
export class RecordComponent implements OnInit {
  readonly store = inject(RecorderStore);
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

  /** Bars in the live waveform (driven by the real mic level signal). 16 bars fit the fixed
   * 84px `.wave` box (16×2px min + 15×2px gap ≈ 62px) with room to flex; 28 overflowed the box
   * and spilled into the caption ("…IIIIDzięki za oglądanie!"). */
  readonly bars = Array.from({ length: 16 }, (_, i) => i);

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
        this.assistant.hasPersistedNotes()) ||
      this.enhanceSettled()
    );
  });

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
   * same session — mirrors {@link BrainRevealCardComponent}'s one-time-seen
   * pattern). Re-appears only if a vault gets configured then unset again.
   */
  private readonly vaultNoticeDismissed = signal(this.readVaultNoticeDismissed());

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
   * True when the last failure was the backend's cloud-egress consent gate. We
   * detect the stable "cloud egress not consented" marker from `make_provider`
   * and surface a friendly consent prompt instead of the raw error banner —
   * never a silent failure.
   */
  readonly needsCloudConsent = computed(() => {
    const e = this.store.error();
    return !!e && /cloud egress not consented/i.test(e);
  });

  /** Human label for the configured provider (for the consent copy). */
  readonly providerLabel = computed(() => {
    switch (this.config()?.providerId) {
      case "anthropic":
        return "The Anthropic API";
      case "claude_code":
        return "Claude Code";
      case "gateway":
        return "Kong AI Gateway";
      case "ollama":
        return "Ollama";
      default:
        return "This provider";
    }
  });

  /**
   * Human name of the destination the redacted transcript goes to (for the
   * consent copy). This banner only shows after the backend's fail-closed
   * `egress_is_cloud` gate refused (`needsCloudConsent`), so the provider is
   * cloud-classified by definition — for ollama that means the base URL is
   * non-loopback, hence "your remote Ollama server" without re-parsing it here.
   */
  readonly cloudDestination = computed(() => {
    switch (this.config()?.providerId) {
      case "anthropic":
      case "claude_code":
        return "Anthropic's cloud";
      case "gateway":
        return "your Kong AI Gateway";
      case "ollama":
        return "your remote Ollama server";
      default:
        return "your provider's cloud";
    }
  });

  /** True while the one-time consent command + retry are in flight. */
  readonly consenting = signal(false);

  /** Real Whisper-model presence (null = checking). */
  readonly modelPresent = signal<boolean | null>(null);
  readonly downloadingModel = signal(false);
  readonly modelDownloadError = signal<string | null>(null);

  /** Busy but not capturing audio → transcribing / summarizing / exporting. */
  readonly isProcessing = computed(
    () => this.store.isBusy() && !this.store.isRecording(),
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
      const exported = !this.vaultMissing() && !!this.store.lastNote()?.exportedPath;
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
      if (this.meetingAppPoll !== null) {
        clearInterval(this.meetingAppPoll);
        this.meetingAppPoll = null;
      }
    });
    await this.store.init();
    // Subscribe the notes/threads store to the wake/result + BOTH tool-trace
    // streams now, regardless of whether the surface is visible yet — otherwise
    // events fired before it renders (or while the config snapshot is stale) drop.
    void this.assistant.init();
    this.config.set(await this.ipc.getConfig());
    void this.ipc.outputIsBuiltinSpeakers().then((v) => this.onSpeakers.set(v ?? true));
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

  /** Best-effort poll for a running meeting app; failures leave the nudge hidden. */
  private async checkMeetingApp(): Promise<void> {
    try {
      this.detectedApp.set(await this.ipc.detectMeetingApp());
    } catch {
      this.detectedApp.set(null);
    }
  }

  /** Nudge primary action — kick off a recording, then let it fade out. */
  startFromNudge(): void {
    void this.store.start();
  }

  /** Nudge ghost action — hide it for the rest of this session. */
  dismissNudge(): void {
    this.nudgeDismissed.set(true);
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
        void this.store.start();
      }
    }
  }

  /** Summon the floating always-on-top bar (also bound to ⌘⇧R globally). */
  popOut(): void {
    void this.ipc.toggleBar();
  }

  /**
   * CLICK-TO-STOP toggle for the "Ask AI" (✨) button. First click opens the
   * voice-command listener (no wake phrase); a second click — while listening —
   * stops it so the FULL utterance is dispatched (→ processing). The backend
   * streams listening/processing over EVENT_VOICE_COMMAND_LISTENING /
   * EVENT_VOICE_COMMAND_PROCESSING and the spoken answer lands in a thread on the
   * notes surface below. Swallow rejections (e.g. brain backend off) — the store
   * resets its listening/processing/in-flight state on error.
   */
  toggleAsk(): void {
    if (this.assistant.listening()) {
      void this.assistant.endAsk().catch(() => {
        /* stop failed — store cleared processing/in-flight */
      });
    } else {
      void this.assistant.askNow().catch(() => {
        /* listener unavailable — store resets the in-flight/listening state */
      });
    }
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
      this.config.set(await this.ipc.getConfig());
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

  /** Download the Whisper model, then re-check presence. */
  async downloadModel(): Promise<void> {
    this.modelDownloadError.set(null);
    this.downloadingModel.set(true);
    try {
      await this.ipc.downloadModel();
      this.modelPresent.set(await this.ipc.modelPresent());
    } catch (e) {
      this.modelDownloadError.set(String(e));
    } finally {
      this.downloadingModel.set(false);
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
