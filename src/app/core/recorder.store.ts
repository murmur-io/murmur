import { Injectable, computed, inject, signal } from "@angular/core";
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
    ["recording", "transcribing", "summarizing", "exporting"].includes(
      this._stage(),
    ),
  );

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

  private unlisten: UnlistenFn | null = null;
  private unlistenVoice: UnlistenFn | null = null;
  private unlistenToggle: UnlistenFn | null = null;
  private unlistenLive: UnlistenFn | null = null;
  private unlistenEcho: UnlistenFn | null = null;
  private unlistenStoragePruned: UnlistenFn | null = null;
  private unlistenCapped: UnlistenFn | null = null;
  private unlistenCaptureFault: UnlistenFn | null = null;

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
        ["recording", "transcribing", "summarizing", "exporting"].includes(
          this._stage(),
        )
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
    this._stage.set(p.stage);
    this._message.set(p.message);
    this._meetingId.set(p.meetingId);
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

  async start(): Promise<void> {
    this._error.set(null);
    this._liveCaption.set("");
    try {
      const res = await this.ipc.startRecording();
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
      // Optimistic preview from the StopResult; then reconcile with the
      // persisted note so the pane shows the canonical provider id / path.
      this._lastNote.set({
        meetingId: res.meetingId,
        providerId: "",
        markdown: res.markdown,
        exportedPath: res.exportedPath,
      });
      this._stage.set("done");
      await this.refreshLastNote();
    } catch (e) {
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
    this._error.set(null);
    this._stage.set("summarizing");
    try {
      const res = await this.ipc.resummarize(meetingId);
      this._lastNote.set({
        meetingId: res.meetingId,
        providerId: "",
        markdown: res.markdown,
        exportedPath: res.exportedPath,
      });
      this._stage.set("done");
      await this.refreshLastNote();
    } catch (e) {
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
    try {
      const note = await this.ipc.getLastNote();
      this._lastNote.set(note);
    } catch (e) {
      this._error.set(String(e));
    }
  }
}
