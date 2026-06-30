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
 * store's listening/processing/in-flight signals + the newest in-flight thread
 * by {@link MeetingConversationStore.orbState}, never set directly.
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
 * count only (no PII). Pushed/updated by {@link MeetingConversationStore.onTool}.
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
 * One turn inside a note's `@brain` THREAD (Slack-style). A thread is a small
 * multi-turn conversation anchored UNDER a note line:
 *   - `user`  — the user's question (the `@brain` marker stripped), or a typed
 *               follow-up inside the thread (no marker needed there);
 *   - `agent` — the brain's reply, with its live tool-trace + citations. Every
 *               agent turn offers "✓ Add to notes" (the agent PROPOSES; the user
 *               ACCEPTS — the only path content enters the main notes).
 */
export interface ThreadTurn {
  /** Stable id for `@for` tracking (never key on $index). */
  id: number;
  role: "user" | "agent";
  /** The question (user) or the answer markdown (agent; empty while pending). */
  text: string;
  /** Agent only: "pending" while in flight, then the result status. */
  status: "pending" | VoiceActionStatus;
  /** Agent only: the live tool-trace chips for this turn. */
  trace: ToolTraceStep[];
  /** Agent only: grounding citations (vault `[[Title]]` + "via web"). */
  citations: AssistantCitation[];
  /** Agent only: true once the user has accepted this turn into the main notes. */
  accepted: boolean;
}

/**
 * One line of the user's NOTES — the MAIN flow (the primary content). A line is
 * either a plain jotting OR the anchor of a `@brain` THREAD (the question that
 * opened the thread is the line text; the Q&A lives in `thread`).
 *
 *   - `text`         — the note line shown in the main flow. For a thread anchor
 *                      this is the user's question (marker stripped).
 *   - `thread`       — the Slack-style nested conversation, or `null` for a plain
 *                      note line that has no thread.
 *   - `threadOpen`   — whether the nested thread is expanded (collapsible).
 *   - `threadPending`— true while THIS thread has an in-flight agent turn (so the
 *                      thread's own follow-up input + the main composer can guard).
 *   - `persisted`    — true when this line's text is part of the saved
 *                      `manual_notes` buffer. A thread-anchor question is NOT a
 *                      note until the user accepts an agent reply into the notes,
 *                      so it stays `false` (it doesn't pollute the saved buffer).
 */
export interface NoteItem {
  /** Stable id for `@for` tracking (never key on $index). */
  id: number;
  text: string;
  thread: ThreadTurn[] | null;
  threadOpen: boolean;
  threadPending: boolean;
  persisted: boolean;
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
 * The in-meeting NOTES + `@brain` THREADS store (Slack-style; the agent PROPOSES,
 * the user ACCEPTS). The MAIN flow is the user's notes — a vertical list of
 * {@link NoteItem} lines persisted to `manual_notes`. An `@brain` line opens an
 * anchored, multi-turn {@link ThreadTurn} thread under that note; every agent
 * reply offers "✓ Add to notes" — the ONLY path content enters the main notes
 * (the agent never auto-writes; the backend in-meeting loop is READ-ONLY).
 *
 * Subscribes ONCE (the RecorderStore.init() pattern) to the wake + result +
 * listening + processing + BOTH tool-trace streams and lands every payload in a
 * `signal` — no NgRx, no subscribe-into-a-field. A voice turn lands in the
 * currently-active thread (or opens a fresh anchorless thread), so the voice
 * question + answer are still acceptable into the notes.
 */
@Injectable({ providedIn: "root" })
export class MeetingConversationStore {
  private readonly ipc = inject(IpcService);

  /** The user's NOTES — the main flow (oldest → newest). Each item may host a thread. */
  private readonly _notes = signal<NoteItem[]>([]);
  readonly notes = this._notes.asReadonly();
  /** Whether anything exists yet (drives the empty-state copy). */
  readonly hasNotes = computed(() => this._notes().length > 0);

  /**
   * True once the active meeting's notes have finished hydrating from
   * `manual_notes` (or there is no meeting to load). The composer is DISABLED
   * until this is true — closing the hydrate-vs-type race: a note submitted
   * before `getManualNotes` resolves would overwrite the server buffer with just
   * the fresh line, and then `loadNotes` would skip hydration (flow length > 0),
   * silently losing the pre-existing server notes. Starts `true` (no meeting yet
   * → nothing to wait on); flipped to `false` the instant a meeting id is set,
   * back to `true` in `loadNotes`'s finally.
   */
  private readonly _loaded = signal(true);
  readonly loaded = this._loaded.asReadonly();

  /**
   * The active recording's meeting id, set by the record screen via
   * {@link setMeetingId}. Notes are persisted to THIS meeting's `manual_notes`
   * buffer. Null when there's no live meeting → a note still shows in the flow
   * but can't be persisted (no meeting to save to yet).
   */
  private readonly _meetingId = signal<string | null>(null);
  readonly meetingId = this._meetingId.asReadonly();

  /**
   * The meeting's manual-notes plaintext buffer, kept in sync so a new note line
   * or an ACCEPTED agent draft APPENDS ("existing\ntext"). Seeded from
   * `getManualNotes` by {@link loadNotes}; updated locally on every change.
   */
  private notesBuffer = "";
  /** Monotonic token so a late `getManualNotes` for a previous meeting is dropped. */
  private notesLoadToken = 0;

  /**
   * The thread the NEXT voice answer should land in. Set when a voice ask is
   * fired (a fresh anchorless thread is opened); the result resolves that
   * thread's pending agent turn. Null → no voice turn in flight.
   */
  private voiceTargetNoteId: number | null = null;

  /**
   * True while the manual "Ask AI" listener has the mic open (between the
   * `{active:true}` and `{active:false}` EVENT_VOICE_COMMAND_LISTENING events).
   * Drives the pulsing mic button + listening indicator in the recording bar.
   */
  private readonly _listening = signal(false);
  readonly listening = this._listening.asReadonly();

  /**
   * True from the instant a manual ask is fired until its answer lands — keeps
   * the surface visible across the whole round-trip even when the realtime
   * config toggle is off. Set by {@link askNow}, cleared when a result resolves.
   */
  private readonly _manualAskInFlight = signal(false);
  readonly manualAskInFlight = this._manualAskInFlight.asReadonly();

  /**
   * True while a dispatched VOICE command is being processed — the gap between
   * the listener stopping and the answer landing. Drives the recording bar's
   * Ask-AI disabled state. (Per-THREAD text turns track their own `threadPending`
   * so two threads don't block each other — only voice uses this global flag.)
   */
  private readonly _processing = signal(false);
  readonly processing = this._processing.asReadonly();

  /**
   * The 4-state orb model collapsed from the existing signals — a PURE
   * `computed` (no signal writes → no NG0600 / trap T1):
   *   processing → "processing" (a voice dispatch is in flight)
   *   listening / manual ask in flight → "listening" (the mic is open)
   *   any thread has a resolved agent turn → "answer"
   *   otherwise → "idle".
   */
  readonly orbState = computed<OrbState>(() => {
    if (this._processing()) return "processing";
    if (this._listening() || this._manualAskInFlight()) return "listening";
    for (const n of this._notes()) {
      const thread = n.thread;
      if (!thread) continue;
      for (const t of thread) {
        if (t.role === "agent" && t.status !== "pending") return "answer";
      }
    }
    return "idle";
  });

  /** Monotonic id source for note items (stable `@for` keys). */
  private nextNoteId = 1;
  /** Monotonic id source for thread turns (stable `@for` keys). */
  private nextTurnId = 1;
  /** Monotonic id source for tool-trace chips (stable `@for` keys). */
  private nextTraceId = 1;

  private unlistenWake: UnlistenFn | null = null;
  private unlistenResult: UnlistenFn | null = null;
  private unlistenListening: UnlistenFn | null = null;
  private unlistenProcessing: UnlistenFn | null = null;
  private unlistenTool: UnlistenFn | null = null;
  private unlistenChatTool: UnlistenFn | null = null;
  /** Synchronous re-entrancy guard so two concurrent init() calls can't double-subscribe. */
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
    // Both tool-trace streams feed the in-flight thread turn: EVENT_ASSISTANT_TOOL
    // from the voice path, EVENT_CHAT_TOOL from ask_assistant_chat (text). Each
    // chip lands on the most recent pending agent turn (no backend change).
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

  /** Empty the notes + threads (called on each new recording). */
  clear(): void {
    this._notes.set([]);
    this.voiceTargetNoteId = null;
  }

  /**
   * Point the store at the active recording's meeting (the record screen calls
   * this on each meeting-id change). When the id changes we (re)load the existing
   * `manual_notes` buffer + seed the note flow from it so a later note append
   * extends — not clobbers — the prior notes. A null id clears the buffer.
   */
  setMeetingId(id: string | null): void {
    if (id === this._meetingId()) return;
    this._meetingId.set(id);
    const token = ++this.notesLoadToken;
    if (!id) {
      // No meeting to load → nothing to wait on; the composer stays enabled.
      this.notesBuffer = "";
      this._loaded.set(true);
      return;
    }
    // A real meeting → block submission until `manual_notes` has hydrated.
    this._loaded.set(false);
    void this.loadNotes(id, token);
  }

  /**
   * Seed the notes buffer + the note flow from the persisted `manual_notes`.
   * Stale-guarded: a response for a meeting we've since left is dropped (it must
   * NOT flip `loaded` for a meeting we've already navigated away from). Failure
   * (locked/sealed/transient) leaves an empty buffer — local appends still work.
   * Only HYDRATES when the flow is still empty (so we never clobber notes the
   * user already typed in this session before the load resolved); the composer is
   * disabled until this resolves, so the empty-flow precondition is guaranteed
   * for the FIRST load (the hydrate-vs-type race is closed).
   */
  private async loadNotes(id: string, token: number): Promise<void> {
    try {
      const text = await this.ipc.getManualNotes(id);
      if (token !== this.notesLoadToken) return;
      this.notesBuffer = text;
      if (this._notes().length === 0 && text.trim().length > 0) {
        this._notes.set(
          text.split("\n").map((line) => ({
            id: this.nextNoteId++,
            text: line,
            thread: null,
            threadOpen: false,
            threadPending: false,
            persisted: true,
          })),
        );
      }
    } catch {
      if (token !== this.notesLoadToken) return;
      this.notesBuffer = "";
    } finally {
      // Re-enable the composer ONLY for the still-current load (a stale response
      // for a meeting we've left must not unblock the new meeting prematurely).
      if (token === this.notesLoadToken) {
        this._loaded.set(true);
      }
    }
  }

  /**
   * Rebuild the `manual_notes` buffer from the PERSISTED note lines (plain notes
   * + accepted agent drafts, "\n"-joined) and save it. Thread-anchor questions
   * (`persisted: false`) are excluded — only content the user wrote or accepted
   * lands in the durable buffer. A no-op when there's no meeting yet (the flow is
   * shown locally; it persists once a meeting exists / on the next change).
   */
  private persistNotes(): void {
    this.notesBuffer = this._notes()
      .filter((n) => n.persisted)
      .map((n) => n.text)
      .join("\n");
    const id = this._meetingId();
    if (!id) return;
    void this.ipc.saveManualNotes(id, this.notesBuffer).catch(() => {
      // Locked/sealed or transient — the flow keeps the local note; we never
      // noisily fail a note save. (The buffer kept the local change.)
    });
  }

  /**
   * Add a plain NOTE line to the main flow (the non-@brain path) + persist. The
   * line is the user's own note (never sent to the agent). A no-op for blank text.
   */
  addNote(text: string): void {
    const t = text.trim();
    if (!t) return;
    this._notes.update((ns) => [
      ...ns,
      {
        id: this.nextNoteId++,
        text: t,
        thread: null,
        threadOpen: false,
        threadPending: false,
        persisted: true,
      },
    ]);
    this.persistNotes();
  }

  /**
   * Open a `@brain` THREAD: append a NEW note line whose text is the question
   * (the anchor) hosting a thread with the user's question + a pending agent
   * turn, then ship the thread's history to the multi-turn brain and resolve the
   * pending agent turn with the reply. The anchor line is NOT persisted to
   * `manual_notes` (it's a question, not a note) — content only enters the notes
   * when the user ACCEPTS an agent reply. A no-op for a blank question.
   */
  async openThread(question: string): Promise<void> {
    const q = question.trim();
    if (!q) return;
    const noteId = this.nextNoteId++;
    const userTurn: ThreadTurn = {
      id: this.nextTurnId++,
      role: "user",
      text: q,
      status: "ok",
      trace: [],
      citations: [],
      accepted: false,
    };
    const agentTurn: ThreadTurn = {
      id: this.nextTurnId++,
      role: "agent",
      text: "",
      status: "pending",
      trace: [],
      citations: [],
      accepted: false,
    };
    this._notes.update((ns) => [
      ...ns,
      {
        id: noteId,
        text: q,
        thread: [userTurn, agentTurn],
        threadOpen: true,
        threadPending: true,
        persisted: false,
      },
    ]);
    await this.runAgentTurn(noteId, agentTurn.id);
  }

  /**
   * A FOLLOW-UP inside an existing thread: append the user's question + a pending
   * agent turn to THAT thread (no `@brain` needed inside a thread), then ship the
   * thread's OWN history to the multi-turn brain. A no-op for blank text or while
   * the thread already has a turn in flight.
   */
  async followUp(noteId: number, text: string): Promise<void> {
    const t = text.trim();
    if (!t) return;
    const note = this._notes().find((n) => n.id === noteId);
    if (!note || !note.thread || note.threadPending) return;
    const userTurn: ThreadTurn = {
      id: this.nextTurnId++,
      role: "user",
      text: t,
      status: "ok",
      trace: [],
      citations: [],
      accepted: false,
    };
    const agentTurn: ThreadTurn = {
      id: this.nextTurnId++,
      role: "agent",
      text: "",
      status: "pending",
      trace: [],
      citations: [],
      accepted: false,
    };
    this._notes.update((ns) =>
      ns.map((n) =>
        n.id === noteId
          ? {
              ...n,
              thread: [...(n.thread ?? []), userTurn, agentTurn],
              threadOpen: true,
              threadPending: true,
            }
          : n,
      ),
    );
    await this.runAgentTurn(noteId, agentTurn.id);
  }

  /**
   * Ship a thread's OWN turns (its history) to `ask_assistant_chat` (multi-turn
   * memory scoped to THIS thread) and resolve the given pending agent turn with
   * the reply. The live tool-trace lands via {@link onTool} (the most recent
   * pending agent turn). Always clears the thread's `threadPending` flag.
   */
  private async runAgentTurn(noteId: number, agentTurnId: number): Promise<void> {
    const note = this._notes().find((n) => n.id === noteId);
    const thread = note?.thread ?? [];
    // CLEAN history: every user question + every real (resolved, non-error,
    // non-empty) agent answer in THIS thread, oldest → newest — what format_chat
    // expects. The just-added pending agent turn is skipped (status pending).
    const payload: ChatMsg[] = thread
      .filter(
        (turn) =>
          turn.role === "user" ||
          (turn.role === "agent" &&
            turn.status !== "pending" &&
            turn.status !== "error" &&
            turn.text.trim().length > 0),
      )
      .map((turn) => ({
        role: turn.role === "user" ? "user" : "assistant",
        text: turn.text,
      }));

    try {
      const reply = await this.ipc.askAssistantChat(payload);
      this.resolveTurn(noteId, agentTurnId, {
        status: reply.status,
        text: reply.summary || "(no answer)",
        citations: parseCitations(reply.citations),
      });
    } catch {
      this.resolveTurn(noteId, agentTurnId, {
        status: "error",
        text: "Couldn't reach the assistant.",
        citations: [],
      });
    }
  }

  /** Resolve a pending agent turn + clear its thread's pending flag (immutable). */
  private resolveTurn(
    noteId: number,
    agentTurnId: number,
    patch: {
      status: VoiceActionStatus;
      text: string;
      citations: AssistantCitation[];
    },
  ): void {
    this._notes.update((ns) =>
      ns.map((n) => {
        if (n.id !== noteId || !n.thread) return n;
        return {
          ...n,
          threadPending: false,
          thread: n.thread.map((turn) =>
            turn.id === agentTurnId
              ? { ...turn, status: patch.status, text: patch.text, citations: patch.citations }
              : turn,
          ),
        };
      }),
    );
  }

  /** Toggle a note's thread open/closed (collapsible). */
  toggleThread(noteId: number): void {
    this._notes.update((ns) =>
      ns.map((n) =>
        n.id === noteId ? { ...n, threadOpen: !n.threadOpen } : n,
      ),
    );
  }

  /**
   * ACCEPT an agent turn into the MAIN notes (the agent PROPOSES; this is the
   * user's accept — the only path content enters the notes). Append a NEW plain,
   * PERSISTED note line carrying the agent's text + mark the source turn accepted
   * (so its "✓ Add to notes" affordance flips to "Added"), then persist. A no-op
   * for an already-accepted / empty turn.
   */
  acceptIntoNotes(noteId: number, agentTurnId: number): void {
    const note = this._notes().find((n) => n.id === noteId);
    const turn = note?.thread?.find((t) => t.id === agentTurnId);
    if (!turn || turn.role !== "agent" || turn.accepted) return;
    const text = turn.text.trim();
    if (!text) return;
    this._notes.update((ns) => {
      const marked = ns.map((n) => {
        if (n.id !== noteId || !n.thread) return n;
        return {
          ...n,
          thread: n.thread.map((t) =>
            t.id === agentTurnId ? { ...t, accepted: true } : t,
          ),
        };
      });
      return [
        ...marked,
        {
          id: this.nextNoteId++,
          text,
          thread: null,
          threadOpen: false,
          threadPending: false,
          persisted: true,
        },
      ];
    });
    this.persistNotes();
  }

  /**
   * DISMISS an agent turn (the propose-accept "reject"). The reply is discarded:
   * the turn's text is blanked to a quiet placeholder + marked accepted so the
   * "✓ Add to notes" affordance disappears. Nothing enters the notes.
   */
  dismissTurn(noteId: number, agentTurnId: number): void {
    this._notes.update((ns) =>
      ns.map((n) => {
        if (n.id !== noteId || !n.thread) return n;
        return {
          ...n,
          thread: n.thread.map((t) =>
            t.id === agentTurnId
              ? { ...t, accepted: true, citations: [], text: "" }
              : t,
          ),
        };
      }),
    );
  }

  /**
   * Fire the manual "Ask AI" trigger: open a fresh anchorless thread to host the
   * voice turn, open the listener, and mark a manual ask in flight so the surface
   * stays visible for the whole round-trip. Errors clear the in-flight flag.
   */
  async askNow(): Promise<void> {
    this._manualAskInFlight.set(true);
    // Open a fresh thread (anchored to a placeholder note line) to host the voice
    // Q&A; the result resolves its pending agent turn. The anchor text is filled
    // with the heard command when the result lands.
    const noteId = this.nextNoteId++;
    const userTurn: ThreadTurn = {
      id: this.nextTurnId++,
      role: "user",
      text: "",
      status: "ok",
      trace: [],
      citations: [],
      accepted: false,
    };
    const agentTurn: ThreadTurn = {
      id: this.nextTurnId++,
      role: "agent",
      text: "",
      status: "pending",
      trace: [],
      citations: [],
      accepted: false,
    };
    this.voiceTargetNoteId = noteId;
    this._notes.update((ns) => [
      ...ns,
      {
        id: noteId,
        text: "🎙 …",
        thread: [userTurn, agentTurn],
        threadOpen: true,
        threadPending: true,
        persisted: false,
      },
    ]);
    try {
      await this.ipc.beginVoiceCommand();
    } catch (e) {
      this._manualAskInFlight.set(false);
      this._listening.set(false);
      this.voiceTargetNoteId = null;
      this.resolveTurn(noteId, agentTurn.id, {
        status: "error",
        text: "Couldn't start the listener.",
        citations: [],
      });
      throw e;
    }
  }

  /**
   * CLICK-TO-STOP: stop the open listener so the FULL accumulated utterance is
   * dispatched. Optimistically flip `listening` off + `processing` on so the orb
   * morphs to PROCESSING the instant the user clicks; the answer clears
   * processing via {@link onResult}. A no-op backend (nothing armed) is fine.
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
   * A wake phrase fired: open a fresh anchorless thread (the heard command as the
   * user turn + a pending agent turn). The matching {@link onResult} resolves it.
   */
  private onWake(p: WakeDetectedPayload): void {
    const noteId = this.nextNoteId++;
    const userTurn: ThreadTurn = {
      id: this.nextTurnId++,
      role: "user",
      text: p.command,
      status: "ok",
      trace: [],
      citations: [],
      accepted: false,
    };
    const agentTurn: ThreadTurn = {
      id: this.nextTurnId++,
      role: "agent",
      text: "",
      status: "pending",
      trace: [],
      citations: [],
      accepted: false,
    };
    this.voiceTargetNoteId = noteId;
    this._notes.update((ns) => [
      ...ns,
      {
        id: noteId,
        text: p.command || "🎙 …",
        thread: [userTurn, agentTurn],
        threadOpen: true,
        threadPending: true,
        persisted: false,
      },
    ]);
  }

  /**
   * Attach a LIVE tool-trace chip to the in-flight (last pending) agent turn,
   * across ALL threads. A "running" event pushes a new chip; a "done" event
   * resolves the most recent matching running chip (or appends one). No pending
   * agent turn → ignore. Pure immutable signal updates — no NG0600. Shared by the
   * voice (EVENT_ASSISTANT_TOOL) + text (EVENT_CHAT_TOOL) tool-trace streams.
   *
   * KNOWN LIMITATION (v1, acceptable): the EVENT_CHAT_TOOL payload carries NO
   * thread id, so when TWO text threads are pending SIMULTANEOUSLY a chip lands on
   * the most-recently-opened pending agent turn — which may not be the one that
   * actually made the call. This is non-PII + purely cosmetic (the trace chip is a
   * coarse "Searching notes…" badge, never content) and is acceptable for v1;
   * fixing it would need the backend to stamp a thread id on the tool event.
   */
  private onTool(p: AssistantToolPayload): void {
    this._notes.update((ns) => {
      // Find the most recent pending agent turn across the flow (newest note first).
      for (let ni = ns.length - 1; ni >= 0; ni--) {
        const note = ns[ni];
        if (!note.thread) continue;
        for (let ti = note.thread.length - 1; ti >= 0; ti--) {
          const turn = note.thread[ti];
          if (turn.role === "agent" && turn.status === "pending") {
            const trace = turn.trace.slice();
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
            const nextThread = note.thread.slice();
            nextThread[ti] = { ...turn, trace };
            const next = ns.slice();
            next[ni] = { ...note, thread: nextThread };
            return next;
          }
        }
      }
      return ns;
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
   * resolve the VOICE target thread's pending agent turn (summary + citations) +
   * backfill the heard command onto its (possibly empty) user turn / anchor line.
   *
   * A voice result resolves ONLY the voice-originated thread (`voiceTargetNoteId`).
   * If that target is gone (null — e.g. cleared by a new recording, or a race),
   * we APPEND a fresh anchorless thread for the voice Q&A rather than STEALING the
   * newest pending TEXT thread — clobbering a typed thread's anchor with the heard
   * command would corrupt an unrelated `@brain` conversation.
   */
  private onResult(p: VoiceActionResultPayload): void {
    this._manualAskInFlight.set(false);
    this._listening.set(false);
    this._processing.set(false);
    const targetId = this.voiceTargetNoteId;
    this.voiceTargetNoteId = null;
    const heard = p.command.trim();

    if (targetId === null) {
      // No voice thread to resolve → append a fresh, already-resolved thread so
      // the voice turn still lands (never steal a pending text thread).
      const userTurn: ThreadTurn = {
        id: this.nextTurnId++,
        role: "user",
        text: heard,
        status: "ok",
        trace: [],
        citations: [],
        accepted: false,
      };
      const agentTurn: ThreadTurn = {
        id: this.nextTurnId++,
        role: "agent",
        text: p.summary,
        status: p.status,
        trace: [],
        citations: parseCitations(p.citations),
        accepted: false,
      };
      // Only include the user turn when something was actually heard (a manual ask
      // that caught nothing must not leave an empty user bubble).
      const thread: ThreadTurn[] = heard ? [userTurn, agentTurn] : [agentTurn];
      this._notes.update((ns) => [
        ...ns,
        {
          id: this.nextNoteId++,
          text: heard || "🎙 …",
          thread,
          threadOpen: true,
          threadPending: false,
          persisted: false,
        },
      ]);
      return;
    }

    this._notes.update((ns) =>
      ns.map((n) => {
        // Resolve ONLY the voice target thread (never a text thread).
        if (n.id !== targetId || !n.thread) return n;
        // Resolve the last pending agent turn in this thread.
        const thread = n.thread.slice();
        let resolvedUser = false;
        for (let i = thread.length - 1; i >= 0; i--) {
          if (thread[i].role === "agent" && thread[i].status === "pending") {
            thread[i] = {
              ...thread[i],
              status: p.status,
              text: p.summary,
              citations: parseCitations(p.citations),
            };
            // Backfill the heard command onto the preceding empty user turn.
            const prev = thread[i - 1];
            if (prev && prev.role === "user" && !prev.text.trim() && heard) {
              thread[i - 1] = { ...prev, text: heard };
              resolvedUser = true;
            }
            break;
          }
        }
        return {
          ...n,
          threadPending: false,
          // Show the heard command as the anchor line when it was a placeholder.
          text: heard && (resolvedUser || !n.text.trim() || n.text === "🎙 …")
            ? heard
            : n.text,
          thread,
        };
      }),
    );
  }
}
