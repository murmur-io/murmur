import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
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
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "../../core/ipc.service";
import { DebounceService } from "../../services/debounce.service";
import type { VoiceActionResultPayload } from "../../core/models";

/** Quiet window before a typed edit is autosaved to the backend. */
const AUTOSAVE_MS = 800;
/** The inline mention marker that arms an @brain ask. */
const BRAIN_MARKER = "@brain";

/**
 * "My notes" — the record-screen surface where the user free-types notes DURING
 * a recording (debounced autosave to the canonical buffer via
 * `save_manual_notes`), plus an inline `@brain` affordance: typing `@` at a word
 * boundary opens a caret-anchored OPAQUE popover offering "brain"; selecting it
 * (or typing `@brain `) arms an ask, and Enter on the `@brain <question>` line
 * sends the question to the in-meeting brain (`ask_assistant_text`). The answer —
 * which arrives on the shared `EVENT_VOICE_ACTION_RESULT` stream — is inserted
 * back into the notes as an attributed `> 🧠 …` blockquote (Granola-style: the
 * user's own text stays plain, the AI answer is a distinct quote).
 *
 * Zoneless rules in play here:
 * - the load-on-mount / load-on-meeting-change effect WRITES `draft` after an
 *   await → `{ allowSignalWrites: true }` (trap T1 / NG0600), with a stale-token
 *   guard so a late response for a previous meeting is dropped;
 * - the debounced autosave uses the sanctioned root {@link DebounceService}
 *   tracked-`setTimeout` pattern — NEVER a bare component `setTimeout`;
 * - caret-set / focus after a programmatic draft edit runs in
 *   `afterNextRender(fn, { injector })`, never `setTimeout`/`rAF`;
 * - the `EVENT_VOICE_ACTION_RESULT` subscription is opened ONCE and released on
 *   teardown; its payload lands in signals (never subscribed-into a plain field);
 * - the floating `@brain` popover is the OPAQUE `--surface-overlay` (trap T3),
 *   not the frosted `.card`.
 */
@Component({
  selector: "app-meeting-notes",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <section class="card notes" role="group" aria-label="My notes">
      <div class="notes-head">
        <span class="notes-title">My notes</span>
        @if (statusText(); as s) {
          <span class="notes-status" role="status">{{ s }}</span>
        }
      </div>

      <div class="editor">
        <textarea
          #ta
          class="notes-input"
          rows="5"
          autocomplete="off"
          spellcheck="true"
          placeholder="Jot your own notes here… type &#64;brain to ask the AI"
          [value]="draft()"
          (input)="onInput($event)"
          (keydown)="onKeydown($event)"
          (blur)="onBlur()"
          aria-label="Meeting notes"
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
                <span class="mention-hint">ask the meeting brain</span>
              </span>
            </button>
          </div>
        }
      </div>

      @if (pendingQuestion(); as q) {
        <p class="brain-pending" role="status">
          <span class="brain-spin" aria-hidden="true"></span>
          Asking the brain — <span class="brain-q">{{ q }}</span>
        </p>
      } @else {
        <p class="notes-foot text-muted">
          Type <span class="kbd">&#64;brain</span> + a question, then press Enter —
          the answer is added to your notes.
        </p>
      }
    </section>
  `,
  styles: [
    `
      .notes {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .notes-head {
        display: flex;
        align-items: center;
        gap: var(--space-2);
      }
      .notes-title {
        color: var(--text-primary);
        font-weight: 600;
        font-size: 0.95rem;
      }
      .notes-status {
        margin-left: auto;
        font-size: 0.78rem;
        color: var(--text-muted);
        font-variant-numeric: tabular-nums;
      }

      /* The textarea lives in a positioned wrapper so the caret-anchored popover
         can be absolutely placed relative to it. */
      .editor {
        position: relative;
      }
      .notes-input {
        width: 100%;
        min-height: 120px;
        max-height: 340px;
        padding: var(--space-3);
        resize: vertical;
        line-height: 1.55;
        font-size: 0.92rem;
      }

      /* ── @brain mention popover — FLOATS over the editor → OPAQUE (trap T3).
         NOT the frosted .card: a translucent surface would bleed the notes
         behind it through (a broken-looking menu). */
      .mention-menu {
        position: absolute;
        z-index: 20;
        min-width: 220px;
        max-width: 280px;
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

      /* ── footer: the @brain hint + the pending indicator (muted/italic) ───── */
      .notes-foot {
        margin: 0;
        font-size: 0.8rem;
        line-height: 1.5;
      }
      .kbd {
        font-family: var(--font-mono, ui-monospace, monospace);
        font-size: 0.74rem;
        padding: 1px 6px;
        border-radius: var(--radius-sm);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
        color: var(--text-secondary);
      }
      .brain-pending {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        margin: 0;
        font-size: 0.82rem;
        font-style: italic;
        color: var(--text-secondary);
      }
      .brain-q {
        color: var(--text-muted);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        max-width: 60%;
      }
      .brain-spin {
        flex: 0 0 auto;
        width: 11px;
        height: 11px;
        border-radius: 50%;
        border: 1.6px solid var(--accent-ring);
        border-top-color: var(--accent);
        animation: notes-spin 0.7s linear infinite;
      }
      @keyframes notes-spin {
        to {
          transform: rotate(360deg);
        }
      }
      @media (prefers-reduced-motion: reduce) {
        .brain-spin {
          animation: none;
        }
      }
    `,
  ],
})
export class MeetingNotesComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly autosave = inject(DebounceService);
  private readonly injector = inject(Injector);
  private readonly destroyRef = inject(DestroyRef);

  /** The active recording's meeting id (null when there's no meeting yet). */
  readonly meetingId = input<string | null>(null);

  private readonly textarea =
    viewChild<ElementRef<HTMLTextAreaElement>>("ta");

  /** The notes buffer — the single source of truth the textarea binds to. */
  protected readonly draft = signal("");

  /** Autosave status line ("Saving…" → "Saved"). */
  protected readonly saveState = signal<"idle" | "saving" | "saved">("idle");
  protected readonly statusText = computed(() => {
    switch (this.saveState()) {
      case "saving":
        return "Saving…";
      case "saved":
        return "Saved";
      default:
        return "";
    }
  });

  /** Whether the @brain mention popover is open + its caret-anchored position. */
  protected readonly menuOpen = signal(false);
  protected readonly menuTop = signal(0);
  protected readonly menuLeft = signal(0);
  /** Index of the `@` that opened the current mention (replaced on select). */
  private atIndex = -1;

  /**
   * The in-flight @brain question (null when none). Doubles as the correlation
   * guard: a result is only inserted when its echoed `command` matches this, so
   * an unrelated voice/assistant result on the shared stream never double-inserts.
   */
  protected readonly pendingQuestion = signal<string | null>(null);

  /** Monotonic token so a late load for a previous meeting is dropped (stale guard). */
  private loadToken = 0;
  /** Released on teardown; set once the result subscription is live. */
  private unlistenResult: UnlistenFn | null = null;
  private destroyed = false;

  constructor() {
    this.destroyRef.onDestroy(() => {
      this.destroyed = true;
      this.unlistenResult?.();
      this.unlistenResult = null;
      // NOTE: a still-pending autosave is intentionally NOT cancelled here — the
      // DebounceService is root-scoped + keyed per meeting, so the final edit
      // flushes even after this component unmounts on recording-stop.
    });

    // Load the saved buffer on mount AND whenever the active meeting id changes.
    // Writes `draft` after an await → allowSignalWrites REQUIRED (trap T1 /
    // NG0600). A bumped token drops a response that arrives after the meeting
    // changed mid-flight.
    effect(
      () => {
        const id = this.meetingId();
        const token = ++this.loadToken;
        this.menuOpen.set(false);
        if (!id) {
          this.draft.set("");
          this.saveState.set("idle");
          return;
        }
        // Don't let a same-id save still pending from before clobber the load.
        this.autosave.cancel(this.saveKey(id));
        void this.load(id, token);
      },
      { allowSignalWrites: true },
    );
  }

  ngOnInit(): void {
    void this.subscribeResult();
  }

  /** Subscribe ONCE to the shared voice-action-result stream (released on teardown). */
  private async subscribeResult(): Promise<void> {
    const un = await this.ipc.onVoiceActionResult((p) => this.onActionResult(p));
    if (this.destroyed) {
      un();
      return;
    }
    this.unlistenResult = un;
  }

  private saveKey(id: string): string {
    return `manual-notes:${id}`;
  }

  /** Fetch the persisted buffer; drop the result if the meeting changed since. */
  private async load(id: string, token: number): Promise<void> {
    try {
      const text = await this.ipc.getManualNotes(id);
      if (token !== this.loadToken) return;
      this.draft.set(text);
      this.saveState.set("idle");
    } catch {
      if (token !== this.loadToken) return;
      this.draft.set("");
      this.saveState.set("idle");
    }
  }

  /** Textarea input: update the draft, (re)arm autosave, recompute the @brain menu. */
  protected onInput(event: Event): void {
    const ta = event.target as HTMLTextAreaElement;
    this.draft.set(ta.value);
    this.scheduleSave();
    this.updateMentionMenu(ta);
  }

  /** Debounced autosave — captures the (id, text) so a meeting switch can't misroute it. */
  private scheduleSave(): void {
    const id = this.meetingId();
    if (!id) return;
    const text = this.draft();
    this.saveState.set("saving");
    this.autosave.schedule(this.saveKey(id), () => void this.flush(id, text), AUTOSAVE_MS);
  }

  private async flush(id: string, text: string): Promise<void> {
    try {
      await this.ipc.saveManualNotes(id, text);
      if (this.meetingId() === id) this.saveState.set("saved");
    } catch {
      // Locked/sealed or a transient backend error — keep the local draft, drop
      // the "Saving…" label. Never noisy; the buffer stays in the editor.
      if (this.meetingId() === id) this.saveState.set("idle");
    }
  }

  /** Keyboard: drive the mention menu, then the @brain submit, else default. */
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
    // Enter (no Shift) on a `@brain <question>` line submits the ask.
    if (event.key === "Enter" && !event.shiftKey) {
      this.trySubmitBrain(event);
    }
  }

  /** Close the menu when focus leaves the textarea (click-away). */
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
    // Only letters, and a prefix of "brain" ("", "b", "br", … "brain").
    if (!/^[a-z]*$/i.test(query) || !"brain".startsWith(query.toLowerCase())) {
      this.menuOpen.set(false);
      return;
    }
    this.atIndex = at;
    const anchor = this.caretAnchor(ta, caret);
    const left = Math.max(
      0,
      Math.min(anchor.left, ta.clientWidth - 220),
    );
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
    this.scheduleSave();
    this.focusAt(pos);
  }

  /**
   * Submit the @brain ask on the caret's line. Captures the text after `@brain`,
   * strips the marker (leaving the question as the user's own note line), fires
   * the ask, and arms the correlation guard. No-op (returns, default newline) when
   * the line has no `@brain <question>` or an ask is already in flight.
   */
  private trySubmitBrain(event: KeyboardEvent): void {
    if (this.pendingQuestion()) return; // one in flight → let Enter add a newline
    const ta = this.textarea()?.nativeElement;
    if (!ta) return;
    const value = this.draft();
    const caret = ta.selectionStart ?? value.length;
    const lineStart = value.lastIndexOf("\n", caret - 1) + 1;
    let lineEnd = value.indexOf("\n", caret);
    if (lineEnd === -1) lineEnd = value.length;
    const line = value.slice(lineStart, lineEnd);
    const markerIdx = line.indexOf(BRAIN_MARKER);
    if (markerIdx === -1) return;
    const question = line.slice(markerIdx + BRAIN_MARKER.length).trim();
    if (!question) return; // bare "@brain" with no question → default newline
    event.preventDefault();

    // Strip the marker → keep just the question as the user's plain note line.
    const newLine = line.slice(0, markerIdx) + question;
    const next = value.slice(0, lineStart) + newLine + value.slice(lineEnd);
    const pos = lineStart + newLine.length;
    this.draft.set(next);
    this.scheduleSave();
    this.focusAt(pos);
    this.fireBrain(question);
  }

  /** Dispatch the @brain question to the shared in-meeting brain. */
  private fireBrain(question: string): void {
    this.pendingQuestion.set(question);
    void this.ipc.askAssistantText(question).catch(() => {
      // Dispatch failed (empty/backed-off) → clear the guard so a retry is possible.
      this.pendingQuestion.set(null);
    });
  }

  /**
   * A voice-action result landed on the shared stream. Insert it ONLY when it
   * answers OUR pending @brain ask (its echoed `command` matches) — the
   * correlation guard so an unrelated assistant/voice result never double-inserts.
   * The answer goes in as an attributed `> 🧠 …` blockquote, then autosaves.
   */
  private onActionResult(p: VoiceActionResultPayload): void {
    const pending = this.pendingQuestion();
    if (!pending) return;
    if (p.command.trim() !== pending) return; // a stray result → ignore, stay pending
    this.pendingQuestion.set(null);
    const answer = (p.summary ?? "").trim();
    if (!answer) return; // nothing_heard / empty → nothing to attribute
    const block = `\n> 🧠 ${answer}\n`;
    this.draft.update((d) => d + block);
    this.scheduleSave();
  }

  /** Focus the textarea and place the caret at `pos` after the DOM updates. */
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
}
