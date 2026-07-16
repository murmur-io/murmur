import { Injectable, computed, effect, inject, signal } from "@angular/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "./ipc.service";
import { FoldersService } from "../services/folders.service";
import { ToastService } from "../services/toast.service";
import type {
  AnsweredFrom,
  AssistantThreadRow,
  AssistantToolPayload,
  ChatMsg,
  FolderNode,
  ProactiveHintPayload,
  VoiceActionResultPayload,
  VoiceActionStatus,
  VoiceCommandListeningPayload,
  VoiceCommandProcessingPayload,
  WakeDetectedPayload,
  WhisperCard,
} from "./models";

/**
 * One Realtime-Reactions "whisper" contradiction card on the record-screen rail,
 * wrapping the backend {@link WhisperCard} with a stable id for `@for` tracking.
 * EPHEMERAL — never persisted, and PURGED from the rail on any lock transition
 * (screen-share auto-relock / Lock all / a fresh seal), the FE analogue of the
 * `convertFileSrc` gate so a card citing a just-sealed meeting never lingers.
 */
export interface RailWhisperCard extends WhisperCard {
  /** Stable id for `@for` tracking (never key a card on $index). */
  id: number;
}

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
 *               follow-up inside the thread (no marker needed there). The FIRST
 *               user turn IS the anchor question — the surface renders it ONCE on
 *               the anchor line and skips it inside the thread (no duplication);
 *   - `agent` — the brain's reply, with its live tool-trace + citations. An agent
 *               turn offers "✓ Add to notes" ONLY when it carries a `proposedNote`
 *               (the model decided the user asked it to MAKE a note); a plain
 *               answer has `proposedNote: null` and NO add affordance.
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
  /**
   * Agent only: the proposed NOTE draft, or `null`. NON-null ONLY when the model
   * called `propose_note` (the user asked it to make/save a note). Drives the
   * "✓ Add to notes" affordance: shown ONLY when this is non-null; on accept THIS
   * draft (not the whole reply) is appended to the notes.
   */
  proposedNote: string | null;
  /**
   * Agent only (Phase 5): which BRAIN CASCADE tier answered — current meeting /
   * vault / connectors — set deterministically by the backend ladder. `null`
   * while pending, on an error, or on an older backend that omits it. Drives the
   * visible tier chip ("answered from: this meeting / your vault / connectors").
   */
  answeredFrom: AnsweredFrom | null;
  /** Agent only: true once the user has ACCEPTED this turn's draft into the main notes. */
  accepted: boolean;
  /**
   * Agent only: true once the user DISMISSED this turn's proposal. Distinct from
   * `accepted` so the surface shows "Dismissed" (not "✓ Added to notes") — both
   * paths clear the affordance, but only a real accept committed content.
   */
  dismissed: boolean;
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
 *   - `threadId`     — the PERSISTENT key of this line's thread (an FE-generated
 *                      UUID, minted when the thread opens and shipped with every
 *                      `ask_assistant_chat` call so the backend persists the
 *                      exchanges). Null for a plain note line, and for a voice
 *                      thread until the result payload stamps one. Also routes
 *                      threadId-carrying tool/result events to the RIGHT thread.
 */
export interface NoteItem {
  /** Stable id for `@for` tracking (never key on $index). */
  id: number;
  text: string;
  thread: ThreadTurn[] | null;
  threadOpen: boolean;
  threadPending: boolean;
  persisted: boolean;
  threadId: string | null;
  /**
   * COMPANION NOTE reference. A sent plain jot / accepted `@brain` draft is
   * appended to the meeting's ONE living companion note via
   * `append_to_companion_note`; on success the returned reference is stamped here
   * so the line renders a "✓ Saved to Notes" card:
   *   - `savedNoteId`     — the companion note's document id (open it by id via
   *                         `TabsService.openNote`); `undefined` while the append
   *                         is in flight or if it hasn't been routed through the
   *                         companion path (a hydrated / thread-anchor line);
   *   - `meetingWikilink` — the visible `[[Meeting]]` display link for the card's
   *                         meeting chip (navigation goes by `meetingId`, not this
   *                         string);
   *   - `saveState`       — the append lifecycle: `"saving"` (optimistic, in
   *                         flight), `"saved"` (reference stamped), or `"error"`
   *                         (the append failed — the line is kept, never dropped,
   *                         and the card shows a retry). Absent for lines that
   *                         never went through the companion path.
   */
  savedNoteId?: string;
  meetingWikilink?: string;
  saveState?: "saving" | "saved" | "error";
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

/** The terminal statuses a persisted thread row may carry (mirrors VoiceActionStatus). */
const VOICE_ACTION_STATUSES: ReadonlySet<string> = new Set([
  "ok",
  "needs_consent",
  "unavailable",
  "unrecognized",
  "nothing_heard",
  "error",
]);

/**
 * Narrow a persisted thread row's `status` string (serialized loosely as
 * `string`) back to a {@link VoiceActionStatus}. An unknown value (a future
 * backend adding a status) degrades to "ok" — the turn still renders as a
 * plain resolved answer rather than breaking the thread.
 */
function coerceStatus(s: string): VoiceActionStatus {
  return (
    VOICE_ACTION_STATUSES.has(s) ? s : "ok"
  ) as VoiceActionStatus;
}

/**
 * The in-meeting NOTES + `@brain` THREADS store (Slack-style; the agent PROPOSES,
 * the user ACCEPTS). The MAIN flow is the user's notes — a vertical list of
 * {@link NoteItem} lines persisted to `manual_notes`. An `@brain` line opens an
 * anchored, multi-turn {@link ThreadTurn} thread under that note; an agent reply
 * that carries a `proposedNote` (the model decided the user asked it to MAKE a
 * note) offers "✓ Add to notes" — the ONLY path content enters the main notes
 * (the agent never auto-writes; the backend in-meeting loop is READ-ONLY). A
 * plain answer has no proposal and no add affordance — it reads as conversation.
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
  private readonly folders = inject(FoldersService);
  private readonly toast = inject(ToastService);

  /** The user's NOTES — the main flow (oldest → newest). Each item may host a thread. */
  private readonly _notes = signal<NoteItem[]>([]);
  readonly notes = this._notes.asReadonly();
  /** Whether anything exists yet (drives the empty-state copy). */
  readonly hasNotes = computed(() => this._notes().length > 0);
  /** ENHANCE-MY-NOTES: true once at least one REAL persisted note line exists — i.e. what
   *  the summarizer will actually see (un-accepted @brain anchors are persisted:false). */
  readonly hasPersistedNotes = computed(() =>
    this._notes().some((n) => n.persisted && n.text.trim().length > 0),
  );

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

  /**
   * The current proactive recall hint (`EVENT_PROACTIVE_HINT`), or null. At most
   * ONE card: a newer hint REPLACES the visible one (the backend throttles to
   * ≤1 per cooldown, so replacement is the spec'd queue-of-1). Cleared on a
   * genuinely-new recording (same lifecycle as the conversation flow) and by
   * {@link dismissHint}. No FE timer anywhere — the cooldown lives backend-side.
   */
  private readonly _hint = signal<ProactiveHintPayload | null>(null);
  readonly hint = this._hint.asReadonly();

  /**
   * Hints the user dismissed THIS app session, keyed `kind:targetId` — a
   * dismissed card never resurfaces even if an event for the same target slips
   * through (the backend session-dedups too; this is the FE half of the belt
   * and braces). Deliberately NOT cleared per recording: "dismissed" is a
   * session-scoped user choice, mirroring the backend's session-level dedup.
   */
  private readonly dismissedHints = new Set<string>();

  /**
   * The Realtime-Reactions "whisper" contradiction cards (`EVENT_WHISPER_CARD`) —
   * the SECOND rail lane beside the recall {@link hint}. Bounded (newest first,
   * capped) + deduped so a repeated contradiction doesn't stack. Ephemeral: never
   * persisted, cleared on a new recording / meeting change / lock transition.
   */
  private readonly _whisperCards = signal<RailWhisperCard[]>([]);
  readonly whisperCards = this._whisperCards.asReadonly();
  /** Keep the rail slim — at most this many contradiction cards at once. */
  private static readonly MAX_WHISPER_CARDS = 3;
  /** Monotonic id source for whisper cards (stable `@for` keys). */
  private nextWhisperId = 1;

  /**
   * Shadow-mode calibration (spec §4.2). The contradiction sub-toggle ships OFF;
   * the backend still COUNTS how many contradiction cards WOULD have fired this
   * recording (`brain_reactions_shadow_count`, resets per recording). Once the
   * user's OWN count clears a small bar we offer "the brain would have flagged N —
   * show them live?" → `set_brain_contradiction_cards(true)`. A nonzero count
   * already implies the toggle is OFF (shadow mode only counts while off), so no
   * separate toggle read is needed. Carries a COUNT only (no meeting content) — it
   * is therefore NOT gated content and is not purged on a lock transition.
   */
  private readonly _shadowCount = signal(0);
  readonly shadowCount = this._shadowCount.asReadonly();
  /** Hidden once the user enables / dismisses the calibration this recording. */
  private readonly _shadowDismissed = signal(false);
  /** Offer the calibration only once the shadow count clears this bar. */
  private static readonly SHADOW_THRESHOLD = 2;
  /** Whether to show the shadow-mode calibration card in the rail. */
  readonly showShadowCalibration = computed(
    () =>
      !this._shadowDismissed() &&
      this._shadowCount() >= MeetingConversationStore.SHADOW_THRESHOLD,
  );

  /**
   * PURGE the whole reactions rail (recall hint + whisper cards) whenever a folder
   * gets MORE locked — a relock/seal makes its content invisible again, and a
   * card that already crossed to the FE (a recall title / a contradiction's
   * `[[sourceMeeting]]`) must not outlive that gate. This is the FE analogue of
   * nulling `audio_path` for a sealed meeting (the `convertFileSrc` leak): the
   * content already left the backend, so the FE must drop it on the lock edge. It
   * fires on Lock all, single relock, a fresh seal, AND the screen-share auto-
   * relock (defense in depth over {@link ScreenShareService}'s direct call).
   *
   * The trigger is a DROP in the number of folders whose content is currently
   * visible (`!locked || unlocked`), mirroring the graph's `_refetchOnLock`
   * folder-tree effect. The effect writes the rail signals via
   * {@link clearRail} (allowed by default since Angular 19).
   */
  private prevVisibleFolderCount: number | null = null;
  private readonly _purgeRailOnLock = effect(
    () => {
      const visible = this.countVisibleFolders(this.folders.tree());
      const prev = this.prevVisibleFolderCount;
      this.prevVisibleFolderCount = visible;
      if (prev !== null && visible < prev) this.clearRail();
    },
  );

  /** Count folders whose content is currently visible to this session (`!locked || unlocked`). */
  private countVisibleFolders(nodes: FolderNode[]): number {
    let n = 0;
    const walk = (list: FolderNode[]): void => {
      for (const node of list) {
        if (!node.locked || node.unlocked) n++;
        if (node.children?.length) walk(node.children);
      }
    };
    walk(nodes);
    return n;
  }

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
  private unlistenHint: UnlistenFn | null = null;
  private unlistenWhisper: UnlistenFn | null = null;
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
    this.unlistenHint = await this.ipc.onProactiveHint((p) => this.onHint(p));
    // Realtime Reactions — the whisper contradiction lane of the rail.
    this.unlistenWhisper = await this.ipc.onWhisperCard((p) => this.onWhisper(p));
  }

  /** Release the event subscriptions (e.g. on app teardown). */
  dispose(): void {
    this.unlistenWake?.();
    this.unlistenResult?.();
    this.unlistenListening?.();
    this.unlistenProcessing?.();
    this.unlistenTool?.();
    this.unlistenChatTool?.();
    this.unlistenHint?.();
    this.unlistenWhisper?.();
    this.unlistenWake = null;
    this.unlistenResult = null;
    this.unlistenListening = null;
    this.unlistenProcessing = null;
    this.unlistenTool = null;
    this.unlistenChatTool = null;
    this.unlistenHint = null;
    this.unlistenWhisper = null;
  }

  /** Empty the notes + threads + the whole reactions rail (called on each new recording). */
  clear(): void {
    this._notes.set([]);
    this.voiceTargetNoteId = null;
    this.clearRail();
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
    // Phase 6 — mirror the viewed meeting into the backend FOCUS pointer (safety-net for any
    // assistant path that falls back off an explicit id, e.g. the voice/wake twin). Best-effort;
    // a failure never blocks the view. Cleared (null) when the store points at no meeting.
    void this.ipc.setFocusMeeting(id);
    const token = ++this.notesLoadToken;
    if (!id) {
      // No meeting to load → nothing to wait on; the composer stays enabled.
      this.notesBuffer = "";
      this._loaded.set(true);
      return;
    }
    // A genuinely NEW meeting id → start a fresh conversation: clear the in-memory
    // flow so a PRIOR meeting's threads (which live only here, not in manual_notes)
    // don't bleed into the new meeting, then hydrate this meeting's notes. The
    // same-id case early-returned above, so switching tabs and returning DURING a
    // recording preserves the conversation (this is the fix for the "threads vanish
    // when I leave and come back" data-loss bug — the old per-component clear-on-
    // record effect mis-fired on re-mount because its edge state reset to false).
    this._notes.set([]);
    this.voiceTargetNoteId = null;
    // A stale recall hint / whisper card must not carry into the new meeting's
    // conversation (dismissedHints stays — a dismissal is session-scoped, like the
    // backend dedup).
    this.clearRail();
    // A new recording resets the backend shadow counter — reset the FE mirror +
    // re-arm the calibration prompt for the fresh session.
    this._shadowCount.set(0);
    this._shadowDismissed.set(false);
    this._loaded.set(false);
    void this.hydrate(id, token);
  }

  /**
   * Hydrate a genuinely-new meeting: the notes buffer FIRST (it seeds the note
   * lines + re-enables the composer — timing unchanged), THEN the persisted
   * `@brain` threads, which attach to the seeded lines by anchor text. Same-id
   * re-entry never reaches here (RAM wins — see {@link setMeetingId}), so an
   * in-progress conversation is never clobbered by its own persisted copy.
   */
  private async hydrate(id: string, token: number): Promise<void> {
    await this.loadNotes(id, token);
    if (token !== this.notesLoadToken) return;
    await this.loadThreads(id, token);
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
            threadId: null,
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
   * Rebuild the persisted `@brain` threads for a reopened meeting: fetch the
   * meeting's thread rows (oldest → newest; a sealed-not-unlocked meeting
   * returns [] — gated server-side), group them by `threadId` (insertion
   * order), turn each row into a resolved user + agent turn pair, then ATTACH
   * each group to the FIRST note line whose text equals the group's anchor and
   * which has no thread yet (the ✨-ask-brain / anchored case). A group with no
   * matching line (anchorless voice thread, an edited/deleted note, or an
   * anchor another group already claimed) APPENDS as a standalone collapsed
   * thread line at the end — never lost, never mis-attached.
   *
   * Hydrated turns are terminal history: `proposedNote: null` (no Add-to-notes
   * affordance resurrects — an accepted draft already lives in `manual_notes`),
   * empty trace, `accepted`/`dismissed` false. Threads stay CONTINUABLE: a
   * follow-up ships the rebuilt RAM turn list + the SAME `threadId`, so new
   * exchanges append as new rows backend-side. Stale-guarded like
   * {@link loadNotes}: a response for a meeting we've since left is dropped.
   * Failure (old backend without the command / transient) leaves the notes
   * flow as-is — threads simply don't rehydrate.
   */
  private async loadThreads(id: string, token: number): Promise<void> {
    let rows: AssistantThreadRow[];
    try {
      rows = await this.ipc.listAssistantThreads(id);
    } catch {
      return;
    }
    if (token !== this.notesLoadToken || rows.length === 0) return;

    // Group rows by threadId, preserving first-seen (insertion) order.
    const groups = new Map<string, AssistantThreadRow[]>();
    for (const row of rows) {
      if (!row.threadId) continue; // backend contract: never happens; stay safe
      const group = groups.get(row.threadId);
      if (group) group.push(row);
      else groups.set(row.threadId, [row]);
    }

    this._notes.update((ns) => {
      let next = ns.slice();
      for (const [threadId, group] of groups) {
        // A thread already in RAM under this id must not hydrate twice (can't
        // happen off the fresh-id path, but the guard is cheap).
        if (next.some((n) => n.threadId === threadId)) continue;
        const thread: ThreadTurn[] = [];
        for (const row of group) {
          thread.push({
            id: this.nextTurnId++,
            role: "user",
            text: row.command,
            status: "ok",
            trace: [],
            citations: [],
            proposedNote: null,
            answeredFrom: null,
            accepted: false,
            dismissed: false,
          });
          thread.push({
            id: this.nextTurnId++,
            role: "agent",
            text: row.answer,
            status: coerceStatus(row.status),
            trace: [],
            citations: parseCitations(row.citations),
            proposedNote: null,
            answeredFrom: null,
            accepted: false,
            dismissed: false,
          });
        }
        const anchorText = group[0].anchorText;
        const anchorIdx =
          anchorText === null
            ? -1
            : next.findIndex((n) => n.text === anchorText && !n.thread);
        if (anchorIdx !== -1) {
          next[anchorIdx] = {
            ...next[anchorIdx],
            thread,
            threadOpen: false,
            threadPending: false,
            threadId,
          };
        } else {
          next = [
            ...next,
            {
              id: this.nextNoteId++,
              text: group[0].command,
              thread,
              threadOpen: false,
              threadPending: false,
              persisted: false,
              threadId,
            },
          ];
        }
      }
      return next;
    });
  }

  /**
   * Rebuild the `manual_notes` buffer from the PERSISTED note lines (plain notes
   * + accepted agent drafts, "\n"-joined) and save it. Thread-anchor questions
   * (`persisted: false`) are excluded — only content the user wrote or accepted
   * lands in the durable buffer. A no-op when there's no meeting yet (the flow is
   * shown locally; it persists once a meeting exists / on the next change).
   *
   * On a REJECTED save (e.g. `AppError::Locked` when the folder's session-unlock
   * lapses between the click and the write — `save_manual_notes_inner`,
   * `commands.rs`) the FE flow already shows the note line / "✓ Added to notes"
   * from local state, so a swallowed rejection would silently lie about a save
   * that never landed (the buffer is NOT durable — lost on next load/restart).
   * Surface it via the toast (mirrors `note-editor.component.ts`'s
   * `saveText`/`saveFull`) so the user knows to unlock and retry; the local note
   * line is intentionally kept either way (never destroy content the user typed).
   */
  private persistNotes(): void {
    this.notesBuffer = this._notes()
      .filter((n) => n.persisted)
      .map((n) => n.text)
      .join("\n");
    const id = this._meetingId();
    if (!id) return;
    void this.ipc.saveManualNotes(id, this.notesBuffer).catch((e) => {
      if (String(e).includes("Locked")) {
        this.toast.danger(
          "This meeting's folder is locked — unlock it so your note saves.",
        );
      } else {
        this.toast.danger("Couldn't save your note — it's not synced yet.");
      }
    });
  }

  /**
   * Add a plain NOTE line to the main flow (the non-@brain path). The line is the
   * user's own note (never sent to the agent). A no-op for blank text.
   *
   * The line is now a REAL, LINKED note: OPTIMISTICALLY appended to the flow
   * (`saveState: "saving"`), then persisted to the meeting's ONE living companion
   * note via {@link appendCompanion} — on success the returned
   * `{ noteId, meetingWikilink }` is stamped onto THIS line so it renders the
   * "✓ Saved to Notes" card; on failure the line stays (never dropped) with
   * `saveState: "error"` + a retry. `manual_notes` is ALSO refreshed additively
   * (the summary / enhance pipeline still reads it — see {@link persistNotes}).
   */
  addNote(text: string): void {
    const t = text.trim();
    if (!t) return;
    const noteId = this.nextNoteId++;
    this._notes.update((ns) => [
      ...ns,
      {
        id: noteId,
        text: t,
        thread: null,
        threadOpen: false,
        threadPending: false,
        persisted: true,
        threadId: null,
        saveState: "saving",
      },
    ]);
    // Additive: keep the manual_notes buffer in sync (enhance/summary reads it).
    this.persistNotes();
    // Durable, linked artifact: append to the companion note + stamp the card ref.
    void this.appendCompanion(noteId, t);
  }

  /**
   * Append a flow line's markdown to the meeting's companion note and stamp the
   * returned reference onto THAT line so its "✓ Saved to Notes" card can render.
   *
   * STALE-RESULT GUARDED two ways: (1) the meeting id is captured at call time —
   * a response that lands after the store points at a DIFFERENT meeting is
   * dropped (the line belongs to the old meeting, whose flow was cleared); (2) the
   * target line is re-found by its stable, never-reused `id` at resolve time — if
   * it's gone (a new recording / meeting change cleared the flow) the response is
   * a no-op, never mis-stamped onto a different line. On success:
   * `saveState: "saved"` + `savedNoteId` + `meetingWikilink`; on failure:
   * `saveState: "error"` (the line is KEPT — never destroy content the user typed;
   * the card offers a retry). A no-op when there's no meeting yet (nothing to link
   * to — the line still shows locally; a later {@link retrySave} once a meeting
   * exists can persist it).
   */
  private async appendCompanion(noteId: number, markdown: string): Promise<void> {
    const meetingId = this._meetingId();
    if (!meetingId) {
      // No meeting to link to yet — leave the optimistic "saving" state off (the
      // line simply isn't a companion note yet). Clear the transient flag so it
      // doesn't spin forever with nothing in flight.
      this.patchNote(noteId, { saveState: undefined });
      return;
    }
    try {
      const res = await this.ipc.appendToCompanionNote(meetingId, markdown);
      // Drop a response that landed after we left this meeting (its line is gone).
      if (this._meetingId() !== meetingId) return;
      this.patchNote(noteId, {
        saveState: "saved",
        savedNoteId: res.noteId,
        meetingWikilink: res.meetingWikilink,
      });
    } catch {
      if (this._meetingId() !== meetingId) return;
      // Keep the line; surface the failure on the card (retryable), never drop it.
      this.patchNote(noteId, { saveState: "error" });
    }
  }

  /** Immutably patch ONE flow line by its stable id (a no-op if it's gone). */
  private patchNote(noteId: number, patch: Partial<NoteItem>): void {
    this._notes.update((ns) =>
      ns.map((n) => (n.id === noteId ? { ...n, ...patch } : n)),
    );
  }

  /**
   * Retry a FAILED companion-note append (the "✓ Saved to Notes" card's error
   * state offers this). Re-optimistic (`saveState: "saving"`) then re-run the
   * append for the line's current text. A no-op for a line that isn't in the
   * error state.
   */
  retrySave(noteId: number): void {
    const note = this._notes().find((n) => n.id === noteId);
    if (!note || note.saveState !== "error") return;
    this.patchNote(noteId, { saveState: "saving" });
    void this.appendCompanion(noteId, note.text);
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
    // Mint the PERSISTENT thread key up front — every ask_assistant_chat call
    // for this thread ships it, so the backend persists the exchanges under it.
    const threadId = crypto.randomUUID();
    const userTurn: ThreadTurn = {
      id: this.nextTurnId++,
      role: "user",
      text: q,
      status: "ok",
      trace: [],
      citations: [],
      proposedNote: null,
      answeredFrom: null,
      accepted: false,
      dismissed: false,
    };
    const agentTurn: ThreadTurn = {
      id: this.nextTurnId++,
      role: "agent",
      text: "",
      status: "pending",
      trace: [],
      citations: [],
      proposedNote: null,
      answeredFrom: null,
      accepted: false,
      dismissed: false,
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
        threadId,
      },
    ]);
    await this.runAgentTurn(noteId, agentTurn.id);
  }

  /**
   * "✨ ask brain" on an EXISTING plain note: retroactively open a thread on a
   * note that has none yet, seeding the agent from the NOTE'S OWN TEXT as context.
   * The note line STAYS the user's note (NOT converted/deleted, still `persisted`)
   * — the thread just hangs under it like a `@brain` thread.
   *
   * The seeded first user turn carries the note text as the question so the agent
   * can ANSWER about it or PROPOSE a note from it; it ships through the same
   * `runAgentTurn` / `ask_assistant_chat` path as `@brain`, so the tool-trace,
   * `proposedNote`-gated Add-to-notes, and follow-ups all behave identically. A
   * no-op for a missing note, a note that ALREADY has a thread, or blank text.
   */
  async askBrainOnNote(noteId: number): Promise<void> {
    const note = this._notes().find((n) => n.id === noteId);
    if (!note || note.thread) return; // only notes WITHOUT a thread yet
    const subject = note.text.trim();
    if (!subject) return;
    // A retroactive thread is a NEW thread → mint its persistent key. The note's
    // text is the anchor, so hydration can re-attach the thread to this line.
    const threadId = crypto.randomUUID();
    const userTurn: ThreadTurn = {
      id: this.nextTurnId++,
      role: "user",
      // Seed the agent from the note's own words (this becomes the thread's first
      // user turn → shipped as the question). It's sliced from the rendered thread
      // by visibleTurns() because the note line already shows the text above.
      text: subject,
      status: "ok",
      trace: [],
      citations: [],
      proposedNote: null,
      answeredFrom: null,
      accepted: false,
      dismissed: false,
    };
    const agentTurn: ThreadTurn = {
      id: this.nextTurnId++,
      role: "agent",
      text: "",
      status: "pending",
      trace: [],
      citations: [],
      proposedNote: null,
      answeredFrom: null,
      accepted: false,
      dismissed: false,
    };
    this._notes.update((ns) =>
      ns.map((n) =>
        n.id === noteId
          ? {
              ...n,
              // The note stays exactly as it is (text + persisted unchanged); we
              // only attach the thread + open it.
              thread: [userTurn, agentTurn],
              threadOpen: true,
              threadPending: true,
              threadId,
            }
          : n,
      ),
    );
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
    // A thread that somehow lacks a persistent key (a voice thread whose result
    // never stamped one / pre-persistence RAM state) gets one NOW, so this and
    // every later exchange persist under the same thread.
    const threadId = note.threadId ?? crypto.randomUUID();
    const userTurn: ThreadTurn = {
      id: this.nextTurnId++,
      role: "user",
      text: t,
      status: "ok",
      trace: [],
      citations: [],
      proposedNote: null,
      answeredFrom: null,
      accepted: false,
      dismissed: false,
    };
    const agentTurn: ThreadTurn = {
      id: this.nextTurnId++,
      role: "agent",
      text: "",
      status: "pending",
      trace: [],
      citations: [],
      proposedNote: null,
      answeredFrom: null,
      accepted: false,
      dismissed: false,
    };
    this._notes.update((ns) =>
      ns.map((n) =>
        n.id === noteId
          ? {
              ...n,
              thread: [...(n.thread ?? []), userTurn, agentTurn],
              threadOpen: true,
              threadPending: true,
              threadId,
            }
          : n,
      ),
    );
    await this.runAgentTurn(noteId, agentTurn.id);
  }

  /**
   * Ship a thread's OWN turns (its history) to `ask_assistant_chat` (multi-turn
   * memory scoped to THIS thread) and resolve the given pending agent turn with
   * the reply. Ships the note's `threadId` + its anchor text (the note line) +
   * the store's anchored `meetingId` (Phase 4 — so the brain scopes "this meeting"
   * to the bound meeting, not whatever is recording), so the backend PERSISTS the
   * exchange under the thread — a reopened meeting rebuilds it via
   * `list_assistant_threads`. The live tool-trace lands via
   * {@link onTool} (routed by the payload's threadId when stamped, else the most
   * recent pending agent turn). Always clears the thread's `threadPending` flag.
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
      const reply = await this.ipc.askAssistantChat(
        payload,
        note?.threadId ?? undefined,
        note?.text.trim() || undefined,
        // Phase 4: bind this thread to the store's anchored meeting so the brain
        // scopes "this meeting" to it (a past/anchored thread answers about ITS
        // meeting; omitting it would fall back to state.current_meeting → the
        // wrong-meeting bug). Null → undefined so the backend uses the recording.
        this._meetingId() ?? undefined,
      );
      this.resolveTurn(noteId, agentTurnId, {
        status: reply.status,
        text: reply.summary || "(no answer)",
        citations: parseCitations(reply.citations),
        proposedNote: reply.proposedNote,
        // Phase 5: the deterministic tier badge from the ladder (or null).
        answeredFrom: reply.answeredFrom ?? null,
      });
    } catch {
      this.resolveTurn(noteId, agentTurnId, {
        status: "error",
        text: "Couldn't reach the assistant.",
        citations: [],
        proposedNote: null,
        answeredFrom: null,
      });
    }
  }

  /**
   * Resolve a pending agent turn + clear its thread's pending flag (immutable).
   * Captures `proposedNote` — the agent's note draft (or null) — which drives the
   * "✓ Add to notes" affordance ONLY when non-null.
   */
  private resolveTurn(
    noteId: number,
    agentTurnId: number,
    patch: {
      status: VoiceActionStatus;
      text: string;
      citations: AssistantCitation[];
      proposedNote: string | null;
      answeredFrom: AnsweredFrom | null;
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
              ? {
                  ...turn,
                  status: patch.status,
                  text: patch.text,
                  citations: patch.citations,
                  proposedNote: patch.proposedNote,
                  answeredFrom: patch.answeredFrom,
                }
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
   * ACCEPT an agent turn's PROPOSED NOTE into the MAIN notes (the agent PROPOSES;
   * this is the user's accept — the only path content enters the notes). Append a
   * NEW plain, PERSISTED note line carrying the turn's `proposedNote` DRAFT (NOT
   * the whole reply) + mark the source turn accepted (so its "✓ Add to notes"
   * affordance flips to "Added"), then persist. A no-op for an already-accepted
   * turn or a turn with NO proposed note (a plain answer — nothing to add).
   */
  acceptIntoNotes(noteId: number, agentTurnId: number): void {
    const note = this._notes().find((n) => n.id === noteId);
    const turn = note?.thread?.find((t) => t.id === agentTurnId);
    if (!turn || turn.role !== "agent" || turn.accepted) return;
    // Append the PROPOSED note draft, never the whole conversational reply.
    const text = (turn.proposedNote ?? "").trim();
    if (!text) return;
    const acceptedNoteId = this.nextNoteId++;
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
          id: acceptedNoteId,
          text,
          thread: null,
          threadOpen: false,
          threadPending: false,
          persisted: true,
          threadId: null,
          // An accepted draft is a real, linked companion note too (same card).
          saveState: "saving",
        },
      ];
    });
    // Additive: keep manual_notes in sync (enhance/summary reads it).
    this.persistNotes();
    // Route the accepted draft through the SAME companion-note path as a jot.
    void this.appendCompanion(acceptedNoteId, text);
  }

  /**
   * DISMISS an agent turn's note PROPOSAL (the propose-accept "reject"). Only the
   * "✓ Add to notes" affordance is dismissed — the reply text STAYS in the thread
   * (it's still a useful answer); we just drop the proposal (`proposedNote: null`)
   * and mark it `dismissed` so the affordance disappears WITHOUT claiming a save.
   * `dismissed` (not `accepted`) is what makes the surface show "Dismissed" rather
   * than "✓ Added to notes". Nothing enters the notes.
   */
  dismissTurn(noteId: number, agentTurnId: number): void {
    this._notes.update((ns) =>
      ns.map((n) => {
        if (n.id !== noteId || !n.thread) return n;
        return {
          ...n,
          thread: n.thread.map((t) =>
            t.id === agentTurnId
              ? { ...t, dismissed: true, proposedNote: null }
              : t,
          ),
        };
      }),
    );
  }

  /**
   * Fire the manual "Ask AI" trigger: open a fresh anchorless thread to host the
   * voice turn, open the listener, and mark a manual ask in flight so the surface
   * stays visible for the whole round-trip. When `begin_voice_command` itself
   * REJECTS, the listener never armed and no {@link onResult} will ever land to
   * backfill the "🎙 …" placeholder anchor — resolving its turn in place would
   * strand an unlabeled mic bubble in the flow with no dismiss/retry for the rest
   * of the session, so instead we DROP the whole placeholder thread (it never
   * became a real note — `persisted: false`) and just clear the in-flight flag,
   * leaving the flow exactly as it was before the click.
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
      proposedNote: null,
      answeredFrom: null,
      accepted: false,
      dismissed: false,
    };
    const agentTurn: ThreadTurn = {
      id: this.nextTurnId++,
      role: "agent",
      text: "",
      status: "pending",
      trace: [],
      citations: [],
      proposedNote: null,
      answeredFrom: null,
      accepted: false,
      dismissed: false,
    };
    this.voiceTargetNoteId = noteId;
    // A voice thread starts WITHOUT a persistent key (begin_voice_command takes
    // none) — the result payload's threadId is adopted when the answer lands.
    this._notes.update((ns) => [
      ...ns,
      {
        id: noteId,
        text: "🎙 …",
        thread: [userTurn, agentTurn],
        threadOpen: true,
        threadPending: true,
        persisted: false,
        threadId: null,
      },
    ]);
    try {
      await this.ipc.beginVoiceCommand();
    } catch (e) {
      this._manualAskInFlight.set(false);
      this._listening.set(false);
      this.voiceTargetNoteId = null;
      // The listener never started — there is nothing to resolve or retry, so
      // remove the placeholder thread rather than leaving an orphaned "🎙 …"
      // bubble permanently stuck in the flow.
      this._notes.update((ns) => ns.filter((n) => n.id !== noteId));
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
      proposedNote: null,
      answeredFrom: null,
      accepted: false,
      dismissed: false,
    };
    const agentTurn: ThreadTurn = {
      id: this.nextTurnId++,
      role: "agent",
      text: "",
      status: "pending",
      trace: [],
      citations: [],
      proposedNote: null,
      answeredFrom: null,
      accepted: false,
      dismissed: false,
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
        threadId: null,
      },
    ]);
  }

  /**
   * Attach a LIVE tool-trace chip to the in-flight agent turn. When the payload
   * carries a `threadId` (the backend now stamps the originating thread on both
   * tool streams) the chip lands ONLY on that thread's pending agent turn — two
   * simultaneously-pending threads no longer cross-attribute. A stamped id that
   * matches NO thread is a VOICE/wake turn: the backend generates the UUID
   * itself, so the FE-side voice thread still has `threadId: null` while the
   * chips stream (the result hasn't landed yet). That chip ADOPTS: it lands on
   * the pending voice-target thread (else the newest pending thread whose
   * threadId is null — only voice threads qualify; text threads always carry an
   * FE-minted id) and stamps the payload's id onto the note, so later chips +
   * the result match it directly. Only when no null-threadId pending thread
   * exists is a stamped chip DROPPED (never mis-filed onto a text thread).
   * Without a threadId (old backend) the previous fallback applies: the most
   * recent pending agent turn across the flow. A "running" event pushes a new
   * chip; a "done" event resolves the most recent matching running chip (or
   * appends one). No pending agent turn → ignore. Pure immutable signal updates
   * — no NG0600. Shared by the voice (EVENT_ASSISTANT_TOOL) + text
   * (EVENT_CHAT_TOOL) tool-trace streams.
   */
  private onTool(p: AssistantToolPayload): void {
    const payloadThreadId = p.threadId ?? null;
    const voiceTargetId = this.voiceTargetNoteId;
    this._notes.update((ns) => {
      const hasPending = (n: NoteItem): boolean =>
        n.thread?.some((t) => t.role === "agent" && t.status === "pending") ??
        false;

      // Phase 1 — pick the target note (and whether it adopts the payload id).
      let ni = -1;
      let adopt = false;
      if (payloadThreadId !== null) {
        ni = ns.findIndex(
          (n) => n.threadId === payloadThreadId && hasPending(n),
        );
        if (ni === -1) {
          // Voice/wake adoption: prefer the voice-target thread, else the
          // newest pending thread still without a persistent key.
          ni = ns.findIndex(
            (n) =>
              n.id === voiceTargetId && n.threadId === null && hasPending(n),
          );
          if (ni === -1) {
            for (let i = ns.length - 1; i >= 0; i--) {
              if (ns[i].threadId === null && hasPending(ns[i])) {
                ni = i;
                break;
              }
            }
          }
          if (ni !== -1) adopt = true;
        }
      } else {
        for (let i = ns.length - 1; i >= 0; i--) {
          if (hasPending(ns[i])) {
            ni = i;
            break;
          }
        }
      }
      if (ni === -1) return ns; // nothing eligible — drop, never mis-file

      // Phase 2 — attach the chip to the note's last pending agent turn.
      const note = ns[ni];
      const thread = note.thread!;
      let ti = -1;
      for (let i = thread.length - 1; i >= 0; i--) {
        if (thread[i].role === "agent" && thread[i].status === "pending") {
          ti = i;
          break;
        }
      }
      if (ti === -1) return ns; // unreachable (hasPending guaranteed a turn)
      const turn = thread[ti];
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
      const nextThread = thread.slice();
      nextThread[ti] = { ...turn, trace };
      const next = ns.slice();
      next[ni] = {
        ...note,
        thread: nextThread,
        threadId: adopt ? payloadThreadId : note.threadId,
      };
      return next;
    });
  }

  /**
   * A proactive recall hint landed. A hint the user already dismissed this
   * session is dropped (belt and braces over the backend's own dedup);
   * otherwise it REPLACES the visible card — at most one, the backend
   * throttles the stream to ≤1 per cooldown window.
   */
  private onHint(p: ProactiveHintPayload): void {
    if (this.dismissedHints.has(`${p.kind}:${p.targetId}`)) return;
    this._hint.set(p);
    // Piggyback a shadow-count refresh on the (throttled, during-recording) recall
    // stream so the calibration can update mid-meeting without a FE timer.
    void this.refreshShadowCount();
  }

  /** Read the per-recording contradiction SHADOW count (best-effort; count-only, no PII). */
  async refreshShadowCount(): Promise<void> {
    try {
      this._shadowCount.set(await this.ipc.brainReactionsShadowCount());
    } catch {
      // No shadow counter (older backend) — leave the calibration hidden.
    }
  }

  /**
   * Enable the realtime contradiction (⚠ whisper) cards from the shadow-mode
   * calibration prompt, then hide the prompt for this recording. Persists via the
   * dedicated command (not the raw settings save).
   */
  async enableContradictionCards(): Promise<void> {
    this._shadowDismissed.set(true);
    try {
      await this.ipc.setBrainContradictionCards(true);
    } catch {
      // Best-effort — the toggle stays off; the prompt is already hidden.
    }
  }

  /** Dismiss the shadow-mode calibration prompt for this recording (no state change). */
  dismissShadowCalibration(): void {
    this._shadowDismissed.set(true);
  }

  /**
   * A realtime "whisper" contradiction card landed. Prepend it (newest first),
   * dedupe by `entity:predicate:oldQuote` so a repeated contradiction doesn't
   * stack, and cap the rail to {@link MAX_WHISPER_CARDS}.
   */
  private onWhisper(p: WhisperCard): void {
    const key = `${p.entity}:${p.predicate}:${p.oldQuote}`;
    const existing = this._whisperCards();
    if (existing.some((c) => `${c.entity}:${c.predicate}:${c.oldQuote}` === key))
      return;
    const card: RailWhisperCard = { ...p, id: this.nextWhisperId++ };
    this._whisperCards.set(
      [card, ...existing].slice(0, MeetingConversationStore.MAX_WHISPER_CARDS),
    );
  }

  /** Dismiss ONE whisper card by id (a user ✕ — it does not resurface this session). */
  dismissWhisper(id: number): void {
    this._whisperCards.set(this._whisperCards().filter((c) => c.id !== id));
  }

  /**
   * PURGE the entire reactions rail (recall hint + every whisper card) WITHOUT
   * marking anything dismissed. The single teardown used by the screen-share
   * privacy guard ({@link clearHint}), the lock-transition effect, a new
   * recording ({@link clear}) and a meeting change. A card may legitimately
   * resurface later — the backend re-gates visibility on every emit.
   */
  clearRail(): void {
    this._hint.set(null);
    this._whisperCards.set([]);
  }

  /**
   * Hide the visible recall hint (and clear the rest of the rail) WITHOUT marking
   * anything dismissed. Called by the screen-share privacy guard
   * (`ScreenShareService`): the backend has just auto-relocked, and a recall
   * title / contradiction citation from a possibly just-sealed meeting must not
   * linger on the very surface being shared. Broadened from hint-only to the whole
   * rail so the whisper lane is purged on the same screen-share edge (the one
   * existing caller). The same content may legitimately resurface later — the
   * backend re-gates visibility on every emit.
   */
  clearHint(): void {
    this.clearRail();
  }

  /**
   * Dismiss the visible recall hint: hide the card and remember the
   * `kind:targetId` for the rest of the session so it never resurfaces.
   */
  dismissHint(): void {
    const h = this._hint();
    if (!h) return;
    this.dismissedHints.add(`${h.kind}:${h.targetId}`);
    this._hint.set(null);
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
   * When the payload carries a `threadId` that matches a thread with a pending
   * agent turn, the result routes THERE (a text ask fired with a threadId — e.g.
   * `ask_assistant_text` — resolves its own thread even with a voice ask also in
   * flight; a voice thread whose streamed chips already ADOPTED the backend's id
   * in {@link onTool} matches here too). Otherwise it resolves ONLY the
   * voice-originated thread (`voiceTargetNoteId`) — a fresh voice thread whose
   * turn used no tools still has `threadId: null`, lands via that fallback, and
   * ADOPTS the payload's stamped key — a later follow-up then continues the SAME
   * persisted thread. If the target is gone
   * (null — e.g. cleared by a new recording, or a race), we APPEND a fresh
   * anchorless thread for the voice Q&A rather than STEALING the newest pending
   * TEXT thread — clobbering a typed thread's anchor with the heard command would
   * corrupt an unrelated `@brain` conversation.
   */
  private onResult(p: VoiceActionResultPayload): void {
    this._manualAskInFlight.set(false);
    this._listening.set(false);
    this._processing.set(false);
    const payloadThreadId = p.threadId ?? null;
    const heard = p.command.trim();

    let targetId: number | null = null;
    if (payloadThreadId !== null) {
      const match = this._notes().find(
        (n) =>
          n.threadId === payloadThreadId &&
          (n.thread?.some(
            (t) => t.role === "agent" && t.status === "pending",
          ) ??
            false),
      );
      if (match) targetId = match.id;
    }
    if (targetId === null) {
      targetId = this.voiceTargetNoteId;
      this.voiceTargetNoteId = null;
    } else if (targetId === this.voiceTargetNoteId) {
      this.voiceTargetNoteId = null;
    }

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
        proposedNote: null,
        answeredFrom: null,
        accepted: false,
        dismissed: false,
      };
      const agentTurn: ThreadTurn = {
        id: this.nextTurnId++,
        role: "agent",
        text: p.summary,
        status: p.status,
        trace: [],
        citations: parseCitations(p.citations),
        proposedNote: p.proposedNote,
        // Phase 5: the deterministic tier badge (current meeting / vault / connectors), or null.
        answeredFrom: p.answeredFrom ?? null,
        accepted: false,
        dismissed: false,
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
          threadId: payloadThreadId,
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
              proposedNote: p.proposedNote,
              // Phase 5: the deterministic tier badge from the ladder (or null).
              answeredFrom: p.answeredFrom ?? null,
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
          // Adopt the backend-stamped persistent key (a voice thread starts with
          // null) so a follow-up continues the SAME persisted thread.
          threadId: n.threadId ?? payloadThreadId,
        };
      }),
    );
  }
}
