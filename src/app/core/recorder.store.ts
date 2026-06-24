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

  readonly stage = this._stage.asReadonly();
  readonly message = this._message.asReadonly();
  readonly lastNote = this._lastNote.asReadonly();
  readonly error = this._error.asReadonly();
  readonly meetingId = this._meetingId.asReadonly();

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

  async init(): Promise<void> {
    if (this.unlisten) return;
    this.unlisten = await this.ipc.onStatus((p) => this.applyStatus(p));
    await this.refreshLastNote();
  }

  private applyStatus(p: StatusPayload): void {
    this._stage.set(p.stage);
    this._message.set(p.message);
    this._meetingId.set(p.meetingId);
    if (p.stage === "error") {
      this._error.set(p.message);
    } else {
      this._error.set(null);
    }
  }

  async start(): Promise<void> {
    this._error.set(null);
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

  async refreshLastNote(): Promise<void> {
    try {
      const note = await this.ipc.getLastNote();
      this._lastNote.set(note);
    } catch (e) {
      this._error.set(String(e));
    }
  }
}
