import {
  DestroyRef,
  Injectable,
  computed,
  inject,
  signal,
} from "@angular/core";
import { toObservable, toSignal } from "@angular/core/rxjs-interop";
import {
  catchError,
  from,
  interval,
  map,
  of,
  startWith,
  switchMap,
} from "rxjs";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "./ipc.service";
import type { NoteDto, Stage, StatusPayload } from "./models";
import { RecordingFlushService } from "./recording-flush.service";
import { ToastService } from "../services/toast.service";
import { ErrorCopyService } from "./copy/error-copy.service";
import { AskHistoryPrivacyBarrierService } from "./ask-history-privacy-barrier.service";

/**
 * Human byte label (binary), matching the Storage settings section's `mb()`:
 * ≥1 GiB → "x.xx GB", else "N MB". Kept module-local so the toast copy and the
 * Storage-section chip round identically.
 */
function humanBytes(bytes: number): string {
  if (bytes >= 1024 * 1024 * 1024)
    return (bytes / (1024 * 1024 * 1024)).toFixed(2) + " GB";
  return Math.round(bytes / (1024 * 1024)) + " MB";
}

/**
 * Signal-based recorder state: status stage, last note, and last error.
 * Subscribes to the EVENT_STATUS stream from the Rust core.
 */
@Injectable({ providedIn: "root" })
export class RecorderStore {
  private readonly ipc = inject(IpcService);
  private readonly toast = inject(ToastService);
  private readonly flushService = inject(RecordingFlushService);
  private readonly errorCopy = inject(ErrorCopyService);
  private readonly privacyBarrier = inject(AskHistoryPrivacyBarrierService);
  private readonly destroyRef = inject(DestroyRef);

  private readonly _stage = signal<Stage>("idle");
  private readonly _message = signal<string>("");
  private readonly _lastNote = signal<NoteDto | null>(null);
  private readonly _error = signal<string | null>(null);
  private readonly _meetingId = signal<string | null>(null);
  private readonly _liveCaption = signal<string>("");

  readonly stage = this._stage.asReadonly();
  readonly message = this._message.asReadonly();
  readonly lastNote = this._lastNote.asReadonly();

  /**
   * The last failure, as a sentence a person can read.
   *
   * `_error` holds the RAW wire string (the `AppError` display, or the terminal `EVENT_STATUS`
   * message — which `pipeline.rs` builds with the same `to_string()`). It is deliberately private:
   * ~2100 `AppError` constructions in the Rust crate carry developer vocabulary, so nothing may
   * render it directly. Behaviour branches read {@link errorCode} instead.
   */
  readonly error = computed(() => {
    const raw = this._error();
    return raw === null ? null : this.errorCopy.humanize(raw, "recording");
  });

  /**
   * The stable `[code]` of the last failure, or `null` for an anonymous one.
   *
   * This is what `record.component.ts` asks to decide whether to show the cloud-consent "Allow"
   * banner — the code, never the prose (see `errcode.rs` for why).
   */
  readonly errorCode = computed(() => this.errorCopy.codeOf(this._error()));

  readonly meetingId = this._meetingId.asReadonly();
  /** Latest live-transcription caption (best-effort, only during recording). */
  readonly liveCaption = this._liveCaption.asReadonly();

  readonly isRecording = computed(() => this._stage() === "recording");
  readonly isBusy = computed(() =>
    [
      "recording",
      "transcribing",
      "summarizing",
      "exporting",
      "saved",
      "finalized",
    ].includes(this._stage()),
  );

  /**
   * Clear only route-owned, terminal presentation state.
   *
   * The recorder is a root singleton, so `done` / `error` and the last meeting
   * otherwise survive destruction of `/record` and reappear when the route is
   * mounted again. Recording and pipeline stages are backend-owned work and are
   * deliberately left untouched: navigating elsewhere must never stop capture
   * or detach finalization. Returns whether a reset was safe, so the route can
   * clear its matching assistant focus at the same boundary.
   */
  resetRoutePresentation(): boolean {
    if (
      [
        "recording",
        "transcribing",
        "summarizing",
        "exporting",
        "saved",
        "finalized",
      ].includes(this._stage())
    ) {
      return false;
    }
    const terminalMeetingId = this._meetingId();
    if (this._stage() === "done" && terminalMeetingId) {
      this.ignoredTerminalMeetingId = terminalMeetingId;
    }
    this._stage.set("idle");
    ++this.terminalStatusRequest;
    this._message.set("");
    this._lastNote.set(null);
    this._error.set(null);
    this._meetingId.set(null);
    this._liveCaption.set("");
    this._recStartMs = 0;
    return true;
  }

  /**
   * Mic peak level 0.0..=1.0 (PHASE0-PLAN §7 recording_level). Fully reactive:
   * while recording, polls every 100ms; emits 0 otherwise. `toSignal` owns the
   * subscription lifecycle — no manual setInterval/clearInterval, and no leak
   * risk if `stop()` is skipped (review MF-1).
   */
  readonly level = toSignal(
    toObservable(this.isRecording).pipe(
      switchMap((rec) =>
        rec
          ? interval(100).pipe(
              startWith(0),
              switchMap(() =>
                from(this.ipc.recordingLevel()).pipe(catchError(() => of(0))),
              ),
            )
          : of(0),
      ),
    ),
    { initialValue: 0 },
  );

  /**
   * Why system audio is not being captured right now, or `null` while it is fine.
   *
   * Polled every 5 s WHILE RECORDING only — the same sanctioned `toObservable` + `interval` +
   * `toSignal` shape as `level`, so the subscription's lifecycle is the framework's, not ours.
   * Five seconds, not 100 ms: this answers "has the helper died", which is a once-per-recording
   * event, and a tighter poll would spend IPC on a question whose answer almost never changes.
   *
   * The backend derives it from the SAME predicate its 100 ms mic-restore watchdog uses, so the
   * warning cannot disagree with the decision to un-mute.
   */
  readonly systemCaptureNote = toSignal(
    toObservable(this.isRecording).pipe(
      switchMap((rec) =>
        rec
          ? interval(5000).pipe(
              startWith(0),
              switchMap(() =>
                from(this.ipc.recordingStatus()).pipe(
                  map((st) => st?.systemCaptureNote ?? null),
                  catchError(() => of(null)),
                ),
              ),
            )
          : of(null),
      ),
    ),
    { initialValue: null as string | null },
  );

  /** Epoch ms when the current recording started — drives the elapsed timer. */
  private _recStartMs = 0;

  /**
   * Seconds elapsed since recording started; 0 when idle. Bridged from an rxjs
   * interval via toSignal — same sanctioned pattern as `level` (no setInterval).
   */
  readonly elapsed = toSignal(
    toObservable(this.isRecording).pipe(
      switchMap((rec) =>
        rec
          ? interval(250).pipe(
              startWith(0),
              map(() =>
                Math.max(0, Math.floor((Date.now() - this._recStartMs) / 1000)),
              ),
            )
          : of(0),
      ),
    ),
    { initialValue: 0 },
  );

  /**
   * True once ANY `EVENT_STATUS` payload has been observed this webview session.
   * The event stream is the live truth: `reconcileStage()` (the one-shot backend
   * resync on init) must never override a stage the backend itself just pushed.
   */
  private statusSeen = false;

  /** Invalidates an older terminal detail read after any newer status. */
  private terminalStatusRequest = 0;

  /**
   * `/record` is a presentation session, not the owner of capture or pipeline
   * work. A terminal detail read may finish after that route was destroyed, so
   * bind every read to the route epoch that requested it. Busy stages without a
   * terminal hydration remain backend-owned and survive navigation unchanged.
   */
  private recordRouteActive = false;
  private recordRouteEpoch = 0;
  private terminalHydration: {
    meetingId: string;
    request: number;
    routeEpoch: number;
  } | null = null;
  /** Content-free tombstone for command/status settlements retired by route exit. */
  private ignoredTerminalMeetingId: string | null = null;

  private unlisten: UnlistenFn | null = null;
  private unlistenVoice: UnlistenFn | null = null;
  private unlistenToggle: UnlistenFn | null = null;
  private unlistenLive: UnlistenFn | null = null;
  private unlistenEcho: UnlistenFn | null = null;
  private unlistenStoragePruned: UnlistenFn | null = null;
  private unlistenCapped: UnlistenFn | null = null;
  private unlistenCaptureFault: UnlistenFn | null = null;

  constructor() {
    // This root store retains the full NoteDto, including markdown and an
    // exported path. Scrub it at the same synchronous process-wide privacy
    // boundary used by mounted content readers, before any later render can
    // expose a stale vault receipt or keep Re-Truth active.
    const unregister = this.privacyBarrier.registerInvalidator(() =>
      this.invalidateTerminalPrivacy(),
    );
    this.destroyRef.onDestroy(unregister);
  }

  /** Start a fresh `/record` presentation epoch. */
  enterRecordRoute(): void {
    this.recordRouteActive = true;
    ++this.recordRouteEpoch;
    if (this.terminalHydration && this._stage() === "saved") {
      // Finalization already reached its terminal read in an older/absent
      // presentation. Do not let its late response resurrect that result on a
      // new visit. A plain `saved` stage without this marker is still genuine
      // backend processing and remains visible.
      ++this.terminalStatusRequest;
      this.retireTerminalPresentation(this.terminalHydration.meetingId);
    }
  }

  /** End the current `/record` presentation epoch without stopping work. */
  leaveRecordRoute(): void {
    this.recordRouteActive = false;
    ++this.recordRouteEpoch;
    if (this.terminalHydration) {
      // A terminal read means the backend has already finalized this meeting;
      // it is not genuine processing anymore. Retire its route-owned
      // presentation immediately so a stale `finally` cannot erase the marker
      // and leave `saved` wedged before the next visit.
      ++this.terminalStatusRequest;
      this.retireTerminalPresentation(this.terminalHydration.meetingId);
    } else if (this._stage() === "done" && this._meetingId()) {
      this.retireTerminalPresentation(this._meetingId()!);
    }
  }

  async init(): Promise<void> {
    if (this.unlisten) return;
    this.unlisten = await this.ipc.onStatus((p) => this.applyStatus(p));
    // Voice trigger: when the backend hears the wake phrase, start a recording.
    this.unlistenVoice = await this.ipc.onVoiceStart(() => {
      if (!this.isRecording()) void this.start();
    });
    // Tray "Start / Stop recording": toggle from the menu bar without opening a window.
    this.unlistenToggle = await this.ipc.onToggleRecord(() =>
      this.toggleRecord(),
    );
    // Live captions during recording (best-effort; backend emits partial transcripts).
    this.unlistenLive = await this.ipc.onLiveCaption((t) =>
      this._liveCaption.set(t),
    );
    // Echo cleanup notice: recording was made on speakers; echoed lines were removed.
    this.unlistenEcho = await this.ipc.onEchoSuppressed((p) => {
      const s = p.suppressed;
      this.toast.info(
        `Removed ${s} echoed line${s === 1 ? "" : "s"} from the transcript — wear headphones for best results 🎧`,
      );
    });
    // Storage auto-prune notice: old recordings' audio was deleted to stay under the cap.
    // Honest feedback that content (audio only — notes are kept) was removed.
    this.unlistenStoragePruned = await this.ipc.onStoragePruned((p) => {
      this.toast.info(
        `Freed ${humanBytes(p.freedBytes)} — removed ${p.prunedCount} old recording${p.prunedCount === 1 ? "" : "s"} to stay under your storage limit`,
      );
    });
    // 4h TIME-cap notice: the backend capture self-stopped at MAX_RECORDING_SECONDS and everything
    // spoken past it is dropped. Surface the notice + AUTO-FINALIZE via the existing stop() action so
    // the meeting still produces a note (the capped buffer is intact). IDEMPOTENT: only dispatch stop()
    // while we're still in the "recording" stage — a second cap event or a user Stop already in flight
    // has moved the stage past "recording", so this becomes a no-op (no double stop_recording).
    // Automatic Stop passes `false`: it has no user-awaited editor flush witness, so Rust preserves
    // the companion row for any late save.
    this.unlistenCapped = await this.ipc.onRecordingCapped((p) => {
      const hours = Math.round(p.limitSeconds / 3600);
      this.toast.info(
        `Maximum recording length (${hours} h) reached — recording stopped; generating your note…`,
      );
      if (this._stage() === "recording") {
        void this.stop(false);
      }
    });
    // Device/storage/authority faults self-stop capture but retain the exact fsynced prefix. Treat
    // them like the 4h cap: surface one content-free notice and idempotently finalize immediately.
    this.unlistenCaptureFault = await this.ipc.onRecordingCaptureFault(() => {
      this.toast.info(
        "Audio capture stopped after a device or storage problem — preserving what was recorded and generating your note…",
      );
      if (this._stage() === "recording") {
        void this.stop(false);
      }
    });
    await this.reconcileStage();
    await this.refreshLastNote();
  }

  /**
   * One-shot reconcile of the FE stage against the BACKEND's truth on webview
   * (re)load. Two desync shapes this closes:
   *
   * 1. The webview reloaded (tauri-dev hot reload, Cmd-R, webview crash) while
   *    the long-lived Rust process is GENUINELY recording — the fresh store
   *    boots at "idle" while `AppState.recorder` is `Some(..)`, so the next
   *    Start hits `start_recording`'s "already recording" guard. Resync to
   *    "recording" (anchoring the elapsed timer to the persisted `startedAt`).
   * 2. A stale optimistic BUSY stage with the backend idle — e.g. the note
   *    pipeline died before this webview session even loaded (its terminal
   *    "error" event fired into a void). Clear it to "idle" so the surface can
   *    never boot wedged on "Transcribing…".
   *
   * `statusSeen` guards both branches: once any live `EVENT_STATUS` payload has
   * arrived (including a detached pipeline still emitting "summarizing"), the
   * event stream is authoritative and this probe must not fight it. Best-effort:
   * a failed probe never blocks init.
   */
  private async reconcileStage(): Promise<void> {
    try {
      const st = await this.ipc.recordingStatus();
      if (this.statusSeen) return;
      if (st?.recording) {
        this._meetingId.set(st.meetingId);
        const startedMs = st.startedAt ? Date.parse(st.startedAt) : NaN;
        this._recStartMs = Number.isFinite(startedMs) ? startedMs : Date.now();
        this._stage.set("recording");
      } else if (
        [
          "recording",
          "transcribing",
          "summarizing",
          "exporting",
          "saved",
          "finalized",
        ].includes(this._stage())
      ) {
        this._stage.set("idle");
      }
    } catch {
      // Best-effort resync — never block init on the probe.
    }
  }

  private applyStatus(p: StatusPayload): void {
    this.statusSeen = true;
    const wasRecording = this._stage() === "recording";
    if (
      p.stage === "finalized" &&
      p.meetingId &&
      p.meetingId === this.ignoredTerminalMeetingId
    ) {
      return;
    }
    if (
      p.stage === "finalized" &&
      p.meetingId &&
      p.meetingId === this._meetingId() &&
      (this._stage() === "done" ||
        this.terminalHydration?.meetingId === p.meetingId)
    ) {
      // Duplicate terminal events are expected across windows. Once this exact
      // meeting is ready (or already hydrating), never clear its safe card or
      // regress it to the processing stage for a redundant refetch.
      return;
    }
    this._message.set(p.message);
    this._meetingId.set(p.meetingId);
    if (p.stage === "saved" || p.stage === "done") {
      // Both are pre-final progress events. The backend emits `finalized` only
      // after recording ownership and the model session are retired; until
      // then filing must remain unavailable in every WebView.
      this._lastNote.set(null);
      this._stage.set("saved");
      this._liveCaption.set("");
      this._error.set(null);
      ++this.terminalStatusRequest;
      this.terminalHydration = null;
      return;
    }
    if (p.stage === "finalized") {
      // This WebView may never have called stop(). Hydrate only the exact gated
      // meeting before exposing its final result; never reuse get_last_note.
      this._lastNote.set(null);
      this._stage.set("saved");
      this._liveCaption.set("");
      this._error.set(null);
      if (p.meetingId) this.beginTerminalHydration(p.meetingId);
      return;
    }

    ++this.terminalStatusRequest;
    this.terminalHydration = null;
    this._stage.set(p.stage);
    if (p.stage === "recording") {
      this.ignoredTerminalMeetingId = null;
      this._lastNote.set(null);
    }
    if (p.stage !== "recording") {
      this._liveCaption.set("");
    }
    // Anchor the elapsed timer when THIS window first observes recording — covers windows
    // that didn't call start() themselves (the floating bar, or a voice-triggered start),
    // where _recStartMs would otherwise stay 0 and show an epoch-sized timer.
    if (p.stage === "recording" && !wasRecording) {
      this._recStartMs = Date.now();
    }
    if (p.stage === "error") {
      this._error.set(p.message);
    } else {
      this._error.set(null);
    }
  }

  async start(folderId: string | null = null): Promise<void> {
    ++this.terminalStatusRequest;
    this.terminalHydration = null;
    this.ignoredTerminalMeetingId = null;
    this._error.set(null);
    this._liveCaption.set("");
    this._lastNote.set(null);
    try {
      const res = await this.ipc.startRecording(folderId);
      this._meetingId.set(res.meetingId);
      this._recStartMs = Date.now();
      this._stage.set("recording");
    } catch (e) {
      this._error.set(String(e));
      this._stage.set("error");
    }
  }

  async stop(flushCompanion = true): Promise<void> {
    // OPTIMISTIC flip BEFORE the await: `stopRecording` runs the WHOLE pipeline inline (transcribe
    // the entire recording + generate the note), which for a long meeting takes minutes. Without
    // this, `_stage` stays "recording" for that whole time — the Stop button keeps rendering
    // (`isRecording()` true) and stays clickable, so the UI looks frozen and a double-Stop is
    // possible. Moving to "transcribing" now (mirrors `resummarize`'s optimistic set) instantly
    // swaps the recording strip for the processing view; the backend's status events + the resolved
    // StopResult then reconcile the exact stage.
    this._error.set(null);
    this._stage.set("transcribing");
    const stoppingMeetingId = this._meetingId();
    ++this.terminalStatusRequest;
    this.terminalHydration = null;
    try {
      // FLUSH-BEFORE-FINALIZE (root-cause fix, 2026-07-17): the recording panel's
      // "Note" tab hosts the embedded companion note editor, which persists via a
      // DEBOUNCED autosave. `stop_recording` deletes that companion note if it is
      // still empty — so a Stop fired inside the debounce window (type → Stop within
      // ~600ms) would delete the note while the user's prose was still only in the
      // editor, losing it from the note, the vault, AND the summary. AWAIT the live
      // editor's durable flush FIRST so the DB carries the user's text before the
      // delete-if-empty predicate ever runs. A no-op when no companion editor is
      // mounted (e.g. Stop from the floating bar window); never rejects.
      // Only an explicitly completed manual flush authorizes empty-stub deletion.
      // Automatic cap/capture-fault Stop passes `false` without waiting and therefore
      // preserves the stub; a late editor save can never race a backend delete.
      const companionFlushCompleted = flushCompanion
        ? await this.flushService.flush()
        : false;
      const res = await this.ipc.stopRecording(companionFlushCompleted);
      if (res.meetingId === this.ignoredTerminalMeetingId) {
        return;
      }
      if (
        !stoppingMeetingId ||
        res.meetingId !== stoppingMeetingId ||
        this._meetingId() !== stoppingMeetingId
      ) {
        return;
      }
      if (this._meetingId() === res.meetingId && this._stage() === "done") {
        return;
      }
      this._meetingId.set(res.meetingId);
      this._lastNote.set(null);
      this._stage.set("saved");
      await this.beginTerminalHydration(res.meetingId);
    } catch (e) {
      if (
        !stoppingMeetingId ||
        stoppingMeetingId === this.ignoredTerminalMeetingId ||
        this._meetingId() !== stoppingMeetingId
      ) {
        return;
      }
      ++this.terminalStatusRequest;
      this._error.set(String(e));
      this._stage.set("error");
    }
  }

  /**
   * Re-run summarization + export for an already-recorded meeting. Used to retry
   * after the user grants cloud-egress consent (the first attempt failed with
   * "cloud egress not consented"). Mirrors stop()'s optimistic-then-reconcile flow.
   */
  async resummarize(meetingId: string): Promise<void> {
    ++this.terminalStatusRequest;
    this.terminalHydration = null;
    this.ignoredTerminalMeetingId = null;
    this._error.set(null);
    this._lastNote.set(null);
    this._stage.set("summarizing");
    try {
      const res = await this.ipc.resummarize(meetingId);
      if (res.meetingId === this.ignoredTerminalMeetingId) {
        return;
      }
      if (res.meetingId !== meetingId || this._meetingId() !== meetingId) {
        return;
      }
      if (this._meetingId() === res.meetingId && this._stage() === "done") {
        return;
      }
      this._meetingId.set(res.meetingId);
      this._stage.set("saved");
      await this.beginTerminalHydration(res.meetingId);
    } catch (e) {
      if (
        meetingId === this.ignoredTerminalMeetingId ||
        this._meetingId() !== meetingId
      ) {
        return;
      }
      ++this.terminalStatusRequest;
      this._error.set(String(e));
      this._stage.set("error");
    }
  }

  /** Tray toggle: stop if recording, else start (ignored while a recording is processing). */
  toggleRecord(): void {
    const s = this._stage();
    if (s === "recording") {
      void this.stop();
    } else if (s === "idle" || s === "done" || s === "error") {
      void this.start();
    }
  }

  async refreshLastNote(): Promise<void> {
    const request = this.terminalStatusRequest;
    const meetingId = this._meetingId();
    const stage = this._stage();
    try {
      const privacyReady = await this.privacyBarrier.ensureReady();
      if (
        !privacyReady ||
        request !== this.terminalStatusRequest ||
        meetingId !== this._meetingId() ||
        stage !== this._stage()
      ) {
        return;
      }
      const note = await this.ipc.getLastNote();
      if (
        request !== this.terminalStatusRequest ||
        meetingId !== this._meetingId() ||
        stage !== this._stage()
      ) {
        return;
      }
      this._lastNote.set(note);
    } catch (e) {
      if (
        request !== this.terminalStatusRequest ||
        meetingId !== this._meetingId() ||
        stage !== this._stage()
      ) {
        return;
      }
      this._error.set(String(e));
    }
  }

  private beginTerminalHydration(meetingId: string): Promise<void> {
    const request = ++this.terminalStatusRequest;
    const routeEpoch = this.recordRouteEpoch;
    this.terminalHydration = { meetingId, request, routeEpoch };
    return this.reconcileTerminalMeeting(meetingId, request, routeEpoch);
  }

  /**
   * Resolve a terminal event/command against the exact gated meeting. The
   * request + meeting guards prevent a late response from restoring another
   * meeting's note after a newer recording or privacy transition.
   */
  private async reconcileTerminalMeeting(
    meetingId: string,
    request: number,
    routeEpoch: number,
  ): Promise<void> {
    try {
      const privacyReady = await this.privacyBarrier.ensureReady();
      if (!this.terminalRequestIsCurrent(meetingId, request, routeEpoch)) {
        return;
      }
      if (!privacyReady) {
        this.settleTerminalWithoutContent(meetingId, request, routeEpoch);
        return;
      }
      const detail = await this.ipc.getMeetingDetail(meetingId);
      if (!this.terminalRequestIsCurrent(meetingId, request, routeEpoch)) {
        return;
      }
      if (!detail || detail.meeting.id !== meetingId) {
        this.settleTerminalWithoutContent(meetingId, request, routeEpoch);
        return;
      }
      if (
        detail.meeting.status !== "SUMMARIZED" &&
        detail.meeting.status !== "EXPORTED"
      ) {
        this.settleTerminalWithoutContent(meetingId, request, routeEpoch);
        return;
      }
      if (detail.note && detail.note.meetingId !== meetingId) {
        this.settleTerminalWithoutContent(meetingId, request, routeEpoch);
        return;
      }
      if (!this.recordRouteActive) {
        this.retireTerminalPresentation(meetingId);
        return;
      }
      this._lastNote.set(detail.locked ? null : detail.note);
      this._stage.set("done");
    } catch {
      // The backend has already declared this exact meeting finalized. A failed
      // gated read must not wedge the UI on "Saved" forever: expose only the
      // content-free exact navigation card. Its placement reader has an
      // explicit retry; never fall back to process-global get_last_note.
      this.settleTerminalWithoutContent(meetingId, request, routeEpoch);
    } finally {
      if (this.terminalHydration?.request === request) {
        this.terminalHydration = null;
      }
    }
  }

  private terminalRequestIsCurrent(
    meetingId: string,
    request: number,
    routeEpoch: number,
  ): boolean {
    return (
      request === this.terminalStatusRequest &&
      meetingId === this._meetingId() &&
      routeEpoch === this.recordRouteEpoch &&
      this.terminalHydration?.request === request
    );
  }

  private settleTerminalWithoutContent(
    meetingId: string,
    request: number,
    routeEpoch: number,
  ): void {
    if (!this.terminalRequestIsCurrent(meetingId, request, routeEpoch)) return;
    this._lastNote.set(null);
    this._error.set(null);
    if (this.recordRouteActive) {
      this._stage.set("done");
    } else {
      this.retireTerminalPresentation(meetingId);
    }
  }

  /** Synchronous content scrub for lock/delete/privacy transitions. */
  private invalidateTerminalPrivacy(): void {
    ++this.terminalStatusRequest;
    this._lastNote.set(null);
    const pending = this.terminalHydration;
    if (pending && this._stage() === "saved") {
      if (this.recordRouteActive) {
        this.terminalHydration = null;
        this._stage.set("done");
      } else {
        this.retireTerminalPresentation(pending.meetingId);
      }
    }
  }

  private retireTerminalPresentation(meetingId: string): void {
    this.ignoredTerminalMeetingId = meetingId;
    this.terminalHydration = null;
    this.clearTerminalPresentationToIdle();
  }

  private clearTerminalPresentationToIdle(): void {
    this._stage.set("idle");
    this._message.set("");
    this._lastNote.set(null);
    this._error.set(null);
    this._meetingId.set(null);
    this._liveCaption.set("");
    this._recStartMs = 0;
  }
}
