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
  signal,
  viewChild,
} from "@angular/core";
import { MeetingConversationStore } from "../../../core/meeting-conversation.store";
import { AiOrbComponent } from "../ai-orb/ai-orb.component";
import { NoteItemComponent } from "../note-item/note-item.component";
import { ProactiveHintCardComponent } from "../proactive-hint-card/proactive-hint-card.component";
import { WhisperCardComponent } from "../whisper-card/whisper-card.component";

/** The inline mention marker that turns a composer line into a `@brain` thread. */
const BRAIN_MARKER = "@brain";

/**
 * Match `@brain` ONLY as a STANDALONE token — preceded by start-or-whitespace AND
 * followed by whitespace-or-end — mirroring the popover's `/\s/.test(prev)` word-
 * boundary check. This is load-bearing for PRIVACY + correctness: a plain note
 * that merely CONTAINS the substring ("bob@brainpower.com", "jane@brainstorm.io",
 * "@brainstorming session") must stay a NOTE (saved verbatim, NEVER shipped to
 * `ask_assistant_chat` → no cloud egress, no mid-string corruption). Only a real
 * standalone `@brain` opens a thread. The capture groups frame the marker so the
 * QUESTION is exactly the text AFTER the standalone token (never a mid-substring
 * splice). `g` so we can scan for the FIRST standalone occurrence.
 */
const BRAIN_TOKEN_RE = /(^|\s)@brain(?=\s|$)/;

/**
 * Resolve a composer line to a `@brain` thread vs a plain note. Returns the
 * QUESTION (everything after the FIRST standalone `@brain`, marker removed,
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
 * The MAIN flow is the user's NOTES — a vertical list of note lines persisted to
 * `manual_notes`. The ONE composer at the foot splits a submitted line by the
 * only signal — `@brain`:
 *   - a line WITHOUT `@brain` is a plain NOTE → {@link MeetingConversationStore.addNote}
 *     (appended to the flow + saved);
 *   - a line WITH `@brain` OPENS an anchored, multi-turn THREAD under a new note
 *     line → {@link MeetingConversationStore.openThread} (the marker stripped),
 *     which ships the thread's history to the agent. Each agent reply offers
 *     "✓ Add to notes" — the only path content enters the notes.
 *
 * The `@brain` autocomplete (typing `@` at a word boundary → an OPAQUE caret-
 * anchored popover offering "brain", trap T3) + the keyed-debounce service are
 * preserved. Each note line + its thread renders via {@link NoteItemComponent}
 * (split out so the per-component style budgets stay well under 16 kB).
 *
 * This surface is IN-FLOW (not a floating overlay) — the frosted `.card` is
 * correct here; only the `@brain` popover floats → it uses `var(--surface-overlay)`.
 */
@Component({
  selector: "app-meeting-conversation",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    AiOrbComponent,
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
  private readonly textarea = viewChild<ElementRef<HTMLTextAreaElement>>("ta");

  /**
   * The active recording's meeting id (null when there's no meeting yet). Pushed
   * into the store so a note line appends to THIS meeting's `manual_notes`.
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

  /** The composer draft (signal-backed — zoneless). */
  protected readonly draft = signal("");

  /**
   * True when the current line carries a STANDALONE `@brain` token (drives the
   * send-button accent). Uses the word-boundary matcher — a substring like
   * "bob@brainpower.com" is NOT an ask, so the button reads as "save note".
   */
  protected readonly draftIsBrain = computed(
    () => BRAIN_TOKEN_RE.test(this.draft()),
  );

  /**
   * Submit is allowed when there's non-blank text AND the meeting's notes have
   * finished hydrating from `manual_notes`. Gating on `loaded()` closes the
   * hydrate-vs-type race: a note submitted before `getManualNotes` resolves would
   * otherwise overwrite the server buffer with just the fresh line, then skip
   * hydration (length > 0) → the pre-existing server notes would be lost.
   */
  protected readonly canSend = computed(
    () => this.store.loaded() && this.draft().trim().length > 0,
  );

  /** Whether the @brain mention popover is open + its caret-anchored position. */
  protected readonly menuOpen = signal(false);
  protected readonly menuTop = signal(0);
  protected readonly menuLeft = signal(0);
  /** Index of the `@` that opened the current mention (replaced on select). */
  private atIndex = -1;

  constructor() {
    // Keep the store pointed at the active meeting so a note line appends to the
    // right `manual_notes` buffer. The effect reads the `meetingId` input and the
    // store method writes the store's `_meetingId` signal (signal writes in
    // effects are allowed since Angular 19).
    effect(
      () => {
        this.store.setMeetingId(this.meetingId());
      },
    );

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
   * Submit the composer line. The ONLY signal is a STANDALONE `@brain` token:
   *   - line WITH a standalone `@brain` → the QUESTION is the text AFTER it; OPEN
   *     a thread (the agent answers in the nested thread; the user accepts what to
   *     keep);
   *   - line WITHOUT a standalone `@brain` → it's a NOTE, kept VERBATIM (persist to
   *     `manual_notes` + a flow line). A substring like "bob@brainpower.com" stays
   *     a note — never spliced, never shipped to the agent (no cloud egress).
   * Blank text / not-yet-hydrated is a no-op. A note never waits on a thread
   * (notes save while a thread is still processing — threads are independent).
   */
  protected submit(event: Event): void {
    event.preventDefault();
    this.menuOpen.set(false);
    if (!this.store.loaded()) return; // guard the hydrate-vs-type race (see store)
    const text = this.draft().trim();
    if (!text) return;

    const question = parseBrainLine(text);
    if (question !== null) {
      if (!question) return; // bare standalone "@brain" with no question → keep draft
      this.draft.set("");
      void this.store.openThread(question).catch(() => {
        /* the store resolves the agent turn with an error in the thread */
      });
      return;
    }

    // Plain line → a note, kept VERBATIM (saved + shown), independent of any thread.
    this.draft.set("");
    this.store.addNote(text);
  }

  /**
   * Keyboard on the composer: drive the @brain mention popover first (Esc closes,
   * Enter/Tab selects), else Enter (no Shift) submits the line.
   */
  protected onKeydown(event: KeyboardEvent): void {
    if (this.menuOpen()) {
      if (event.key === "Escape") {
        event.preventDefault();
        this.menuOpen.set(false);
        return;
      }
      if (event.key === "Enter" || event.key === "Tab") {
        event.preventDefault();
        this.chooseBrain();
        return;
      }
      return;
    }
    if (event.key === "Enter" && !event.shiftKey) {
      this.submit(event);
    }
  }

  /** Composer input: update the draft + recompute the @brain mention popover. */
  protected onInput(event: Event): void {
    const ta = event.target as HTMLTextAreaElement;
    this.draft.set(ta.value);
    this.updateMentionMenu(ta);
  }

  /** Close the mention menu when focus leaves the textarea (click-away). */
  protected onBlur(): void {
    this.menuOpen.set(false);
  }

  /**
   * Re-evaluate the @brain popover from the live caret. Open it when the caret
   * sits inside an `@`-token at a word boundary whose text is a prefix of
   * "brain"; otherwise close it. Anchors the popover just below the caret.
   */
  private updateMentionMenu(ta: HTMLTextAreaElement): void {
    const value = ta.value;
    const caret = ta.selectionStart ?? value.length;
    let at = -1;
    for (let i = caret - 1; i >= 0; i--) {
      const ch = value[i];
      if (ch === "@") {
        const prev = i === 0 ? " " : value[i - 1];
        if (/\s/.test(prev)) at = i;
        break;
      }
      if (/\s/.test(ch)) break; // whitespace before any '@' → not in a token
    }
    if (at === -1) {
      this.menuOpen.set(false);
      return;
    }
    const query = value.slice(at + 1, caret);
    if (!/^[a-z]*$/i.test(query) || !"brain".startsWith(query.toLowerCase())) {
      this.menuOpen.set(false);
      return;
    }
    this.atIndex = at;
    const anchor = this.caretAnchor(ta, caret);
    const left = Math.max(0, Math.min(anchor.left, ta.clientWidth - 200));
    this.menuTop.set(ta.offsetTop + anchor.top + anchor.height);
    this.menuLeft.set(ta.offsetLeft + left);
    this.menuOpen.set(true);
  }

  /** Select "brain": replace the typed `@…` token with `@brain ` + re-focus. */
  protected chooseBrain(): void {
    const ta = this.textarea()?.nativeElement;
    if (!ta || this.atIndex < 0) {
      this.menuOpen.set(false);
      return;
    }
    const value = this.draft();
    const caret = ta.selectionStart ?? value.length;
    const before = value.slice(0, this.atIndex);
    const after = value.slice(caret);
    const insert = `${BRAIN_MARKER} `;
    const next = before + insert + after;
    const pos = before.length + insert.length;
    this.menuOpen.set(false);
    this.draft.set(next);
    this.focusAt(pos);
  }

  /** Focus the textarea + place the caret at `pos` after the DOM updates. */
  private focusAt(pos: number): void {
    afterNextRender(
      () => {
        const ta = this.textarea()?.nativeElement;
        if (!ta) return;
        ta.focus();
        ta.setSelectionRange(pos, pos);
      },
      { injector: this.injector },
    );
  }

  /**
   * Caret pixel position inside a textarea via the mirror-div technique: clone
   * the textarea's box + text up to the caret into a hidden div, measure a marker
   * span. Returns coords relative to the textarea's border box + the line height.
   */
  private caretAnchor(
    ta: HTMLTextAreaElement,
    caret: number,
  ): { top: number; left: number; height: number } {
    const style = getComputedStyle(ta);
    const div = document.createElement("div");
    const ds = div.style;
    ds.position = "absolute";
    ds.visibility = "hidden";
    ds.whiteSpace = "pre-wrap";
    ds.overflowWrap = "break-word";
    ds.overflow = "hidden";
    const copy = [
      "box-sizing",
      "padding-top",
      "padding-right",
      "padding-bottom",
      "padding-left",
      "border-top-width",
      "border-right-width",
      "border-bottom-width",
      "border-left-width",
      "font-family",
      "font-size",
      "font-weight",
      "font-style",
      "font-variant",
      "letter-spacing",
      "line-height",
      "text-transform",
      "word-spacing",
      "text-indent",
      "tab-size",
    ];
    for (const p of copy) ds.setProperty(p, style.getPropertyValue(p));
    ds.width = `${ta.clientWidth}px`;
    ds.height = "auto";
    const value = ta.value;
    div.textContent = value.slice(0, caret);
    const marker = document.createElement("span");
    marker.textContent = value.slice(caret) || ".";
    div.appendChild(marker);
    document.body.appendChild(div);
    const lhRaw = parseFloat(style.lineHeight);
    const height = Number.isNaN(lhRaw)
      ? parseFloat(style.fontSize) * 1.4
      : lhRaw;
    const top =
      marker.offsetTop + parseFloat(style.borderTopWidth || "0") - ta.scrollTop;
    const left =
      marker.offsetLeft +
      parseFloat(style.borderLeftWidth || "0") -
      ta.scrollLeft;
    document.body.removeChild(div);
    return { top, left, height };
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
