import { Injectable, computed, inject, signal } from "@angular/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "./ipc.service";
import type {
  AssistantToolPayload,
  ChatMsg,
  VoiceActionResultPayload,
  VoiceActionStatus,
  VoiceCommandListeningPayload,
  VoiceCommandProcessingPayload,
  WakeDetectedPayload,
} from "./models";

/**
 * The 4-state visual model of the assistant orb (industry-convergent
 * idle → listening → processing → answer). Pure presentation: derived from the
 * store's listening/processing/in-flight signals + the newest assistant bubble
 * status by {@link AssistantStore.orbState}, never set directly.
 */
export type OrbState = "idle" | "listening" | "processing" | "answer";

/**
 * One parsed grounding citation. The backend sends a flat `string[]` mixing two
 * shapes (`voice_action.rs`): a VAULT meeting wikilink `[[Title]]`, and a WEB hit
 * `(web) Title — https://…` (the loud "via web" attribution). We parse each into
 * this discriminated shape so the surface renders vault chips and "via web" links
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
 * count only (no PII). Pushed/updated by {@link AssistantStore.onTool}.
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

/**
 * One message in the unified in-meeting assistant thread. Voice and text turns
 * both land here as a `user` bubble (the heard/typed command) paired with an
 * `assistant` bubble (resolved from "pending"), so speech and text share ONE
 * chronological conversation. A user message carries the question; an assistant
 * message carries the brain's answer, its live tool-trace, and its citations.
 */
export interface ChatMessage {
  /** Stable id for `@for` tracking (never key on $index). */
  id: number;
  role: "user" | "assistant";
  /** The question (user) or the answer markdown (assistant; empty while pending). */
  text: string;
  /** Assistant only: "pending" while in flight, then the result status. */
  status: "pending" | VoiceActionStatus;
  /** Assistant only: the live tool-trace chips for this turn. */
  trace: ToolTraceStep[];
  /** Assistant only: grounding citations (vault `[[Title]]` + "via web"). */
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
 * The SINGLE in-meeting assistant store: one chronological conversation thread
 * fed by BOTH voice and text, plus the voice input-state (orb / listening /
 * processing). Subscribes ONCE (the RecorderStore.init() pattern) to the wake +
 * result + listening + processing + BOTH tool-trace streams and lands every
 * payload in a `signal` — no NgRx, no subscribe-into-a-field.
 *
 * Memory: a TEXT `send` ships the FULL clean history to `ask_assistant_chat`
 * (multi-turn memory); a VOICE turn answers one-shot via the voice backend and
 * is appended to the SAME thread, so a typed follow-up remembers what was said.
 */
@Injectable({ providedIn: "root" })
export class AssistantStore {
  private readonly ipc = inject(IpcService);

  private readonly _messages = signal<ChatMessage[]>([]);
  /** The unified conversation, oldest → newest. */
  readonly messages = this._messages.asReadonly();
  /** Whether any turn exists yet (drives the empty-state copy). */
  readonly hasMessages = computed(() => this._messages().length > 0);

  /**
   * True while the manual "Ask AI" listener has the mic open (between the
   * `{active:true}` and `{active:false}` EVENT_VOICE_COMMAND_LISTENING events).
   * Drives the pulsing mic button + "🎙 Słucham…" inline indicator.
   */
  private readonly _listening = signal(false);
  readonly listening = this._listening.asReadonly();

  /**
   * True from the instant a manual ask is fired until its answer lands — keeps
   * the assistant surface visible across the whole round-trip even when the
   * realtime-reactions config toggle is off. Set by {@link askNow}, cleared when
   * a result resolves a pending bubble.
   */
  private readonly _manualAskInFlight = signal(false);
  readonly manualAskInFlight = this._manualAskInFlight.asReadonly();

  /**
   * True while a dispatched command (voice OR text) is being processed — the
   * gap between the listener stopping / the text turn dispatching and the answer
   * landing. Drives the orb's PROCESSING state, the composer disable, and the
   * "🧠 Przetwarzam…" shimmer label. Cleared by the result (voice) or the
   * send() finally (text), or by the end-ask error path.
   */
  private readonly _processing = signal(false);
  readonly processing = this._processing.asReadonly();

  /**
   * The 4-state orb model collapsed from the existing signals — a PURE
   * `computed` (no signal writes → no NG0600 / trap T1):
   *   processing → "processing" (highest priority: a dispatch is in flight)
   *   listening  → "listening"  (the mic is open)
   *   manual ask in flight (begin clicked, listener not yet open) → "listening"
   *   newest assistant bubble resolved → "answer" (a result is on screen)
   *   otherwise → "idle".
   */
  readonly orbState = computed<OrbState>(() => {
    if (this._processing()) return "processing";
    if (this._listening() || this._manualAskInFlight()) return "listening";
    const msgs = this._messages();
    for (let i = msgs.length - 1; i >= 0; i--) {
      if (msgs[i].role === "assistant") {
        return msgs[i].status !== "pending" ? "answer" : "idle";
      }
    }
    return "idle";
  });

  /** Monotonic id source for message bubbles (stable `@for` keys). */
  private nextId = 1;
  /** Monotonic id source for tool-trace chips (stable `@for` keys). */
  private nextTraceId = 1;

  private unlistenWake: UnlistenFn | null = null;
  private unlistenResult: UnlistenFn | null = null;
  private unlistenListening: UnlistenFn | null = null;
  private unlistenProcessing: UnlistenFn | null = null;
  private unlistenTool: UnlistenFn | null = null;
  private unlistenChatTool: UnlistenFn | null = null;
  /** Synchronous re-entrancy guard so two concurrent init() calls (e.g. the record
   * screen + the surface both initialising) can't double-subscribe before the first
   * `await` resolves. */
  private initializing = false;

  /** Subscribe once to the wake/result/listening/processing + both tool streams. */
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
    // Both tool-trace streams feed the ONE thread: EVENT_ASSISTANT_TOOL from the
    // voice path, EVENT_CHAT_TOOL from ask_assistant_chat (text). Each chip lands
    // on the last pending assistant bubble (no backend change).
    this.unlistenTool = await this.ipc.onAssistantTool((p) => this.onTool(p));
    this.unlistenChatTool = await this.ipc.onChatTool((p) => this.onTool(p));
  }

  /** Release the event subscriptions (e.g. on app teardown). */
  dispose(): void {
    this.unlistenWake?.();
    this.unlistenResult?.();
    this.unlistenListening?.();
    this.unlistenProcessing?.();
    this.unlistenTool?.();
    this.unlistenChatTool?.();
    this.unlistenWake = null;
    this.unlistenResult = null;
    this.unlistenListening = null;
    this.unlistenProcessing = null;
    this.unlistenTool = null;
    this.unlistenChatTool = null;
  }

  /** Empty the conversation (called on each new recording). */
  clear(): void {
    this._messages.set([]);
  }

  /**
   * Fire the manual "Ask AI" trigger: open the listener AND mark a manual ask in
   * flight so the surface stays visible for the whole round-trip. Errors clear
   * the in-flight flag so the UI doesn't get stuck pulsing.
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
   * morphs to PROCESSING the instant the user clicks; the answer clears processing
   * via {@link onResult}. A no-op backend (nothing armed) is fine.
   */
  async endAsk(): Promise<void> {
    this._listening.set(false);
    this._processing.set(true);
    try {
      await this.ipc.endVoiceCommand();
    } catch (e) {
      this._processing.set(false);
      this._manualAskInFlight.set(false);
      throw e;
    }
  }

  /**
   * Send a TEXT message into the unified thread: optimistically append the user
   * bubble + a pending assistant bubble, ship the FULL clean conversation to the
   * multi-turn brain (`ask_assistant_chat` → memory), then resolve the assistant
   * bubble with the reply. The live tool-trace lands via {@link onTool}. A no-op
   * while another turn is in flight.
   */
  async send(text: string): Promise<void> {
    const t = text.trim();
    if (!t || this._processing()) return;
    const userMsg: ChatMessage = {
      id: this.nextId++,
      role: "user",
      text: t,
      status: "ok",
      trace: [],
      citations: [],
    };
    const botMsg: ChatMessage = {
      id: this.nextId++,
      role: "assistant",
      text: "",
      status: "pending",
      trace: [],
      citations: [],
    };
    this._messages.update((m) => [...m, userMsg, botMsg]);
    this._processing.set(true);

    // Build a CLEAN conversation payload: every user turn + every real assistant
    // answer (skip the in-flight bubble + any prior error/empty bubble), newest
    // user message last — exactly what the backend's format_chat expects.
    const payload: ChatMsg[] = this._messages()
      .filter(
        (m) =>
          m.role === "user" ||
          (m.status !== "pending" &&
            m.status !== "error" &&
            m.text.trim().length > 0),
      )
      .map((m) => ({ role: m.role, text: m.text }));

    try {
      const reply = await this.ipc.askAssistantChat(payload);
      this._messages.update((m) =>
        m.map((x) =>
          x.id === botMsg.id
            ? {
                ...x,
                status: reply.status,
                text: reply.summary || "(no answer)",
                citations: parseCitations(reply.citations),
              }
            : x,
        ),
      );
    } catch {
      this._messages.update((m) =>
        m.map((x) =>
          x.id === botMsg.id
            ? {
                ...x,
                status: "error" as const,
                text: "Couldn't send your message.",
              }
            : x,
        ),
      );
    } finally {
      this._processing.set(false);
    }
  }

  /**
   * A wake phrase fired: append a USER bubble (the heard command) + a PENDING
   * assistant bubble to the SAME thread. The matching {@link onResult} resolves
   * the assistant bubble.
   */
  private onWake(p: WakeDetectedPayload): void {
    const userMsg: ChatMessage = {
      id: this.nextId++,
      role: "user",
      text: p.command,
      status: "ok",
      trace: [],
      citations: [],
    };
    const botMsg: ChatMessage = {
      id: this.nextId++,
      role: "assistant",
      text: "",
      status: "pending",
      trace: [],
      citations: [],
    };
    this._messages.update((m) => [...m, userMsg, botMsg]);
  }

  /**
   * Attach a LIVE tool-trace chip to the in-flight (last pending) assistant
   * bubble. A "running" event pushes a new chip; a "done" event resolves the most
   * recent matching running chip (or appends one if none). No pending bubble →
   * ignore (the trace has no home). Pure immutable signal updates — no NG0600.
   * Shared by BOTH the voice (EVENT_ASSISTANT_TOOL) and text (EVENT_CHAT_TOOL)
   * tool-trace streams.
   */
  private onTool(p: AssistantToolPayload): void {
    this._messages.update((msgs) => {
      let idx = -1;
      for (let i = msgs.length - 1; i >= 0; i--) {
        if (msgs[i].role === "assistant" && msgs[i].status === "pending") {
          idx = i;
          break;
        }
      }
      if (idx === -1) return msgs;
      const row = msgs[idx];
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
      const next = msgs.slice();
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

  /**
   * A voice answer landed. Clear the listening/processing/in-flight state, then
   * resolve the last pending assistant bubble (summary + citations). If there is
   * NO pending bubble (a manual "Ask AI" with no preceding wake), append a fresh
   * USER bubble (the heard command) + a resolved assistant bubble so the voice
   * turn still lands in the one thread.
   */
  private onResult(p: VoiceActionResultPayload): void {
    this._manualAskInFlight.set(false);
    this._listening.set(false);
    this._processing.set(false);
    this._messages.update((msgs) => {
      let idx = -1;
      for (let i = msgs.length - 1; i >= 0; i--) {
        if (msgs[i].role === "assistant" && msgs[i].status === "pending") {
          idx = i;
          break;
        }
      }
      if (idx === -1) {
        const heard = p.command.trim();
        const botMsg: ChatMessage = {
          id: this.nextId++,
          role: "assistant",
          text: p.summary,
          status: p.status,
          trace: [],
          citations: parseCitations(p.citations),
        };
        // Only add a user bubble when something was actually heard. A manual ask that caught
        // NOTHING (empty command, no preceding wake) must not append an empty user turn — it would
        // render a blank bubble AND ship `{role:"user",text:""}` in the next send()'s history.
        if (!heard) {
          return [...msgs, botMsg];
        }
        const userMsg: ChatMessage = {
          id: this.nextId++,
          role: "user",
          text: heard,
          status: "ok",
          trace: [],
          citations: [],
        };
        return [...msgs, userMsg, botMsg];
      }
      const next = msgs.slice();
      // Resolve the assistant bubble.
      next[idx] = {
        ...next[idx],
        status: p.status,
        text: p.summary,
        citations: parseCitations(p.citations),
      };
      // Backfill the heard command onto the preceding user bubble if the wake
      // event never captured one (wake fired with an empty trailing command).
      const prev = next[idx - 1];
      if (prev && prev.role === "user" && !prev.text.trim() && p.command) {
        next[idx - 1] = { ...prev, text: p.command };
      }
      return next;
    });
  }
}
