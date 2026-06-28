import { Injectable, computed, inject, signal } from "@angular/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "./ipc.service";
import type {
  VoiceActionResultPayload,
  VoiceActionStatus,
  VoiceCommandListeningPayload,
  WakeDetectedPayload,
} from "./models";

/**
 * Phase H — one recent in-meeting voice-assistant interaction. A wake creates a
 * `pending` row ("heard: {command}"); the matching result resolves it with a
 * summary + citation chips + a status pill. Newest-first.
 */
export interface AssistantInteraction {
  /** Stable id for `@for` tracking (we never key on $index). */
  id: number;
  /** What the user said after the wake phrase. */
  command: string;
  /** "pending" until the result arrives, then mirrors the result status. */
  status: "pending" | VoiceActionStatus;
  /** The assistant's answer (empty while pending). */
  summary: string;
  /** Grounding citations → rendered as [[Title]] chips. */
  citations: string[];
}

/**
 * Signal-based store for the in-meeting voice assistant. Subscribes ONCE (the
 * RecorderStore.init() pattern) to the wake + result event streams and keeps a
 * capped, newest-first list of interactions. No NgRx, no subscribe-into-a-field:
 * the event payloads land in a `signal`.
 */
@Injectable({ providedIn: "root" })
export class AssistantStore {
  private readonly ipc = inject(IpcService);

  /** Most recent N interactions kept; older ones drop off (no unbounded growth). */
  private static readonly MAX = 12;

  private readonly _interactions = signal<AssistantInteraction[]>([]);
  /** Newest-first list of recent assistant interactions. */
  readonly interactions = this._interactions.asReadonly();

  /** Whether any interaction has ever been observed (drives empty-state copy). */
  readonly hasAny = computed(() => this._interactions().length > 0);

  /**
   * True while the manual "Ask AI" listener has the mic open (between the
   * `{active:true}` and `{active:false}` EVENT_VOICE_COMMAND_LISTENING events).
   * Drives the pulsing button + "🎙 Słucham…" inline indicator.
   */
  private readonly _listening = signal(false);
  readonly listening = this._listening.asReadonly();

  /**
   * True from the instant a manual ask is fired until its answer lands — keeps
   * the assistant-actions card visible across the whole round-trip even when the
   * realtime-reactions config toggle is off. Set by {@link askStarted}, cleared
   * when a result resolves a pending row.
   */
  private readonly _manualAskInFlight = signal(false);
  readonly manualAskInFlight = this._manualAskInFlight.asReadonly();

  /** Monotonic id source for interaction rows (stable `@for` keys). */
  private nextId = 1;

  private unlistenWake: UnlistenFn | null = null;
  private unlistenResult: UnlistenFn | null = null;
  private unlistenListening: UnlistenFn | null = null;
  /** Synchronous re-entrancy guard so two concurrent init() calls (e.g. the record
   * screen + the card both initialising) can't double-subscribe before the first
   * `await` resolves. */
  private initializing = false;

  /** Subscribe once to the wake + result streams. Idempotent + concurrency-safe. */
  async init(): Promise<void> {
    if (this.unlistenWake || this.initializing) return;
    this.initializing = true;
    this.unlistenWake = await this.ipc.onWakeDetected((p) => this.onWake(p));
    this.unlistenResult = await this.ipc.onVoiceActionResult((p) =>
      this.onResult(p),
    );
    this.unlistenListening = await this.ipc.onVoiceCommandListening((p) =>
      this.onListening(p),
    );
  }

  /** Release the event subscriptions (e.g. on app teardown). */
  dispose(): void {
    this.unlistenWake?.();
    this.unlistenResult?.();
    this.unlistenListening?.();
    this.unlistenWake = null;
    this.unlistenResult = null;
    this.unlistenListening = null;
  }

  /**
   * Fire the manual "Ask AI" trigger: open the listener AND mark a manual ask in
   * flight so the assistant card stays visible for the whole round-trip. Errors
   * (backend rejects / listener unavailable) clear the in-flight flag so the UI
   * doesn't get stuck pulsing.
   */
  async askNow(): Promise<void> {
    this._manualAskInFlight.set(true);
    try {
      await this.ipc.beginVoiceCommand();
    } catch (e) {
      this._manualAskInFlight.set(false);
      this._listening.set(false);
      throw e;
    }
  }

  private onWake(p: WakeDetectedPayload): void {
    const row: AssistantInteraction = {
      id: this.nextId++,
      command: p.command,
      status: "pending",
      summary: "",
      citations: [],
    };
    this._interactions.update((rows) =>
      [row, ...rows].slice(0, AssistantStore.MAX),
    );
  }

  private onListening(p: VoiceCommandListeningPayload): void {
    this._listening.set(p.active);
  }

  private onResult(p: VoiceActionResultPayload): void {
    // The answer landed — the manual ask (if any) is no longer in flight, and
    // the listener is closed.
    this._manualAskInFlight.set(false);
    this._listening.set(false);
    this._interactions.update((rows) => {
      // Resolve the most recent still-pending row; if none (a result without a
      // wake we observed), prepend a fresh resolved row so nothing is lost.
      const idx = rows.findIndex((r) => r.status === "pending");
      if (idx === -1) {
        // A manual ("Ask AI") result has NO preceding wake row — surface the
        // HEARD command straight from the payload so the card shows what the
        // user actually said (not an empty "usłyszano: …").
        const row: AssistantInteraction = {
          id: this.nextId++,
          command: p.command,
          status: p.status,
          summary: p.summary,
          citations: p.citations,
        };
        return [row, ...rows].slice(0, AssistantStore.MAX);
      }
      const next = rows.slice();
      next[idx] = {
        ...next[idx],
        // Keep the wake-detected command, but fall back to the payload's heard
        // command if the pending row never captured one.
        command: next[idx].command || p.command,
        status: p.status,
        summary: p.summary,
        citations: p.citations,
      };
      return next;
    });
  }
}
