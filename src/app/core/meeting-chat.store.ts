import { Injectable, computed, inject, signal } from "@angular/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "./ipc.service";
import { parseCitations } from "./assistant.store";
import type { AssistantCitation, ToolTraceStep } from "./assistant.store";
import type {
  AssistantToolPayload,
  ChatMsg,
  VoiceActionStatus,
} from "./models";

/**
 * One message in the in-meeting CHAT conversation (the dedicated chat panel). A
 * user message carries the question; an assistant message carries the brain's
 * answer (resolved from "pending"), its live tool-trace, and grounding citations.
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
 * Signal-based store for the in-meeting CHAT panel — a MULTI-TURN conversation
 * with the brain, distinct from the quick-Q&A assistant card. Owns the whole
 * conversation; on each send it ships the FULL history to `ask_assistant_chat`
 * (so the brain has conversation memory), resolves the in-flight assistant
 * bubble with the reply, and accretes the live tool-trace from `EVENT_CHAT_TOOL`.
 * Listen-once-in-init() + signal-only state — no NgRx, no subscribe-into-a-field.
 */
@Injectable({ providedIn: "root" })
export class MeetingChatStore {
  private readonly ipc = inject(IpcService);

  private readonly _messages = signal<ChatMessage[]>([]);
  readonly messages = this._messages.asReadonly();
  readonly hasMessages = computed(() => this._messages().length > 0);

  /** True while a chat turn is in flight (the composer disables, the bubble shows the trace). */
  private readonly _pending = signal(false);
  readonly pending = this._pending.asReadonly();

  /** Whether the slide-out chat panel is open. */
  private readonly _open = signal(false);
  readonly open = this._open.asReadonly();

  private nextId = 1;
  private nextTraceId = 1;
  private unlistenTool: UnlistenFn | null = null;
  private initializing = false;

  /** Subscribe once to the chat tool-trace stream. Idempotent + concurrency-safe. */
  async init(): Promise<void> {
    if (this.unlistenTool || this.initializing) return;
    this.initializing = true;
    this.unlistenTool = await this.ipc.onChatTool((p) => this.onTool(p));
  }

  dispose(): void {
    this.unlistenTool?.();
    this.unlistenTool = null;
  }

  openPanel(): void {
    this._open.set(true);
  }
  closePanel(): void {
    this._open.set(false);
  }
  toggle(): void {
    this._open.update((o) => !o);
  }
  clear(): void {
    this._messages.set([]);
  }

  /**
   * Send a chat message: optimistically append the user bubble + a pending
   * assistant bubble, ship the FULL clean conversation to the multi-turn brain,
   * then resolve the assistant bubble with the reply. The live tool-trace lands
   * via {@link onTool}. A no-op while another turn is in flight.
   */
  async send(text: string): Promise<void> {
    const t = text.trim();
    if (!t || this._pending()) return;
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
    this._pending.set(true);

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
            ? { ...x, status: "error" as const, text: "Couldn't send your message." }
            : x,
        ),
      );
    } finally {
      this._pending.set(false);
    }
  }

  /** Attach a live tool-trace chip to the in-flight (last pending) assistant bubble. */
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
}
