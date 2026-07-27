import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
  viewChild,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import type { ChatTurn, SourceRef } from "../../../core/models";
import { SourceScopeService } from "../../../services/source-scope.service";
import { SourcePickerComponent } from "../../../design-system/source-picker/source-picker.component";
import { MarkdownComponent } from "../../../shared/markdown/markdown.component";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";

/** The starter prompts shown in the empty state — tap to ask immediately. */
const STARTERS: readonly string[] = [
  "Summarize this note",
  "What's missing?",
  "Find related meetings",
];

/**
 * "Ask about this note" — a grounded Q&A panel anchored to a single authored
 * note (Brain v3, source-scoped Brain PR-4). A presentational TWIN of
 * {@link MeetingChatComponent}: the same conversation/draft/pending/error/
 * canSend/send/retry/clear/auto-scroll idioms and MarkdownComponent replies.
 *
 * The one shape difference from the meeting twin is grounding: it answers via
 * {@link IpcService.askVault} PINNED to this note's source scope (the note +
 * its active links, pre-filled into the `<mur-source-picker>`), rather than a
 * per-meeting transcript chat. Like meeting-chat it is STATELESS — no thread
 * persistence — so `askVault`'s `askThreadId` is left `undefined` and the FULL
 * prior conversation is re-sent as history each turn.
 *
 * Lives in its own file so its scoped styles get their own per-component
 * `anyComponentStyle` budget.
 */
@Component({
  selector: "app-note-chat",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MarkdownComponent, SourcePickerComponent],
  templateUrl: "./note-chat.component.html",
  styleUrl: "./note-chat.component.scss",
})
export class NoteChatComponent {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);
  private readonly sourceScope = inject(SourceScopeService);
  private readonly errorCopy = inject(ErrorCopyService);

  /** The note whose content grounds every answer (the pre-fill anchor). */
  readonly noteId = input.required<string>();
  /**
   * The note's display title — the label of the anchor chip pre-filled into the
   * picker (falls back to the id when absent). Display-only; identity is
   * `kind + id`.
   */
  readonly anchorTitle = input<string | null>(null);

  /**
   * The anchor's KIND. "note" (default) grounds via the local note's source scope, prefilled into
   * the picker. "org" grounds a read-only SHARED (org-feed) note (the org-item viewer): the item is
   * pinned server-side via `pinnedOrgItemId` — an org item is NOT a valid local {@link SourceRef},
   * so we do NOT prefill a (wrong) note scope; the picker starts empty and the user may still add
   * their own notes/meetings as extra scope.
   */
  readonly anchorKind = input<"note" | "org">("note");

  /**
   * BARE mode (docked in the note-editor's right drawer): drop the frosted `.card` frame + aurora
   * glow and FILL the host height, so the drawer is ONE coherent surface (no card-in-panel double
   * border) with the composer pinned to the bottom. Defaults false ⇒ the standalone card look.
   */
  readonly bare = input(false);

  /**
   * Show a close (×) affordance in the header band — set by the drawer host so the chat OWNS its
   * own header (title + close) as a single aligned pane header, rather than a floating corner
   * button. Pressing it emits {@link close}; the host toggles the drawer shut.
   */
  readonly showClose = input(false);
  /** Emitted when the header × is pressed. The drawer host closes itself. (Not `close` — that
   *  collides with the native DOM event name and trips `@angular-eslint/no-output-native`.) */
  readonly closed = output<void>();

  /** The running conversation (optimistic user turns + grounded replies). */
  readonly conversation = signal<ChatTurn[]>([]);
  /** True while an {@link IpcService.askVault} call is in flight. */
  readonly pending = signal(false);
  /** Inline error message (with a Retry affordance); null when clear. */
  readonly error = signal<string | null>(null);
  /** Working copy of the composer text (textarea (input) → signal). */
  readonly draft = signal("");

  /**
   * Source-scoped Brain — the `<mur-source-picker>` selection (this note + its
   * active links, pre-filled on load). A NON-empty selection PINS the answer to
   * exactly those sources + their links; `send()` always passes it (the note
   * itself is one of the sources, so the scope is never whole-vault by default).
   */
  readonly sources = signal<SourceRef[]>([]);

  /** Starter prompts for the empty state. */
  protected readonly starters = STARTERS;

  /**
   * Pre-fill the picker with the default scope for THIS note (the note itself +
   * its active linked neighbours) whenever the note id changes. A legitimate
   * signal-writing IPC effect (T1): keyed on `noteId()`, with a monotonic
   * stale-result guard so a late reply for a superseded note is dropped.
   */
  private prefillSeq = 0;
  private readonly _prefill = effect(() => {
    const kind = this.anchorKind();
    const id = this.noteId();
    const title = this.anchorTitle() ?? undefined;
    const seq = ++this.prefillSeq;
    if (kind !== "note") {
      // Org anchor: pinned server-side via `pinnedOrgItemId`; an org item is not a local SourceRef,
      // so prefilling a note scope for its id would be wrong. Start the picker empty.
      this.sources.set([]);
      return;
    }
    void this.sourceScope.defaultSources("note", id, title).then((defaults) => {
      if (seq === this.prefillSeq) {
        this.sources.set(defaults);
      }
    });
  });

  /** A submit is allowed only with non-empty text and no in-flight request. */
  readonly canSend = computed(
    () => !this.pending() && this.draft().trim().length > 0,
  );

  /** Mirror the textarea value into the `draft` signal. */
  onDraftInput(event: Event): void {
    this.draft.set((event.target as HTMLTextAreaElement).value);
  }

  /** Enter sends; Shift+Enter inserts a newline (textarea default). */
  onKeydown(event: KeyboardEvent): void {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      void this.send();
    }
  }

  /** Composer form submit (Send button / Enter). */
  onSubmit(event: Event): void {
    event.preventDefault();
    void this.send();
  }

  /** Fill + send a starter chip's question. */
  ask(question: string): void {
    if (this.pending()) {
      return;
    }
    this.draft.set(question);
    void this.send();
  }

  /** Re-send the last user question after an error (it's still in the log). */
  retry(): void {
    if (this.pending()) {
      return;
    }
    const turns = this.conversation();
    const last = turns[turns.length - 1];
    if (last?.role !== "user") {
      this.error.set(null);
      return;
    }
    // Pop the dangling user turn back into the composer, then re-run send()
    // so it captures the correct prior history.
    this.conversation.set(turns.slice(0, -1));
    this.draft.set(last.content);
    void this.send();
  }

  /** Clear the whole conversation back to the empty state. */
  clear(): void {
    if (this.pending()) {
      return;
    }
    this.conversation.set([]);
    this.error.set(null);
  }

  /**
   * Ask the current question, grounded in this note's source scope. Captures the
   * question + the PRIOR history (the conversation before this turn),
   * optimistically appends the user turn, awaits the grounded reply, then
   * appends the assistant turn. On failure the user's question is kept (an inline
   * Retry re-runs it). Stateless like the meeting twin: no `askThreadId`.
   */
  async send(): Promise<void> {
    const question = this.draft().trim();
    if (!question || this.pending()) {
      return;
    }

    // History as seen by the model = everything BEFORE this turn.
    const priorHistory = this.conversation();
    // Source-scoped Brain: pin the answer to this note + its links.
    const scope = this.sources();

    this.error.set(null);
    this.draft.set("");
    this.conversation.set([
      ...priorHistory,
      { role: "user", content: question },
    ]);
    this.pending.set(true);
    this.scrollToLatest();

    try {
      const result = await this.ipc.askVault(
        question,
        priorHistory,
        undefined,
        scope.length ? scope : undefined,
        // Org anchor: pin the shared note server-side so the answer is always grounded in it
        // (works for the local Brain too, which otherwise never retrieves org-feed content).
        this.anchorKind() === "org" ? this.noteId() : undefined,
      );
      this.conversation.update((turns) => [
        ...turns,
        { role: "assistant", content: result.answer },
      ]);
    } catch (e) {
      // Keep the user's question in the log so Retry can re-send it.
      this.error.set(this.errorCopy.because("Couldn’t get an answer", e));
    } finally {
      this.pending.set(false);
      this.scrollToLatest();
    }
  }

  // --- Auto-scroll ---------------------------------------------------------

  /** The scrollable message log. */
  private readonly scroller = viewChild<ElementRef<HTMLDivElement>>("scroller");

  /**
   * Pin the log to the newest message. Runs after the next render so the new
   * row/typing indicator is laid out before we measure scrollHeight — zoneless
   * safe, no setTimeout. afterNextRender registered with this component's
   * injector is a one-shot torn down with the component, so there is nothing to
   * clean up manually.
   */
  private scrollToLatest(): void {
    afterNextRender(
      () => {
        const el = this.scroller()?.nativeElement;
        if (el) {
          el.scrollTop = el.scrollHeight;
        }
      },
      { injector: this.injector },
    );
  }
}
