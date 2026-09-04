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
  signal,
  viewChild,
} from "@angular/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "../../../core/ipc.service";
import type {
  AssistantToolPayload,
  AskConversation,
  AskConversationScope,
  AskConversationSummary,
  DashboardScopeRef,
  SourceRef,
  VaultSource,
  LinkKind,
} from "../../../core/models";
import { SourcePickerComponent } from "../../../design-system/source-picker/source-picker.component";
import { MarkdownComponent } from "../../../shared/markdown/markdown.component";
import { SourcesComponent } from "../../../shared/sources/sources.component";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";
import { ChatHistoryComponent } from "../../../design-system/chat-history/chat-history.component";
import { FoldersService } from "../../../services/folders.service";
import { MurIconComponent } from "../../../design-system/icon/icon.component";
import { NotesService } from "../../../services/notes.service";
import { AskHistoryPrivacyBarrierService } from "../../../core/ask-history-privacy-barrier.service";
import { DateFormatService } from "../../../core/date-format.service";

/**
 * A conversation turn as rendered on the Ask page. It mirrors {@link ChatTurn}
 * but assistant turns also carry the source meetings the answer was grounded in
 * (rendered as chips that deep-link into each meeting).
 */
interface AskTurn {
  /**
   * Stable id for `@for` tracking (never key a turn on $index) — the log is
   * NOT append-only: `retry()` pops the dangling user turn and re-appends it,
   * which would land back at the same index and fool an index-tracked `@for`
   * into reusing the old DOM node (silently skipping its entrance animation).
   */
  id: string;
  role: "user" | "assistant";
  content: string;
  /** Present on assistant turns only — the meetings that grounded the answer. */
  sources?: VaultSource[];
}

/**
 * One tool the `ask_vault` agentic loop used while answering the IN-FLIGHT
 * question — drives the live trace chips in the typing row ("Searching
 * notes… ✓"). Tool name + a coarse count only (no PII), same visual language
 * as the record-screen thread chips. Cleared when the answer lands.
 */
interface AskTraceStep {
  /** Stable id for `@for` tracking (never key a trace chip on $index). */
  id: number;
  tool: string;
  state: "running" | "done";
  ok: boolean;
  count: number | null;
}

/** The starter prompts shown in the empty state — tap to ask immediately. */
const STARTERS: readonly string[] = [
  "What did we decide about …?",
  "What are my open action items across all meetings?",
  "Summarize my last week",
];

/**
 * "Ask your meetings" — a premium full-page chat that answers grounded in the
 * WHOLE vault (every past meeting), via {@link IpcService.askVault}. It is a
 * page-level sibling of meeting-chat (which is scoped to one meeting): the same
 * conversation/composer/auto-scroll/retry language, but answers span all
 * meetings and each reply lists its source meetings as deep-link chips.
 *
 * Lives in its own file so its inline styles get their own per-component
 * `anyComponentStyle` budget.
 *
 * Assistant replies are rendered as PLAIN TEXT with `white-space: pre-wrap`
 * (no markdown lib, no innerHTML/DomSanitizer) — line breaks + spacing from the
 * model are preserved verbatim and safely.
 */
@Component({
  selector: "app-ask",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    MarkdownComponent,
    SourcesComponent,
    SourcePickerComponent,
    ChatHistoryComponent,
    MurIconComponent,
  ],
  templateUrl: "./ask.component.html",
  styleUrl: "./ask.component.scss",
})
export class AskComponent implements OnInit {
  private readonly dates = inject(DateFormatService);

  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);
  private readonly destroyRef = inject(DestroyRef);
  private readonly errorCopy = inject(ErrorCopyService);
  private readonly folders = inject(FoldersService);
  private readonly notes = inject(NotesService);
  private readonly historyPrivacy = inject(AskHistoryPrivacyBarrierService);

  /** True while the initial "any meetings?" probe is in flight. */
  readonly loading = signal(true);
  /** True when the vault has no meetings — the page shows an empty state. */
  readonly isEmpty = signal(false);

  /** The running conversation (optimistic user turns + grounded replies). */
  readonly conversation = signal<AskTurn[]>([]);
  /** True while an {@link IpcService.askVault} call is in flight. */
  readonly pending = signal(false);
  /** Inline error message (with a Retry affordance); null when clear. */
  readonly error = signal<string | null>(null);
  /** Working copy of the composer text (textarea (input) → signal). */
  readonly draft = signal("");

  /**
   * Source-scoped Brain — the `<mur-source-picker>` selection. Default EMPTY:
   * the Ask page answers across the WHOLE vault unless the user opts in by
   * picking sources. A NON-empty selection pins the answer to those sources +
   * their links; empty ⇒ pass `undefined` (whole-vault) to {@link askVault}.
   */
  readonly sources = signal<SourceRef[]>([]);

  /**
   * What the Sources picker may offer here — the content kinds it always offered, PLUS whole
   * containers (a Space or folder).
   *
   * A container behaves differently from the rest and the split happens in {@link askScope}: a
   * content source is PINNED (packed into the corpus verbatim), while a container is a SCOPE
   * (retrieval is narrowed to what is filed under it, subtree included). That is why a container
   * can be picked at all despite `LinkKind.Container` deliberately not being a content source —
   * it never enters the corpus as text, it only says where to look.
   */
  protected readonly askSourceKinds: readonly LinkKind[] = [
    "meeting",
    "note",
    "document",
    "container",
  ];

  /**
   * The picker selection, split into the two things the backend takes.
   *
   * `pinned` are exact items to pack; `scopeFolderIds` are containers to search inside. Sending a
   * container as a pinned source would hand the packer a place that holds no text of its own — the
   * backend refuses that by construction, so the split has to happen here.
   */
  protected askScope(): { pinned: SourceRef[]; scopeFolderIds: string[] } {
    const selected = this.sources();
    return {
      pinned: selected.filter((s) => s.kind !== "container"),
      scopeFolderIds: selected
        .filter((s) => s.kind === "container")
        .map((s) => s.id),
    };
  }
  /** One composite board identity; never expanded into child SourceRefs in the WebView. */
  readonly dashboard = signal<DashboardScopeRef | null>(null);

  /** Durable SQLite conversation id; distinct from the per-request trace id. */
  readonly conversationId = signal<string | null>(null);
  /** In-flow history browser state. The current conversation remains intact behind it. */
  readonly historyOpen = signal(false);
  readonly history = signal<AskConversationSummary[]>([]);
  readonly historyLoading = signal(false);
  readonly historyError = signal<string | null>(null);
  readonly historyActionError = signal<string | null>(null);
  readonly historyResumeId = signal<string | null>(null);
  readonly historyPrivacyReady = this.historyPrivacy.ready;
  readonly historyPrivacyError = this.historyPrivacy.error;
  private readonly conversationScope: AskConversationScope = { kind: "vault" };
  private historyLoadSeq = 0;
  private requestSeq = 0;

  /** Live tool-trace chips for the IN-FLIGHT question (cleared when it lands). */
  readonly trace = signal<AskTraceStep[]>([]);
  /**
   * The FE-minted thread id of the in-flight question — `murmur://ask-tool`
   * payloads are routed STRICTLY by it (anything else, including unstamped
   * events, is dropped: never mis-file a chip). Plumbing only, never rendered.
   */
  private activeAskId: string | null = null;
  /** Monotonic id source for trace chips (stable `@for` keys). */
  private nextTraceId = 1;
  /** Ephemeral key source until a successful durable send returns backend message UUIDs. */
  private nextTurnId = 1;
  private unlistenAskTool: UnlistenFn | null = null;
  private removeHistoryInvalidator: (() => void) | null = null;
  private destroyed = false;
  private visibleFolders: Set<string> | null = null;

  /**
   * Durable v1 answers are conservatively global-derived. If any previously
   * visible folder becomes hidden/removed, immediately evict every plaintext
   * turn/title/source from this mounted WebView while the backend purges SQLite.
   */
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

  constructor() {
    this.removeHistoryInvalidator = this.historyPrivacy.registerInvalidator(
      () => {
        this.sourcePicker()?.scrubPrivateState();
        this.resetConversation(true);
      },
    );
    void this.historyPrivacy.ensureReady();
    this.destroyRef.onDestroy(() => {
      this.destroyed = true;
      this.unlistenAskTool?.();
      this.removeHistoryInvalidator?.();
      this.removeHistoryInvalidator = null;
    });
  }

  /**
   * Probe whether there is ANYTHING to ask about — meetings OR notes (Ask
   * chats across the whole vault, not just recordings; a vault with only
   * standalone notes and zero meetings is very much askable — live-found bug,
   * 2026-07-12: this used to check `listMeetings()` alone, so a notes-only
   * vault wrongly showed "No meetings to ask about yet" and blocked the
   * feature). A failure here is not fatal — we still let the user try (the
   * ask itself surfaces its own error), so we only flip to the empty state
   * when BOTH come back confirmed-empty.
   */
  async ngOnInit(): Promise<void> {
    void this.listenAskTool();
    try {
      const [meetings, notes] = await Promise.all([
        this.ipc.listMeetings(),
        this.ipc.listNotes(null),
      ]);
      this.isEmpty.set(meetings.length === 0 && notes.length === 0);
    } catch {
      this.isEmpty.set(false);
    } finally {
      this.loading.set(false);
    }
  }

  /**
   * Subscribe ONCE to the Ask page's own tool-trace stream and store the
   * unlisten so DestroyRef can release it (no leaked listener). Best-effort:
   * a failure just means no trace chips — asking still works.
   */
  private async listenAskTool(): Promise<void> {
    try {
      const unlisten = await this.ipc.onAskTool((p) => this.onAskTool(p));
      if (this.destroyed) unlisten();
      else this.unlistenAskTool = unlisten;
    } catch {
      // Not running under Tauri (browser smoke without the mock): no chips.
    }
  }

  /**
   * Land one `murmur://ask-tool` payload as a trace chip on the in-flight
   * question. Routed STRICTLY by threadId — a payload for another turn (a
   * stale event from the previous question, or an unstamped one) is dropped.
   * A "running" event pushes a new chip; a "done" event resolves the most
   * recent matching running chip (or appends one, if "running" was missed).
   * Plain event callback writing signals — not an effect, so no NG0600.
   */
  private onAskTool(p: AssistantToolPayload): void {
    if (this.activeAskId === null || p.threadId !== this.activeAskId) {
      return;
    }
    // Internal plumbing tools are not user-facing work — never chip them
    // (mirrors the record-screen thread's visibleTrace filter).
    if (p.tool === "propose_note") {
      return;
    }
    this.trace.update((trace) => {
      if (p.state === "running") {
        return [
          ...trace,
          {
            id: this.nextTraceId++,
            tool: p.tool,
            state: "running",
            ok: true,
            count: p.count,
          },
        ];
      }
      const next = trace.slice();
      for (let i = next.length - 1; i >= 0; i--) {
        if (next[i].tool === p.tool && next[i].state === "running") {
          next[i] = { ...next[i], state: "done", ok: p.ok, count: p.count };
          return next;
        }
      }
      next.push({
        id: this.nextTraceId++,
        tool: p.tool,
        state: "done",
        ok: p.ok,
        count: p.count,
      });
      return next;
    });
    // Keep the growing chip row pinned in view like every other new message.
    this.scrollToLatest();
  }

  /** Human label for a tool-trace chip (same wording as the record screen). */
  protected toolLabel(tool: string): string {
    switch (tool) {
      case "search_meetings":
        return "Searching notes";
      case "search_semantic":
        return "Searching by meaning";
      case "get_meeting":
        return "Reading a meeting";
      case "list_recent_meetings":
        return "Listing meetings";
      case "get_open_commitments":
        return "Checking action items";
      case "get_entity_dossier":
        return "Looking up an entity";
      case "web_search":
        return "Searching the web";
      case "calendar_lookup":
        return "Checking the calendar";
      default:
        return tool;
    }
  }

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

  /** Open/close the in-flow history browser and refresh its bounded rows. */
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

  /** Start a blank draft without deleting the durable conversation. */
  newConversation(): void {
    if (!this.pending() && this.historyPrivacyReady()) {
      this.resetConversation(false);
      this.focusComposer();
    }
  }

  /** Retry the newest-first list without touching the conversation underneath. */
  retryHistory(): void {
    if (this.historyPrivacyReady()) {
      void this.loadHistory();
    }
  }

  retryHistoryPrivacy(): void {
    this.resetConversation(true);
    void this.historyPrivacy.ensureReady();
  }

  /**
   * A durable thread cannot change composite identity in place. User selection
   * therefore starts a fresh conversation while preserving manual sources.
   * History restore writes `dashboard` directly and never enters this handler.
   */
  onDashboardChange(next: DashboardScopeRef | null): void {
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

  /** Load and resume one canonical conversation, including its saved sources. */
  async resumeConversation(id: string): Promise<void> {
    if (this.pending() || !this.historyPrivacyReady()) {
      return;
    }
    const seq = ++this.historyLoadSeq;
    this.historyResumeId.set(id);
    this.historyActionError.set(null);
    try {
      const detail = await this.ipc.loadAskConversation(
        this.conversationScope,
        id,
      );
      if (seq !== this.historyLoadSeq) {
        return;
      }
      this.requestSeq++;
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
   * Ask the current question across the whole vault. Captures the question +
   * the PRIOR history (the conversation before this turn, as plain
   * {@link ChatTurn}s the backend expects), optimistically appends the user
   * turn, awaits the grounded reply, then appends the assistant turn with its
   * source meetings. On failure the user's question is kept (Retry re-runs it).
   *
   * Each question mints a fresh `askThreadId` that keys its live tool-trace
   * (`murmur://ask-tool` chips route strictly by it); the chips are cleared
   * when the turn lands — the answer's source chips remain the durable record.
   * Durable conversation context is loaded and bounded by the backend from
   * SQLite; the WebView sends only the durable id and the new question.
   */
  async send(): Promise<void> {
    const question = this.draft().trim();
    if (!question || this.pending() || !this.historyPrivacyReady()) {
      return;
    }

    const askThreadId = crypto.randomUUID();
    const requestSeq = ++this.requestSeq;
    const conversationId = this.conversationId() ?? undefined;
    const optimisticUserId = `local-${this.nextTurnId++}`;
    this.activeAskId = askThreadId;
    this.trace.set([]);
    this.error.set(null);
    this.draft.set("");
    this.conversation.update((turns) => [
      ...turns,
      {
        id: optimisticUserId,
        role: "user",
        content: question,
      },
    ]);
    this.pending.set(true);
    this.scrollToLatest();

    // Source-scoped Brain: an empty selection ⇒ pass undefined (whole-vault);
    // a non-empty selection pins the answer to those sources + their links.
    // Pinned CONTENT and container SCOPE are two different instructions to the backend, so the
    // one picker selection is split before it is sent: items get packed, containers narrow the
    // search. Both empty ⇒ the unchanged whole-vault path.
    const { pinned, scopeFolderIds } = this.askScope();
    try {
      const result = await this.ipc.askVaultPersisted(
        this.conversationScope,
        question,
        conversationId,
        pinned.length ? pinned : undefined,
        askThreadId,
        this.dashboard()?.id,
        scopeFolderIds.length ? scopeFolderIds : undefined,
      );
      if (requestSeq !== this.requestSeq) {
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
          sources: result.sources,
        },
      ]);
    } catch (e) {
      // Keep the user's question in the log so Retry can re-send it.
      if (requestSeq === this.requestSeq) {
        this.error.set(this.errorCopy.because("Couldn’t get an answer", e));
      }
    } finally {
      if (requestSeq === this.requestSeq) {
        // Retire this turn's trace: late tool events for it are dropped.
        this.activeAskId = null;
        this.trace.set([]);
        this.pending.set(false);
        this.scrollToLatest();
      }
    }
  }

  private async loadHistory(): Promise<void> {
    if (!this.historyPrivacyReady()) {
      return;
    }
    const seq = ++this.historyLoadSeq;
    this.historyLoading.set(true);
    this.historyError.set(null);
    this.historyActionError.set(null);
    try {
      const rows = await this.ipc.listAskConversations(this.conversationScope);
      if (seq === this.historyLoadSeq) {
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
    this.activeAskId = null;
    this.trace.set([]);
    this.pending.set(false);
    this.conversationId.set(null);
    this.conversation.set([]);
    this.draft.set("");
    this.error.set(null);
    this.sources.set([]);
    this.dashboard.set(null);
    this.historyOpen.set(false);
    this.historyLoading.set(false);
    this.historyError.set(null);
    this.historyActionError.set(null);
    this.historyResumeId.set(null);
    if (clearHistoryRows) {
      this.history.set([]);
    }
  }

  private renderTurns(detail: AskConversation): AskTurn[] {
    return detail.messages.map((message) => ({
      id: message.id,
      role: message.role,
      content: message.content,
      ...(message.role === "assistant" && message.sources.length
        ? { sources: message.sources }
        : {}),
    }));
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

  /** Presentational only: render a source timestamp as a friendly local date. */
  /** Formatted through {@link DateFormatService} — the one place a date becomes user-visible text. */
  formatDate(startedAt: string): string {
    return this.dates.day(startedAt);
  }
}
