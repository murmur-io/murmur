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
  DashboardScopeRef,
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

interface NoteChatTurn extends ChatTurn {
  id: string;
}

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
 * per-meeting transcript chat. Local authored notes use SQLite-canonical durable
 * history; the shared Org-item caller remains intentionally stateless and keeps
 * sending its in-memory prior turns through the legacy `askVault` contract.
 *
 * Lives in its own file so its scoped styles get their own per-component
 * `anyComponentStyle` budget.
 */
@Component({
  selector: "app-note-chat",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    MarkdownComponent,
    SourcePickerComponent,
    ChatHistoryComponent,
    MurIconComponent,
  ],
  templateUrl: "./note-chat.component.html",
  styleUrl: "./note-chat.component.scss",
})
export class NoteChatComponent {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);
  private readonly sourceScope = inject(SourceScopeService);
  private readonly errorCopy = inject(ErrorCopyService);
  private readonly folders = inject(FoldersService);
  private readonly notes = inject(NotesService);
  private readonly destroyRef = inject(DestroyRef);
  private readonly historyPrivacy = inject(AskHistoryPrivacyBarrierService);

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
  readonly conversation = signal<NoteChatTurn[]>([]);
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
  /** Optional ID-only composite board beside this note's canonical anchor. */
  readonly dashboard = signal<DashboardScopeRef | null>(null);
  private readonly defaultSources = signal<SourceRef[]>([]);

  /** Durable history is local-authored-note only; Org stays intentionally stateless. */
  readonly historyEnabled = computed(() => this.anchorKind() === "note");
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
  private activeAnchorKey: string | null = null;
  private removeHistoryInvalidator: (() => void) | null = null;

  constructor() {
    this.removeHistoryInvalidator = this.historyPrivacy.registerInvalidator(
      () => {
        this.sourcePicker()?.scrubPrivateState();
        this.resetConversation(true);
      },
    );
    this.destroyRef.onDestroy(() => {
      this.removeHistoryInvalidator?.();
      this.removeHistoryInvalidator = null;
    });
  }

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
    const privacyReady = this.historyPrivacy.ready();
    const key = `${kind}:${id}`;
    if (this.activeAnchorKey !== null && this.activeAnchorKey !== key) {
      this.resetConversation(true);
    }
    this.activeAnchorKey = key;
    const seq = ++this.prefillSeq;
    if (kind !== "note") {
      // Org anchor: pinned server-side via `pinnedOrgItemId`; an org item is not a local SourceRef,
      // so prefilling a note scope for its id would be wrong. Start the picker empty and make the
      // unsupported pinned-org + dashboard combination impossible even across component reuse.
      this.defaultSources.set([]);
      this.sources.set([]);
      this.dashboard.set(null);
      return;
    }
    if (!privacyReady) {
      this.defaultSources.set([]);
      this.sources.set([]);
      return;
    }
    void this.sourceScope.defaultSources("note", id, title).then((defaults) => {
      if (
        this.anchorKind() !== "note" ||
        this.noteId() !== id ||
        seq !== this.prefillSeq
      ) {
        return;
      }
      this.defaultSources.set(defaults);
      if (this.conversationId() === null) {
        this.sources.set(defaults);
      }
    });
  });

  /**
   * Org remains stateless, but it can still hold local source titles and an
   * answer derived from them. Every NoteChat therefore joins the same privacy
   * barrier; `historyEnabled` controls persistence UI only.
   */
  private readonly _ensureAskPrivacy = effect(() => {
    this.anchorKind();
    void this.historyPrivacy.ensureReady();
  });

  /** Local-note durable history is global-derived and leaves DOM on any lock reduction. */
  private readonly _dropOnVisibilityReduction = effect(() => {
    const enabled = this.historyEnabled();
    const next = this.collectVisibleFolderIds(
      this.folders.tree(),
      this.notes.noteFolders(),
    );
    const previous = this.visibleFolders;
    this.visibleFolders = next;
    if (enabled && previous && [...previous].some((id) => !next.has(id))) {
      this.resetConversation(true);
    }
  });

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

  /** Clear the whole conversation back to the empty state. */
  clear(): void {
    if (this.pending()) {
      return;
    }
    this.conversation.set([]);
    this.error.set(null);
  }

  toggleHistory(): void {
    if (
      !this.historyEnabled() ||
      this.pending() ||
      !this.historyPrivacyReady()
    ) {
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
    if (
      this.historyEnabled() &&
      !this.pending() &&
      this.historyPrivacyReady()
    ) {
      this.resetConversation(false);
      this.focusComposer();
    }
  }

  retryHistory(): void {
    if (this.historyEnabled() && this.historyPrivacyReady()) {
      void this.loadHistory();
    }
  }

  retryHistoryPrivacy(): void {
    this.resetConversation(true);
    void this.historyPrivacy.ensureReady();
  }

  /** A board switch cannot mutate an existing durable thread's identity. */
  onDashboardChange(next: DashboardScopeRef | null): void {
    if (this.anchorKind() === "org") {
      this.dashboard.set(null);
      return;
    }
    if (this.dashboard()?.id === next?.id) return;
    const sources = this.sources();
    if (this.conversationId() !== null || this.conversation().length > 0) {
      this.resetConversation(false);
      this.sources.set(sources);
    }
    this.dashboard.set(next);
  }

  onSourcesChange(next: SourceRef[]): void {
    const dashboard = this.dashboard();
    if (this.conversationId() !== null || this.conversation().length > 0) {
      this.resetConversation(false);
      this.dashboard.set(dashboard);
    }
    this.sources.set(next);
  }

  async resumeConversation(id: string): Promise<void> {
    if (
      !this.historyEnabled() ||
      this.pending() ||
      !this.historyPrivacyReady()
    ) {
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
      this.dashboard.set(detail.dashboard ?? null);
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
   * Ask the current question, grounded in this note's source scope. Captures the
   * question + the PRIOR history (the conversation before this turn),
   * optimistically appends the user turn, awaits the grounded reply, then
   * appends the assistant turn. On failure the user's question is kept (an inline
   * Retry re-runs it). Authored notes continue a backend-owned durable id;
   * shared Org items stay on the legacy stateless branch below.
   */
  async send(): Promise<void> {
    const question = this.draft().trim();
    if (!question || this.pending() || !this.historyPrivacyReady()) {
      return;
    }

    // History as seen by the model = everything BEFORE this turn.
    const priorHistory: ChatTurn[] = this.conversation().map((turn) => ({
      role: turn.role,
      content: turn.content,
    }));
    // Source-scoped Brain: pin the answer to this note + its links.
    const selectedSources = this.sources();

    const requestSeq = ++this.requestSeq;
    const scopeKind = this.anchorKind();
    const conversationScope = this.scope();
    const anchorKey = `${scopeKind}:${this.noteId()}`;
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

    try {
      let answer: string;
      let persistedConversationId: string | null = null;
      let persistedUserMessageId: string | null = null;
      let persistedAssistantMessageId: string | null = null;
      if (scopeKind === "org") {
        const result = await this.ipc.askVault(
          question,
          priorHistory,
          undefined,
          selectedSources.length ? selectedSources : undefined,
          this.noteId(),
          undefined,
        );
        answer = result.answer;
      } else {
        const result = await this.ipc.askVaultPersisted(
          conversationScope,
          question,
          conversationId,
          selectedSources.length ? selectedSources : undefined,
          undefined,
          this.dashboard()?.id,
        );
        answer = result.answer;
        persistedConversationId = result.conversationId;
        persistedUserMessageId = result.userMessageId;
        persistedAssistantMessageId = result.assistantMessageId;
      }
      if (
        requestSeq !== this.requestSeq ||
        anchorKey !== `${this.anchorKind()}:${this.noteId()}`
      ) {
        return;
      }
      if (persistedConversationId)
        this.conversationId.set(persistedConversationId);
      this.conversation.update((turns) => [
        ...turns.map((turn) =>
          persistedUserMessageId && turn.id === optimisticUserId
            ? { ...turn, id: persistedUserMessageId }
            : turn,
        ),
        {
          id: persistedAssistantMessageId ?? `local-${this.nextTurnId++}`,
          role: "assistant",
          content: answer,
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
    return { kind: "note", refId: this.noteId() };
  }

  private async loadHistory(): Promise<void> {
    if (!this.historyEnabled() || !this.historyPrivacyReady()) {
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
      this.dashboard.set(null);
    } else {
      this.sources.set(this.defaultSources());
      this.dashboard.set(null);
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

  private renderTurns(detail: AskConversation): NoteChatTurn[] {
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
  private readonly composer =
    viewChild<ElementRef<HTMLTextAreaElement>>("input");
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
