import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  Injector,
  OnInit,
  afterNextRender,
  computed,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "../../../core/ipc.service";
import type {
  AssistantToolPayload,
  ChatTurn,
  VaultSource,
} from "../../../core/models";
import { MarkdownComponent } from "../../../shared/markdown/markdown.component";
import { SourcesComponent } from "../../../shared/sources/sources.component";

/**
 * A conversation turn as rendered on the Ask page. It mirrors {@link ChatTurn}
 * but assistant turns also carry the source meetings the answer was grounded in
 * (rendered as chips that deep-link into each meeting).
 */
interface AskTurn {
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
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink, MarkdownComponent, SourcesComponent],
  templateUrl: "./ask.component.html",
  styleUrl: "./ask.component.scss",
})
export class AskComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);
  private readonly destroyRef = inject(DestroyRef);

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
  private unlistenAskTool: UnlistenFn | null = null;

  /** Starter prompts for the empty state. */
  protected readonly starters = STARTERS;

  /** A submit is allowed only with non-empty text and no in-flight request. */
  readonly canSend = computed(
    () => !this.pending() && this.draft().trim().length > 0,
  );

  /**
   * Probe whether there are any meetings to ask about. A failure here is not
   * fatal — we still let the user try (the ask itself surfaces its own error),
   * so we only flip to the empty state on a confirmed empty list.
   */
  async ngOnInit(): Promise<void> {
    void this.listenAskTool();
    try {
      const meetings = await this.ipc.listMeetings();
      this.isEmpty.set(meetings.length === 0);
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
      this.unlistenAskTool = await this.ipc.onAskTool((p) => this.onAskTool(p));
      this.destroyRef.onDestroy(() => this.unlistenAskTool?.());
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
   * Ask the current question across the whole vault. Captures the question +
   * the PRIOR history (the conversation before this turn, as plain
   * {@link ChatTurn}s the backend expects), optimistically appends the user
   * turn, awaits the grounded reply, then appends the assistant turn with its
   * source meetings. On failure the user's question is kept (Retry re-runs it).
   *
   * Each question mints a fresh `askThreadId` that keys its live tool-trace
   * (`murmur://ask-tool` chips route strictly by it); the chips are cleared
   * when the turn lands — the answer's source chips remain the durable record.
   * We ship the FULL conversation as history: the backend caps it at the last
   * 12 messages itself, so no FE-side truncation.
   */
  async send(): Promise<void> {
    const question = this.draft().trim();
    if (!question || this.pending()) {
      return;
    }

    // History as seen by the model = everything BEFORE this turn, reduced to
    // the {role, content} shape the IPC contract takes (drop source metadata).
    const priorHistory: ChatTurn[] = this.conversation().map((t) => ({
      role: t.role,
      content: t.content,
    }));

    const askThreadId = crypto.randomUUID();
    this.activeAskId = askThreadId;
    this.trace.set([]);
    this.error.set(null);
    this.draft.set("");
    this.conversation.update((turns) => [
      ...turns,
      { role: "user", content: question },
    ]);
    this.pending.set(true);
    this.scrollToLatest();

    try {
      const result = await this.ipc.askVault(
        question,
        priorHistory,
        askThreadId,
      );
      this.conversation.update((turns) => [
        ...turns,
        {
          role: "assistant",
          content: result.answer,
          sources: result.sources,
        },
      ]);
    } catch (e) {
      // Keep the user's question in the log so Retry can re-send it.
      this.error.set("Couldn’t get an answer: " + String(e));
    } finally {
      // Retire this turn's trace: late tool events for it are dropped.
      this.activeAskId = null;
      this.trace.set([]);
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

  /** Presentational only: render a source timestamp as a friendly local date. */
  formatDate(startedAt: string): string {
    const d = new Date(startedAt);
    if (Number.isNaN(d.getTime())) {
      return startedAt;
    }
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  }
}
