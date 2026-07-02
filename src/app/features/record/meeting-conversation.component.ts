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
import { MeetingConversationStore } from "../../core/meeting-conversation.store";
import { AiOrbComponent } from "./ai-orb.component";
import { NoteItemComponent } from "./note-item.component";
import { ProactiveHintCardComponent } from "./proactive-hint-card.component";

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
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [AiOrbComponent, NoteItemComponent, ProactiveHintCardComponent],
  template: `
    <div class="card surface" role="group" aria-label="In-meeting notes">
      <div class="surface-head">
        <app-ai-orb class="head-orb" [state]="store.orbState()" />
        <span class="surface-title">Notes</span>
        <span class="surface-hint">
          Type <span class="kbd">&#64;brain</span> to ask in a thread
        </span>
      </div>

      <!-- Proactive recall card — PINNED above the notes flow (not inside the
           scroller, so it can't scroll away mid-meeting). At most ONE: the store
           keeps only the newest; hintsEnabled is the FE half of the global mute
           (the backend silences the event source too — belt and braces). -->
      @if (hintsEnabled() && store.hint(); as h) {
        <app-proactive-hint-card [hint]="h" (dismissed)="store.dismissHint()" />
      }

      <div class="flow" #flow>
        @if (!store.hasNotes()) {
          <p class="flow-empty text-muted">
            Jot your notes here — they're saved with the meeting. Type
            <span class="kbd">&#64;brain</span> + a question to open a thread; the
            assistant answers there and you choose what to add to your notes.
          </p>
        }
        @for (n of store.notes(); track n.id) {
          <app-note-item [note]="n" (followed)="scrollToBottom()" />
        }
      </div>

      <form class="composer" (submit)="submit($event)">
        <button
          type="button"
          class="mic-btn"
          [class.is-listening]="store.listening()"
          [disabled]="store.processing()"
          (click)="toggleAsk()"
          [attr.aria-pressed]="store.listening()"
          [attr.aria-label]="
            store.listening() ? 'Stop listening and ask' : 'Ask by voice'
          "
          [title]="store.listening() ? 'Stop & ask' : 'Ask by voice'"
        >
          <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true">
            <path
              d="M12 3l1.6 4.4L18 9l-4.4 1.6L12 15l-1.6-4.4L6 9l4.4-1.6L12 3z"
              fill="currentColor"
            />
            <path
              d="M18.5 14l.8 2.2 2.2.8-2.2.8-.8 2.2-.8-2.2-2.2-.8 2.2-.8.8-2.2z"
              fill="currentColor"
              opacity="0.85"
            />
          </svg>
        </button>
        <div class="composer-editor">
          <textarea
            #ta
            class="composer-input"
            rows="1"
            autocomplete="off"
            spellcheck="true"
            [disabled]="!store.loaded()"
            [placeholder]="
              store.loaded()
                ? 'pisz notatkę… (@brain to open a thread)'
                : 'Loading notes…'
            "
            [value]="draft()"
            (input)="onInput($event)"
            (keydown)="onKeydown($event)"
            (blur)="onBlur()"
            aria-label="Note or @brain question"
          ></textarea>

          @if (menuOpen()) {
            <div
              class="mention-menu"
              role="listbox"
              aria-label="Mention"
              [style.top.px]="menuTop()"
              [style.left.px]="menuLeft()"
            >
              <button
                type="button"
                class="mention-item"
                role="option"
                [attr.aria-selected]="true"
                (mousedown)="$event.preventDefault()"
                (click)="chooseBrain()"
              >
                <span class="mention-ico" aria-hidden="true">🧠</span>
                <span class="mention-text">
                  <span class="mention-label">brain</span>
                  <span class="mention-hint">open a thread + ask</span>
                </span>
              </button>
            </div>
          }
        </div>
        <button
          type="submit"
          class="btn btn-primary composer-send"
          [class.is-brain]="draftIsBrain()"
          [disabled]="!canSend()"
          [attr.aria-label]="draftIsBrain() ? 'Open a @brain thread' : 'Save note'"
          [title]="draftIsBrain() ? 'Ask (Enter)' : 'Save note (Enter)'"
        >
          <svg
            width="16"
            height="16"
            viewBox="0 0 24 24"
            fill="none"
            aria-hidden="true"
          >
            <path
              d="M5 12h14M13 6l6 6-6 6"
              stroke="currentColor"
              stroke-width="2.2"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </button>
      </form>
    </div>
  `,
  styles: [
    `
      /* The surface fills its host (the record screen makes it the full-height
         main view) so the notes flow grows to the bottom. */
      :host {
        display: block;
        height: 100%;
      }
      .surface {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        height: 100%;
        min-height: 0;
      }
      .surface-head {
        display: flex;
        align-items: center;
        gap: var(--space-2);
      }
      .head-orb {
        --orb-size: 22px;
      }
      .surface-title {
        color: var(--text-primary);
        font-weight: 600;
        font-size: 0.95rem;
      }
      .surface-hint {
        margin-left: auto;
        color: var(--text-muted);
        font-size: 0.78rem;
      }

      /* ── The scrollable notes flow (oldest → newest) ─────────────────────── */
      .flow {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        flex: 1 1 auto;
        min-height: 180px;
        overflow-y: auto;
        padding-right: var(--space-1);
      }
      .flow-empty {
        margin: 0;
        font-size: 0.875rem;
        line-height: 1.55;
      }

      /* inline keycap (head + empty-state hint) */
      .kbd {
        font-family: var(--font-mono, ui-monospace, monospace);
        font-size: 0.74rem;
        padding: 1px 6px;
        border-radius: var(--radius-sm);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
        color: var(--text-secondary);
      }

      /* ── input row: voice mic + text composer, pinned at the foot ───────── */
      .composer {
        display: flex;
        align-items: flex-end;
        gap: var(--space-2);
        flex: none;
      }
      /* Positioned wrapper so the caret-anchored @brain popover can be placed
         relative to the textarea. */
      .composer-editor {
        position: relative;
        flex: 1;
        min-width: 0;
      }

      /* ── @brain mention popover — FLOATS over the composer → OPAQUE (trap T3).
         NOT the frosted .card: a translucent surface would bleed the notes flow
         behind it through (a broken-looking menu). ──────────────────────────── */
      .mention-menu {
        position: absolute;
        z-index: 20;
        min-width: 200px;
        max-width: 260px;
        padding: var(--space-1);
        background: var(--surface-overlay);
        border: 1px solid var(--border-strong);
        border-radius: var(--radius-md);
        box-shadow: var(--shadow-lg);
        -webkit-backdrop-filter: none;
        backdrop-filter: none;
      }
      .mention-item {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        width: 100%;
        padding: var(--space-2) var(--space-3);
        border: none;
        border-radius: var(--radius-sm);
        background: transparent;
        color: var(--text-primary);
        cursor: pointer;
        text-align: left;
        transition: background var(--transition);
      }
      .mention-item:hover,
      .mention-item:focus-visible {
        outline: none;
        background: var(--accent-soft);
      }
      .mention-ico {
        font-size: 1.05rem;
        line-height: 1;
      }
      .mention-text {
        display: flex;
        flex-direction: column;
        gap: 1px;
      }
      .mention-label {
        font-weight: 600;
        font-size: 0.9rem;
      }
      .mention-hint {
        font-size: 0.76rem;
        color: var(--text-muted);
      }
      .mic-btn {
        flex: 0 0 auto;
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 40px;
        height: 40px;
        border: 1px solid var(--accent-ring);
        border-radius: 50%;
        color: var(--accent-hover);
        background: var(--accent-soft);
        cursor: pointer;
        transition:
          transform var(--transition-fast),
          background var(--transition),
          box-shadow var(--transition),
          color var(--transition);
      }
      .mic-btn:hover:not(:disabled) {
        background: var(--accent);
        color: #fff;
        transform: scale(1.05);
      }
      .mic-btn:active:not(:disabled) {
        transform: scale(0.96);
      }
      .mic-btn:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .mic-btn.is-listening {
        background: var(--accent-gradient);
        color: #fff;
        border-color: transparent;
        animation: mic-pulse 1.5s ease-in-out infinite;
      }
      .mic-btn:disabled {
        opacity: 0.55;
        cursor: default;
      }
      @keyframes mic-pulse {
        0%,
        100% {
          box-shadow: 0 0 0 0 var(--accent-ring);
        }
        50% {
          box-shadow: 0 0 0 8px rgba(110, 118, 255, 0);
        }
      }
      .composer-input {
        display: block;
        width: 100%;
        min-height: 40px;
        max-height: 140px;
        padding: var(--space-2) var(--space-3);
        resize: none;
        line-height: 1.45;
        font-size: 0.9rem;
      }
      .composer-send {
        flex: 0 0 auto;
        width: 40px;
        height: 40px;
        padding: 0;
        justify-content: center;
      }
      /* When the line is an @brain ask, the send button reads as the accent
         "ask" affordance (vs the calmer save-note default). */
      .composer-send.is-brain {
        box-shadow: 0 0 0 2px var(--accent-ring);
      }
      .composer-send:disabled {
        opacity: 0.5;
        cursor: default;
      }

      @media (prefers-reduced-motion: reduce) {
        .mic-btn.is-listening {
          animation: none;
        }
      }
    `,
  ],
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
    // store method WRITES the store's `_meetingId` signal → allowSignalWrites is
    // REQUIRED in Angular 18 (trap T1 / NG0600), even though it's a different
    // signal than the one read.
    effect(
      () => {
        this.store.setMeetingId(this.meetingId());
      },
      { allowSignalWrites: true },
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
