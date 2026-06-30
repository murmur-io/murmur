import { Injectable, computed, inject, signal } from "@angular/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "./ipc.service";
import type {
  VoiceActionResultPayload,
  VoiceActionStatus,
  VoiceCommandListeningPayload,
  VoiceCommandProcessingPayload,
  AssistantToolPayload,
  WakeDetectedPayload,
} from "./models";

/**
 * The 4-state visual model of the assistant orb (industry-convergent
 * idle → listening → processing → answer). Pure presentation: derived from the
 * store's listening/processing/in-flight signals + the newest interaction
 * status by {@link AssistantStore.orbState}, never set directly.
 */
export type OrbState = "idle" | "listening" | "processing" | "answer";

/**
 * Phase H — one recent in-meeting voice-assistant interaction. A wake creates a
 * `pending` row ("heard: {command}"); the matching result resolves it with a
 * summary + citation chips + a status pill. Newest-first.
 */
/**
 * One parsed grounding citation. The backend sends a flat `string[]` mixing two
 * shapes (`voice_action.rs`): a VAULT meeting wikilink `[[Title]]`, and a WEB hit
 * `(web) Title — https://…` (the loud "via web" attribution). We parse each into
 * this discriminated shape so the card renders vault chips and "via web" links
 * distinctly — a web source is visibly off-device, never a `[[vault]]` chip.
 */
export interface AssistantCitation {
  kind: "vault" | "web";
  /** Display label: the bare title (brackets stripped for vault). */
  label: string;
  /** The destination URL for a web source (absent for vault). */
  url?: string;
}

/**
 * One tool the brain used during an agentic turn — drives the LIVE trace chips
 * ("Searching notes… ✓", "Checking the web…"). Carries the tool name + a coarse
 * count only (no PII). Pushed/updated by {@link AssistantStore.onAssistantTool}.
 */
export interface ToolTraceStep {
  /** Stable id for `@for` tracking (never key a trace chip on $index). */
  id: number;
  /** The tool name (search_meetings / search_semantic / web_search / …). */
  tool: string;
  /** "running" while the call is in flight, "done" once it returns. */
  state: "running" | "done";
  /** False when the call errored (the chip shows a muted state). */
  ok: boolean;
  /** Coarse result-size badge ("✓ N") — never the content. */
  count: number | null;
}

export interface AssistantInteraction {
  /** Stable id for `@for` tracking (we never key on $index). */
  id: number;
  /** Where the question came from — drives the row icon (mic / keyboard / wake). */
  source: "voice" | "wake" | "text";
  /** What the user said (voice) or typed (text). */
  command: string;
  /** "pending" until the result arrives, then mirrors the result status. */
  status: "pending" | VoiceActionStatus;
  /** The assistant's answer (empty while pending). */
  summary: string;
  /** The live tool-trace for THIS turn (the chips), in call order. */
  trace: ToolTraceStep[];
  /** Parsed grounding citations → vault `[[Title]]` chips + "via web" links. */
  citations: AssistantCitation[];
}

/**
 * Parse the backend's flat citation strings into typed vault/web citations.
 * A web hit is `(web) Title — https://…` (or `(web) Title` with no URL); anything
 * else is a vault meeting, whose `[[…]]` brackets we strip for display.
 */
export function parseCitations(raw: string[]): AssistantCitation[] {
  return raw.map((c) => {
    const s = c.trim();
    if (s.startsWith("(web)")) {
      const body = s.slice("(web)".length).trim();
      // Split a trailing " — https://…" off the title; the URL is the last
      // " — "-separated chunk when it looks like a link.
      const sep = body.lastIndexOf(" — ");
      if (sep !== -1) {
        const tail = body.slice(sep + 3).trim();
        if (/^https?:\/\//i.test(tail)) {
          return { kind: "web", label: body.slice(0, sep).trim(), url: tail };
        }
      }
      return { kind: "web", label: body };
    }
    // Vault wikilink — strip surrounding [[ ]] for the chip label.
    const label = s.replace(/^\[\[/, "").replace(/\]\]$/, "");
    return { kind: "vault", label };
  });
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

  /**
   * True while a dispatched command is being processed (between the listener
   * stopping and the answer landing) — the EVENT_VOICE_COMMAND_PROCESSING
   * `{active}` boolean. Drives the orb's PROCESSING state + the "🧠 Przetwarzam…"
   * shimmer label. Cleared by the result (or by the end-ask error path).
   */
  private readonly _processing = signal(false);
  readonly processing = this._processing.asReadonly();

  /**
   * The 4-state orb model collapsed from the existing signals — a PURE
   * `computed` (no signal writes → no NG0600 / trap T1):
   *   processing → "processing" (highest priority: a dispatch is in flight)
   *   listening  → "listening"  (the mic is open)
   *   manual ask in flight (begin clicked, listener not yet open) → "listening"
   *   newest interaction resolved → "answer" (a result is on screen)
   *   otherwise → "idle".
   * Bound on the orb as `[state]="orbState()"`.
   */
  readonly orbState = computed<OrbState>(() => {
    if (this._processing()) return "processing";
    if (this._listening() || this._manualAskInFlight()) return "listening";
    const top = this._interactions()[0];
    if (top && top.status !== "pending") return "answer";
    return "idle";
  });

  /** Monotonic id source for interaction rows (stable `@for` keys). */
  private nextId = 1;
  /** Monotonic id source for tool-trace chips (stable `@for` keys). */
  private nextTraceId = 1;

  private unlistenWake: UnlistenFn | null = null;
  private unlistenResult: UnlistenFn | null = null;
  private unlistenListening: UnlistenFn | null = null;
  private unlistenProcessing: UnlistenFn | null = null;
  private unlistenTool: UnlistenFn | null = null;
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
    this.unlistenProcessing = await this.ipc.onVoiceCommandProcessing((p) =>
      this.onProcessing(p),
    );
    this.unlistenTool = await this.ipc.onAssistantTool((p) =>
      this.onAssistantTool(p),
    );
  }

  /** Release the event subscriptions (e.g. on app teardown). */
  dispose(): void {
    this.unlistenWake?.();
    this.unlistenResult?.();
    this.unlistenListening?.();
    this.unlistenProcessing?.();
    this.unlistenTool?.();
    this.unlistenWake = null;
    this.unlistenResult = null;
    this.unlistenListening = null;
    this.unlistenProcessing = null;
    this.unlistenTool = null;
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

  /**
   * CLICK-TO-STOP: stop the open listener so the FULL accumulated utterance is
   * dispatched. Optimistically flip `listening` off + `processing` on so the orb
   * morphs to PROCESSING the instant the user clicks (the backend's
   * `{active:false}` listening + `{active:true}` processing events reconcile it
   * shortly after); the answer clears processing via {@link onResult}. The manual
   * ask stays in flight so the card keeps the answer's home. A no-op backend
   * (nothing armed) is fine — `endVoiceCommand` is a graceful no-op there.
   */
  async endAsk(): Promise<void> {
    this._listening.set(false);
    this._processing.set(true);
    try {
      await this.ipc.endVoiceCommand();
    } catch (e) {
      // The stop call itself failed — don't leave the orb stuck "processing".
      this._processing.set(false);
      this._manualAskInFlight.set(false);
      throw e;
    }
  }

  /**
   * Ask the assistant a TYPED question (the text composer). Optimistically prepend
   * a pending row with the typed text BEFORE the await (the {@link onWake} pattern)
   * so the user sees it instantly, mark a turn in flight (orb → processing), then
   * route through the SAME gated brain via IPC. The live tool-trace lands via
   * {@link onAssistantTool}; the answer resolves the pending row via {@link onResult}.
   */
  async askText(text: string): Promise<void> {
    const command = text.trim();
    if (!command) return;
    const row: AssistantInteraction = {
      id: this.nextId++,
      source: "text",
      command,
      status: "pending",
      summary: "",
      trace: [],
      citations: [],
    };
    this._interactions.update((rows) =>
      [row, ...rows].slice(0, AssistantStore.MAX),
    );
    this._processing.set(true);
    try {
      await this.ipc.askAssistantText(command);
    } catch (e) {
      this._processing.set(false);
      // Don't leave the optimistic row stuck "pending" if the dispatch was rejected.
      this._interactions.update((rows) =>
        rows.map((r) =>
          r.id === row.id
            ? { ...r, status: "error" as const, summary: "Couldn't send your question." }
            : r,
        ),
      );
      throw e;
    }
  }

  private onWake(p: WakeDetectedPayload): void {
    const row: AssistantInteraction = {
      id: this.nextId++,
      source: "wake",
      command: p.command,
      status: "pending",
      summary: "",
      trace: [],
      citations: [],
    };
    this._interactions.update((rows) =>
      [row, ...rows].slice(0, AssistantStore.MAX),
    );
  }

  /**
   * Attach a LIVE tool-trace chip to the in-flight (top pending) interaction. A
   * "running" event pushes a new chip; a "done" event resolves the most recent
   * matching running chip (or appends one if none). No pending row → ignore (the
   * trace has no home). Pure immutable signal updates — no NG0600.
   */
  private onAssistantTool(p: AssistantToolPayload): void {
    this._interactions.update((rows) => {
      const idx = rows.findIndex((r) => r.status === "pending");
      if (idx === -1) return rows;
      const row = rows[idx];
      const trace = row.trace.slice();
      if (p.state === "running") {
        trace.push({
          id: this.nextTraceId++,
          tool: p.tool,
          state: "running",
          ok: true,
          count: p.count,
        });
      } else {
        let resolved = false;
        for (let i = trace.length - 1; i >= 0; i--) {
          if (trace[i].tool === p.tool && trace[i].state === "running") {
            trace[i] = { ...trace[i], state: "done", ok: p.ok, count: p.count };
            resolved = true;
            break;
          }
        }
        if (!resolved) {
          trace.push({
            id: this.nextTraceId++,
            tool: p.tool,
            state: "done",
            ok: p.ok,
            count: p.count,
          });
        }
      }
      const next = rows.slice();
      next[idx] = { ...row, trace };
      return next;
    });
  }

  private onListening(p: VoiceCommandListeningPayload): void {
    this._listening.set(p.active);
  }

  private onProcessing(p: VoiceCommandProcessingPayload): void {
    this._processing.set(p.active);
    // The backend stops the listener implicitly when it begins dispatching.
    if (p.active) this._listening.set(false);
  }

  private onResult(p: VoiceActionResultPayload): void {
    // The answer landed — the manual ask (if any) is no longer in flight, the
    // listener is closed, and the dispatch is no longer processing.
    this._manualAskInFlight.set(false);
    this._listening.set(false);
    this._processing.set(false);
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
          source: "voice",
          command: p.command,
          status: p.status,
          summary: p.summary,
          trace: [],
          citations: parseCitations(p.citations),
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
        citations: parseCitations(p.citations),
      };
      return next;
    });
  }
}
