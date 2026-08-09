import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
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
import type {
  AskConversation,
  AskConversationScope,
  AskConversationSummary,
  ChatTurn,
  SourceRef,
} from "../../../core/models";
import { SourceScopeService } from "../../../services/source-scope.service";
import { SourcePickerComponent } from "../../../design-system/source-picker/source-picker.component";
import { MarkdownComponent } from "../../../shared/markdown/markdown.component";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";
import { ChatHistoryComponent } from "../../../design-system/chat-history/chat-history.component";
import { FoldersService } from "../../../services/folders.service";
import { MurIconComponent } from "../../../design-system/icon/icon.component";
import { NotesService } from "../../../services/notes.service";
import { AskHistoryPrivacyBarrierService } from "../../../core/ask-history-privacy-barrier.service";

interface MeetingChatTurn extends ChatTurn {
  id: string;
}

/** The starter prompts shown in the empty state — tap to ask immediately. */
const STARTERS: readonly string[] = [
  "Summarize the key decisions",
  "What are my action items?",
  "What questions were left open?",
];

/**
 * "Chat with this meeting" — a grounded Q&A panel over a single meeting's
 * transcript plus visibility-gated sources the user selects. It is a
 * presentational sibling of the timeline + analysis cards: the parent owns the
 * meeting; this component owns only the conversation it builds via
 * {@link IpcService.chatMeeting}.
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
  imports: [
    MarkdownComponent,
    SourcePickerComponent,
    ChatHistoryComponent,
    MurIconComponent,
  ],
  templateUrl: "./meeting-chat.component.html",
  styleUrl: "./meeting-chat.component.scss",
})
export class MeetingChatComponent {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);
  private readonly sourceScope = inject(SourceScopeService);
  private readonly errorCopy = inject(ErrorCopyService);
  private readonly folders = inject(FoldersService);
  private readonly notes = inject(NotesService);
  private readonly destroyRef = inject(DestroyRef);
  private readonly historyPrivacy = inject(AskHistoryPrivacyBarrierService);

  /** The anchor meeting whose transcript supplies the primary grounding. */
  readonly meetingId = input.required<string>();
  /**
   * The meeting's display title — the label of the anchor chip pre-filled into
   * the picker (falls back to the id when absent). Display-only; identity is
   * `kind + id`.
   */
  readonly anchorTitle = input<string | null>(null);

  /**
   * BARE mode (docked in the detail view's right-side Ask drawer): drop the
   * frosted `.card` frame + aurora glow and FILL the host height, so the drawer
   * is ONE coherent surface (no card-in-panel double border) with the composer
   * pinned to the bottom. Defaults false ⇒ the standalone card look (no other
   * caller exists, so the default keeps existing behavior byte-identical).
   */
  readonly bare = input(false);

  /**
   * Show a close (×) affordance in the header band — set by the drawer host so
   * the chat OWNS its own header (title + close) as a single aligned pane header
   * rather than a floating corner button. Pressing it emits {@link closed}; the
   * host toggles the drawer shut.
   */
  readonly showClose = input(false);
  /** Emitted when the header × is pressed. The drawer host closes itself. (Not `close` — that
   *  collides with the native DOM event name and trips `@angular-eslint/no-output-native`.) */
  readonly closed = output<void>();

  /**
   * Source-scoped Brain — the `<mur-source-picker>` selection (this meeting +
   * its active links, pre-filled on load). A NON-empty selection PINS the answer
   * to exactly those sources + their links; empty keeps the whole-meeting
   * grounding. `send()` passes `undefined` when empty (see below).
   */
  readonly sources = signal<SourceRef[]>([]);
  private readonly defaultSources = signal<SourceRef[]>([]);

  /**
   * Pre-fill the picker with the default scope for THIS meeting (the meeting
   * itself + its active linked neighbours) whenever the meeting id changes.
   * A legitimate signal-writing IPC effect (T1): keyed on `meetingId()`, with a
   * monotonic stale-result guard so a late reply for a superseded meeting is
   * dropped (the id can change while the component is reused across meetings).
   */
  private prefillSeq = 0;
  private readonly _prefill = effect(() => {
    const id = this.meetingId();
    const title = this.anchorTitle() ?? undefined;
    const privacyReady = this.historyPrivacy.ready();
    if (this.activeMeetingId !== null && this.activeMeetingId !== id) {
      this.resetConversation(true);
    }
    this.activeMeetingId = id;
    const seq = ++this.prefillSeq;
    if (!privacyReady) {
      this.defaultSources.set([]);
      this.sources.set([]);
      return;
    }
    void this.sourceScope
      .defaultSources("meeting", id, title)
      .then((defaults) => {
        if (this.meetingId() !== id || seq !== this.prefillSeq) {
          return;
        }
        this.defaultSources.set(defaults);
        if (this.conversationId() === null) {
          this.sources.set(defaults);
        }
      });
  });

  /** The running conversation (optimistic user turns + grounded replies). */
  readonly conversation = signal<MeetingChatTurn[]>([]);
  /** True while a {@link IpcService.chatMeeting} call is in flight. */
  readonly pending = signal(false);
  /** Inline error message (with a Retry affordance); null when clear. */
  readonly error = signal<string | null>(null);
  /** Working copy of the composer text (textarea (input) → signal). */
  readonly draft = signal("");

  readonly conversationId = signal<string | null>(null);
  readonly historyOpen = signal(false);
  readonly history = signal<AskConversationSummary[]>([]);
  readonly historyLoading = signal(false);
  readonly historyError = signal<string | null>(null);
  readonly historyActionError = signal<string | null>(null);
  readonly historyResumeId = signal<string | null>(null);
  readonly historyPrivacyReady = this.historyPrivacy.ready;
  readonly historyPrivacyError = this.historyPrivacy.error;
  private historyLoadSeq = 0;
  private requestSeq = 0;
  private nextTurnId = 1;
  private visibleFolders: Set<string> | null = null;
  private activeMeetingId: string | null = null;
  private removeHistoryInvalidator: (() => void) | null = null;

  constructor() {
    this.removeHistoryInvalidator = this.historyPrivacy.registerInvalidator(
      () => {
        this.sourcePicker()?.scrubPrivateState();
        this.resetConversation(true);
      },
    );
    void this.historyPrivacy.ensureReady();
    this.destroyRef.onDestroy(() => {
      this.removeHistoryInvalidator?.();
      this.removeHistoryInvalidator = null;
    });
  }

  /** Global-derived v1 history must leave the DOM on any visibility reduction. */
  private readonly _dropOnVisibilityReduction = effect(() => {
    const next = this.collectVisibleFolderIds(
      this.folders.tree(),
      this.notes.noteFolders(),
    );
    const previous = this.visibleFolders;
    this.visibleFolders = next;
    if (previous && [...previous].some((id) => !next.has(id))) {
      this.resetConversation(true);
    }
  });

  /** Starter prompts for the empty state. */
  protected readonly starters = STARTERS;

  /** A submit is allowed only with non-empty text and no in-flight request. */
  readonly canSend = computed(
    () =>
      this.historyPrivacyReady() &&
      !this.pending() &&
      this.draft().trim().length > 0,
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
    if (this.pending() || !this.historyPrivacyReady()) {
      return;
    }
    this.draft.set(question);
    void this.send();
  }

  /** Re-send the last user question after an error (it's still in the log). */
  retry(): void {
    if (this.pending() || !this.historyPrivacyReady()) {
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

  toggleHistory(): void {
    if (this.pending() || !this.historyPrivacyReady()) {
      return;
    }
    if (this.historyOpen()) {
      this.historyOpen.set(false);
      return;
    }
    this.historyOpen.set(true);
    void this.loadHistory();
  }

  newConversation(): void {
    if (!this.pending() && this.historyPrivacyReady()) {
      this.resetConversation(false);
      this.focusComposer();
    }
  }

  retryHistory(): void {
    if (this.historyPrivacyReady()) {
      void this.loadHistory();
    }
  }

  retryHistoryPrivacy(): void {
    this.resetConversation(true);
    void this.historyPrivacy.ensureReady();
  }

  async resumeConversation(id: string): Promise<void> {
    if (this.pending() || !this.historyPrivacyReady()) {
      return;
    }
    const seq = ++this.historyLoadSeq;
    const scope = this.scope();
    this.historyResumeId.set(id);
    this.historyActionError.set(null);
    try {
      const detail = await this.ipc.loadAskConversation(scope, id);
      if (seq !== this.historyLoadSeq || !this.sameScope(scope, this.scope())) {
        return;
      }
      this.requestSeq++;
      this.prefillSeq++;
      this.conversationId.set(detail.id);
      this.sources.set(detail.selectedSources);
      this.conversation.set(this.renderTurns(detail));
      this.draft.set("");
      this.error.set(null);
      this.historyActionError.set(null);
      this.historyOpen.set(false);
      this.scrollToLatest();
      this.focusComposer();
    } catch (e) {
      if (seq === this.historyLoadSeq) {
        this.historyActionError.set(
          this.errorCopy.because("Couldn’t load this conversation", e),
        );
      }
    } finally {
      if (seq === this.historyLoadSeq) {
        this.historyResumeId.set(null);
      }
    }
  }

  /**
   * Ask the current question. Captures the question + the PRIOR history
   * (the conversation before this turn), optimistically appends the user turn,
   * awaits the grounded reply, then appends the assistant turn. On failure the
   * user's question is kept (an inline Retry re-runs it).
   */
  async send(): Promise<void> {
    const question = this.draft().trim();
    if (!question || this.pending() || !this.historyPrivacyReady()) {
      return;
    }

    const requestSeq = ++this.requestSeq;
    const scope = this.scope();
    const conversationId = this.conversationId() ?? undefined;
    const optimisticUserId = `local-${this.nextTurnId++}`;
    this.error.set(null);
    this.draft.set("");
    this.conversation.set([
      ...this.conversation(),
      { id: optimisticUserId, role: "user", content: question },
    ]);
    this.pending.set(true);
    this.scrollToLatest();

    // Source-scoped Brain: an empty selection ⇒ pass undefined so the backend
    // keeps this-meeting grounding; a non-empty selection pins to those sources.
    const selectedSources = this.sources();
    try {
      const result = await this.ipc.chatMeetingPersisted(
        this.meetingId(),
        question,
        conversationId,
        selectedSources.length ? selectedSources : undefined,
      );
      if (
        requestSeq !== this.requestSeq ||
        !this.sameScope(scope, this.scope())
      ) {
        return;
      }
      this.conversationId.set(result.conversationId);
      this.conversation.update((turns) => [
        ...turns.map((turn) =>
          turn.id === optimisticUserId
            ? { ...turn, id: result.userMessageId }
            : turn,
        ),
        {
          id: result.assistantMessageId,
          role: "assistant",
          content: result.answer,
        },
      ]);
    } catch (e) {
      // Keep the user's question in the log so Retry can re-send it.
      if (requestSeq === this.requestSeq) {
        this.error.set(this.errorCopy.because("Couldn’t get an answer", e));
      }
    } finally {
      if (requestSeq === this.requestSeq) {
        this.pending.set(false);
        this.scrollToLatest();
      }
    }
  }

  private scope(): AskConversationScope {
    return { kind: "meeting", refId: this.meetingId() };
  }

  private async loadHistory(): Promise<void> {
    if (!this.historyPrivacyReady()) {
      return;
    }
    const seq = ++this.historyLoadSeq;
    const scope = this.scope();
    this.historyLoading.set(true);
    this.historyError.set(null);
    this.historyActionError.set(null);
    try {
      const rows = await this.ipc.listAskConversations(scope);
      if (seq === this.historyLoadSeq && this.sameScope(scope, this.scope())) {
        this.history.set(rows);
      }
    } catch (e) {
      if (seq === this.historyLoadSeq) {
        this.historyError.set(
          this.errorCopy.because("Couldn’t load conversation history", e),
        );
      }
    } finally {
      if (seq === this.historyLoadSeq) {
        this.historyLoading.set(false);
      }
    }
  }

  private resetConversation(clearHistoryRows: boolean): void {
    this.requestSeq++;
    this.historyLoadSeq++;
    this.pending.set(false);
    this.conversationId.set(null);
    this.conversation.set([]);
    this.draft.set("");
    this.error.set(null);
    if (clearHistoryRows) {
      this.prefillSeq++;
      this.defaultSources.set([]);
      this.sources.set([]);
    } else {
      this.sources.set(this.defaultSources());
    }
    this.historyOpen.set(false);
    this.historyLoading.set(false);
    this.historyError.set(null);
    this.historyActionError.set(null);
    this.historyResumeId.set(null);
    if (clearHistoryRows) {
      this.history.set([]);
    }
  }

  private renderTurns(detail: AskConversation): MeetingChatTurn[] {
    return detail.messages.map((message) => ({
      id: message.id,
      role: message.role,
      content: message.content,
    }));
  }

  private sameScope(a: AskConversationScope, b: AskConversationScope): boolean {
    if (a.kind === "vault" || b.kind === "vault") {
      return a.kind === b.kind;
    }
    return a.kind === b.kind && a.refId === b.refId;
  }

  private collectVisibleFolderIds(
    nodes: readonly {
      id: string;
      locked: boolean;
      unlocked: boolean;
      children?: unknown[];
    }[],
    noteFolders: readonly {
      id: string;
      locked: boolean;
      unlocked: boolean;
    }[],
  ): Set<string> {
    const visible = new Set<string>();
    const visit = (items: typeof nodes): void => {
      for (const node of items) {
        if (!node.locked || node.unlocked) {
          visible.add(`meeting:${node.id}`);
        }
        visit((node.children ?? []) as typeof nodes);
      }
    };
    visit(nodes);
    for (const folder of noteFolders) {
      if (!folder.locked || folder.unlocked) {
        visible.add(`note:${folder.id}`);
      }
    }
    return visible;
  }

  // --- Auto-scroll ---------------------------------------------------------

  /** The scrollable message log. */
  private readonly scroller = viewChild<ElementRef<HTMLDivElement>>("scroller");
  private readonly composer = viewChild<ElementRef<HTMLTextAreaElement>>("input");
  private readonly sourcePicker = viewChild(SourcePickerComponent);

  private focusComposer(): void {
    afterNextRender(() => this.composer()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

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
