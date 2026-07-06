import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  inject,
  input,
  signal,
  viewChild,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import type { ChatTurn } from "../../../core/models";
import { MarkdownComponent } from "../../../shared/markdown/markdown.component";

/** The starter prompts shown in the empty state — tap to ask immediately. */
const STARTERS: readonly string[] = [
  "Summarize the key decisions",
  "What are my action items?",
  "What questions were left open?",
];

/**
 * "Chat with this meeting" — a grounded Q&A panel over a single meeting's
 * transcript. It is a presentational sibling of the timeline + analysis cards:
 * the parent owns the meeting; this component owns only the conversation it
 * builds via {@link IpcService.chatMeeting}, which answers strictly from that
 * meeting's transcript.
 *
 * Lives in its own file so its inline styles get their own per-component
 * `anyComponentStyle` budget (the detail component's styles are near the cap).
 *
 * Assistant replies are rendered as PLAIN TEXT with `white-space: pre-wrap`
 * (no markdown lib, no innerHTML/DomSanitizer) — line breaks + spacing from the
 * model are preserved verbatim and safely.
 */
@Component({
  selector: "app-meeting-chat",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MarkdownComponent],
  templateUrl: "./meeting-chat.component.html",
  styleUrl: "./meeting-chat.component.scss",
})
export class MeetingChatComponent {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);

  /** The meeting whose transcript grounds every answer. */
  readonly meetingId = input.required<string>();

  /** The running conversation (optimistic user turns + grounded replies). */
  readonly conversation = signal<ChatTurn[]>([]);
  /** True while a {@link IpcService.chatMeeting} call is in flight. */
  readonly pending = signal(false);
  /** Inline error message (with a Retry affordance); null when clear. */
  readonly error = signal<string | null>(null);
  /** Working copy of the composer text (textarea (input) → signal). */
  readonly draft = signal("");

  /** Starter prompts for the empty state. */
  protected readonly starters = STARTERS;

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
   * Ask the current question. Captures the question + the PRIOR history
   * (the conversation before this turn), optimistically appends the user turn,
   * awaits the grounded reply, then appends the assistant turn. On failure the
   * user's question is kept (an inline Retry re-runs it).
   */
  async send(): Promise<void> {
    const question = this.draft().trim();
    if (!question || this.pending()) {
      return;
    }

    // History as seen by the model = everything BEFORE this turn.
    const priorHistory = this.conversation();

    this.error.set(null);
    this.draft.set("");
    this.conversation.set([
      ...priorHistory,
      { role: "user", content: question },
    ]);
    this.pending.set(true);
    this.scrollToLatest();

    try {
      const answer = await this.ipc.chatMeeting(
        this.meetingId(),
        question,
        priorHistory,
      );
      this.conversation.update((turns) => [
        ...turns,
        { role: "assistant", content: answer },
      ]);
    } catch (e) {
      // Keep the user's question in the log so Retry can re-send it.
      this.error.set("Couldn’t get an answer: " + String(e));
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
   * injector is a one-shot and is auto-torn-down when the component is
   * destroyed, so there is nothing to clean up manually.
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
