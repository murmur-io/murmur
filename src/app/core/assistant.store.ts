import { Injectable, computed, inject, signal } from "@angular/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "./ipc.service";
import type {
  VoiceActionResultPayload,
  VoiceActionStatus,
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

  /** Monotonic id source for interaction rows (stable `@for` keys). */
  private nextId = 1;

  private unlistenWake: UnlistenFn | null = null;
  private unlistenResult: UnlistenFn | null = null;
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
  }

  /** Release the event subscriptions (e.g. on app teardown). */
  dispose(): void {
    this.unlistenWake?.();
    this.unlistenResult?.();
    this.unlistenWake = null;
    this.unlistenResult = null;
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

  private onResult(p: VoiceActionResultPayload): void {
    this._interactions.update((rows) => {
      // Resolve the most recent still-pending row; if none (a result without a
      // wake we observed), prepend a fresh resolved row so nothing is lost.
      const idx = rows.findIndex((r) => r.status === "pending");
      if (idx === -1) {
        const row: AssistantInteraction = {
          id: this.nextId++,
          command: "",
          status: p.status,
          summary: p.summary,
          citations: p.citations,
        };
        return [row, ...rows].slice(0, AssistantStore.MAX);
      }
      const next = rows.slice();
      next[idx] = {
        ...next[idx],
        status: p.status,
        summary: p.summary,
        citations: p.citations,
      };
      return next;
    });
  }
}
