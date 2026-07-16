import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  OnInit,
  afterNextRender,
  computed,
  effect,
  inject,
  input,
  viewChild,
} from "@angular/core";
import { MeetingConversationStore } from "../../../core/meeting-conversation.store";
import { MarkdownComposerComponent } from "../../../design-system/markdown-composer/markdown-composer.component";
import { AiOrbComponent } from "../ai-orb/ai-orb.component";
import { NoteItemComponent } from "../note-item/note-item.component";
import { ProactiveHintCardComponent } from "../proactive-hint-card/proactive-hint-card.component";
import { WhisperCardComponent } from "../whisper-card/whisper-card.component";

/** The inline mention marker that turns a composer line into a `@brain` thread. */
const BRAIN_MARKER = "@brain";

/**
 * Match `@brain` ONLY as a STANDALONE token — preceded by start-or-whitespace AND
 * followed by whitespace-or-end. This is load-bearing for PRIVACY + correctness: a
 * plain note that merely CONTAINS the substring ("bob@brainpower.com",
 * "jane@brainstorm.io", "@brainstorming session") must stay a NOTE (saved as a
 * real companion note, NEVER shipped to `ask_assistant_chat` → no cloud egress, no
 * mid-string corruption). Only a real standalone `@brain` opens a thread. The
 * capture group frames the marker so the QUESTION is exactly the text AFTER the
 * standalone token (never a mid-substring splice).
 */
const BRAIN_TOKEN_RE = /(^|\s)@brain(?=\s|$)/;

/**
 * Resolve a submitted composer line to a `@brain` thread vs a plain note. Returns
 * the QUESTION (everything after the FIRST standalone `@brain`, marker removed,
 * trimmed) when the line carries a standalone `@brain` token, else `null` (→ a
 * plain note, kept verbatim). Anything matching only as a substring
 * ("a@brainx", "x@brain.io") returns `null` and is therefore treated as a note.
 */
export function parseBrainLine(text: string): string | null {
  const m = BRAIN_TOKEN_RE.exec(text);
  if (!m) return null;
  // m.index points at the leading boundary char; the marker starts after it.
  const markerStart = m.index + m[1].length;
  const question = text.slice(markerStart + BRAIN_MARKER.length).trim();
  return question;
}

/**
 * The in-meeting NOTES + `@brain` THREADS surface — the full-height main view of
 * the record screen (Slack-style; the agent PROPOSES, the user ACCEPTS).
 *
 * The MAIN flow is the user's NOTES — a vertical list of note lines that are now
 * REAL, LINKED companion notes (each send appends a block to the meeting's ONE
 * living companion note in the Notes ROOT + renders a "✓ Saved to Notes" card).
 * The ONE composer at the foot is the shared design-system
 * {@link MarkdownComposerComponent} (`/` slash blocks, `[[` link picker, ⌘B/⌘I/⌘1-3,
 * list continuation, Enter = send / Shift+Enter = newline). On send the host splits
 * the emitted markdown by the only signal — a standalone `@brain`:
 *   - a line WITHOUT `@brain` is a plain NOTE → {@link MeetingConversationStore.addNote}
 *     (appended to the flow + persisted to the companion note);
 *   - a line WITH `@brain` OPENS an anchored, multi-turn THREAD (the marker
 *     stripped) → {@link MeetingConversationStore.openThread}, which ships the
 *     thread's history to the agent. Each agent reply offers "✓ Add to notes" — the
 *     only path an accepted draft enters the notes (also as a companion note).
 *
 * The `@brain` hint lives in the composer placeholder + the panel hint copy (the
 * composer owns its own textarea/caret, so the former caret-anchored `@brain`
 * autocomplete popover is replaced by that lightweight equivalent — typing
 * `@brain <q>` and sending still opens a thread). Each note line + its thread
 * renders via {@link NoteItemComponent}.
 *
 * This surface is IN-FLOW (not a floating overlay) — the frosted `.card` is
 * correct here (trap T3 N/A for the panel; the composer's own `/`/`[[` menus float
 * OPAQUE inside it).
 */
@Component({
  selector: "app-meeting-conversation",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    AiOrbComponent,
    MarkdownComposerComponent,
    NoteItemComponent,
    ProactiveHintCardComponent,
    WhisperCardComponent,
  ],
  templateUrl: "./meeting-conversation.component.html",
  styleUrl: "./meeting-conversation.component.scss",
})
export class MeetingConversationComponent implements OnInit {
  protected readonly store = inject(MeetingConversationStore);
  private readonly injector = inject(Injector);
  private readonly flow = viewChild<ElementRef<HTMLElement>>("flow");

  /**
   * The active recording's meeting id (null when there's no meeting yet). Pushed
   * into the store so a note line appends to THIS meeting's companion note +
   * `manual_notes`.
   */
  readonly meetingId = input<string | null>(null);

  /**
   * The `proactiveHintsEnabled` config flag (the record screen passes its config
   * snapshot down). False ⇒ the recall card NEVER renders, even if an event
   * slips through before the backend mute takes effect. Defaults true, matching
   * the backend default.
   */
  readonly hintsEnabled = input<boolean>(true);

  /** ENHANCE-MY-NOTES presentation inputs (pure; all state lives in root stores). */
  readonly enhancing = input(false);
  readonly settled = input(false);
  readonly enhanceAware = input(false);

  /** During the enhance pass the orb shows its shipped 'processing' choreography. */
  readonly orbStateView = computed(() =>
    this.enhancing() ? ("processing" as const) : this.store.orbState(),
  );

  /** Stagger for the one-shot sweep — capped so short summarizes still show a full pass. */
  sweepDelay(i: number): number {
    return Math.min(i, 10) * 180;
  }

  /**
   * The composer placeholder: the notes are still hydrating from `manual_notes`
   * until `loaded()` (the composer is disabled meanwhile — see the store's
   * hydrate-vs-type race note), then a hint that a jot becomes a linked note and
   * `@brain` opens a thread.
   */
  protected readonly composerPlaceholder = computed(() =>
    this.store.loaded()
      ? "Jot a note… / for blocks, [[ to link, @brain to ask"
      : "Loading notes…",
  );

  constructor() {
    // Keep the store pointed at the active meeting so a note line appends to the
    // right meeting's companion note + `manual_notes`. The effect reads the
    // `meetingId` input and the store method writes the store's `_meetingId`
    // signal (signal writes in effects are allowed since Angular 19).
    effect(() => {
      this.store.setMeetingId(this.meetingId());
    });

    // Auto-scroll the flow to the newest line whenever the notes change. Tracks
    // the notes signal in the effect, schedules the DOM work via afterNextRender
    // (zoneless-safe; no signal writes → no NG0600).
    effect(() => {
      this.store.notes();
      afterNextRender(() => this.scrollToBottom(), { injector: this.injector });
    });
  }

  ngOnInit(): void {
    // Subscribe once to the wake/result/tool streams (idempotent). The store is a
    // root singleton, so its subscriptions outlive this component — we don't
    // unlisten on destroy here (the store owns lifetime; cf. RecorderStore).
    void this.store.init();
  }

  /** Scroll the notes flow to its newest content. */
  protected scrollToBottom(): void {
    const el = this.flow()?.nativeElement;
    if (el) el.scrollTop = el.scrollHeight;
  }

  /**
   * A composer line was SENT (Enter with no Shift/menu, or the Send button). The
   * composer self-clears; the host routes the emitted markdown by the ONLY signal —
   * a STANDALONE `@brain` token:
   *   - line WITH a standalone `@brain` → the QUESTION is the text AFTER it; OPEN a
   *     thread (the agent answers in the nested thread; the user accepts what to
   *     keep);
   *   - line WITHOUT a standalone `@brain` → it's a NOTE, kept VERBATIM (append to
   *     the companion note + a flow line). A substring like "bob@brainpower.com"
   *     stays a note — never spliced, never shipped to the agent (no cloud egress).
   * Blank text / not-yet-hydrated is a no-op (the composer also guards non-empty).
   * A note never waits on a thread (notes save while a thread still processes).
   */
  protected onSend(markdown: string): void {
    if (!this.store.loaded()) return; // guard the hydrate-vs-type race (see store)
    const text = markdown.trim();
    if (!text) return;

    const question = parseBrainLine(text);
    if (question !== null) {
      if (!question) return; // bare standalone "@brain" with no question → drop
      void this.store.openThread(question).catch(() => {
        /* the store resolves the agent turn with an error in the thread */
      });
      return;
    }

    // Plain line → a note, kept VERBATIM (saved + shown), independent of any thread.
    this.store.addNote(text);
  }

  /**
   * Esc in the composer with no `/`/`[[` menu open. Nothing to close on this
   * always-visible in-flow surface — kept as a no-op hook so the composer's
   * `escape` output has a defined target (rewire contract).
   */
  protected onEscape(): void {
    /* no floating surface to dismiss here — the panel stays in flow */
  }

  /**
   * CLICK-TO-STOP voice trigger: while listening, stop so the full utterance is
   * dispatched into a thread; otherwise open the listener. Swallow rejections —
   * the store resets its listening/processing/in-flight state on error.
   */
  protected toggleAsk(): void {
    if (this.store.listening()) {
      void this.store.endAsk().catch(() => {
        /* stop failed — store cleared processing/in-flight */
      });
    } else {
      void this.store.askNow().catch(() => {
        /* listener unavailable — store resets the listening/in-flight state */
      });
    }
  }
}
