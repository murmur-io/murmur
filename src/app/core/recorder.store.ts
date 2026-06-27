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

/**
 * Signal-based recorder state: status stage, last note, and last error.
 * Subscribes to the EVENT_STATUS stream from the Rust core.
 */
@Injectable({ providedIn: "root" })
export class RecorderStore {
  private readonly ipc = inject(IpcService);

  private readonly _stage = signal<Stage>("idle");
  private readonly _message = signal<string>("");
  private readonly _lastNote = signal<NoteDto | null>(null);
  private readonly _error = signal<string | null>(null);
  private readonly _meetingId = signal<string | null>(null);
  private readonly _liveCaption = signal<string>("");

  readonly stage = this._stage.asReadonly();
  readonly message = this._message.asReadonly();
  readonly lastNote = this._lastNote.asReadonly();
  readonly error = this._error.asReadonly();
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

  private unlisten: UnlistenFn | null = null;
  private unlistenVoice: UnlistenFn | null = null;
  private unlistenToggle: UnlistenFn | null = null;
  private unlistenLive: UnlistenFn | null = null;

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
    await this.refreshLastNote();
  }

  private applyStatus(p: StatusPayload): void {
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

  async stop(): Promise<void> {
    try {
      const res = await this.ipc.stopRecording();
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
