import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  inject,
  input,
  output,
  signal,
  viewChild,
} from "@angular/core";
import { MeetingConversationStore } from "../../../core/meeting-conversation.store";
import type { NoteItem, ThreadTurn } from "../../../core/meeting-conversation.store";
import { MarkdownComponent } from "../../../shared/markdown/markdown.component";
import { AssistantSourcesComponent } from "../../../shared/assistant-sources/assistant-sources.component";

/**
 * ONE line of the user's notes + its anchored `@brain` THREAD (Slack-style).
 *
 *  - A PLAIN note line is the user's own jotting — a quiet "note" treatment (a
 *    subtle note glyph + token styling) so it reads clearly as the user's note,
 *    distinct from a thread/agent line.
 *  - A `@brain` THREAD renders as a Slack-style chat: a collapsible "▸ Thread (N)"
 *    toggle (collapsed → a short snippet of the question); expanded → the user's
 *    question as the FIRST right-aligned (purple) user bubble, then the brain's
 *    replies LEFT-aligned with a friendly "🧠 brain" identity, follow-up user
 *    bubbles on the right, and the thread's own Reply input. The question shows
 *    ONCE — the leading anchor turn is sliced out of the loop (no duplication).
 *
 * Propose → accept: an agent reply shows a DRAFT-NOTE card with a readable preview
 * of the proposed draft + "✓ Add to notes" / "Dismiss" ONLY when it carries a
 * `proposedNote` (the model decided the user asked it to MAKE a note); accept
 * appends THAT draft (not the whole reply) to the main notes; dismiss drops the
 * proposal (showing "Dismissed", never "✓ Added to notes"). A plain answer has no
 * proposal → no card, so the surface reads as a conversation, not a notes app with
 * a button under everything. The internal `propose_note` tool chip is filtered out
 * of the trace (it's plumbing, not a user-facing tool). The thread has its OWN
 * follow-up input so a follow-up goes to the agent WITHOUT re-typing `@brain`.
 *
 * Presentational — all state lives in the store; this component holds only its
 * follow-up draft. A note line holds TURNS, not notes, so there is NO mutual
 * recursion (no `forwardRef` / trap T2 needed). The thread is IN-FLOW (not a
 * floating overlay), so the in-flow surface tokens are correct (trap T3 N/A).
 */
@Component({
  selector: "app-note-item",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MarkdownComponent, AssistantSourcesComponent],
  templateUrl: "./note-item.component.html",
  styleUrl: "./note-item.component.scss",
})
export class NoteItemComponent {
  protected readonly store = inject(MeetingConversationStore);
  private readonly injector = inject(Injector);

  /** The note line (with its thread, if any) — the single source of truth. */
  readonly note = input.required<NoteItem>();
  /** Fired when this item's follow-up form is submitted (parent scrolls/etc.). */
  readonly followed = output<void>();

  private readonly followInput = viewChild<ElementRef<HTMLInputElement>>("fin");

  /** This thread's follow-up draft (signal-backed — zoneless). */
  protected readonly followDraft = signal("");

  /**
   * The turns rendered INSIDE the expanded thread, EXCLUDING the leading user
   * turn — that is the ANCHOR question, which the template renders explicitly as
   * the user's first right-aligned bubble (so it shows ONCE, no duplication).
   * Follow-up user turns + every agent turn still render here.
   */
  protected readonly visibleTurns = computed<ThreadTurn[]>(() => {
    const thread = this.note().thread ?? [];
    if (thread.length > 0 && thread[0].role === "user") {
      return thread.slice(1);
    }
    return thread;
  });

  /** Number of agent replies (for the collapsed "N" badge). */
  protected readonly replyCount = computed(
    () => this.note().thread?.filter((t) => t.role === "agent").length ?? 0,
  );

  /** A short one-line preview of the question shown on the collapsed toggle. */
  protected readonly snippet = computed(() => {
    const q = this.note().text.replace(/\s+/g, " ").trim();
    return q.length > 64 ? `${q.slice(0, 63)}…` : q;
  });

  /** Follow-up send is allowed with non-blank text and no in-flight thread turn. */
  protected readonly canFollow = computed(
    () => !this.note().threadPending && this.followDraft().trim().length > 0,
  );

  /**
   * Whether an agent turn can be accepted into the notes: ONLY when it carries a
   * `proposedNote` (the agent decided the user asked for a note) and has NOT been
   * accepted or dismissed. A plain answer (`proposedNote === null`) is never
   * acceptable → no "Add to notes" button.
   */
  protected canAccept(turn: ThreadTurn): boolean {
    return (
      turn.role === "agent" &&
      turn.status !== "pending" &&
      !turn.accepted &&
      !turn.dismissed &&
      turn.proposedNote !== null &&
      turn.proposedNote.trim().length > 0
    );
  }

  /**
   * The tool-trace chips to RENDER. Filters out INTERNAL mechanisms the user
   * should never see as "tools" — `propose_note` is the note-proposal plumbing,
   * not a user-facing search/lookup; the Draft-note card already conveys it (and
   * filtering it also drops the duplicated "propose_note" chips). Other tools
   * (search_meetings, web_search, …) still surface.
   */
  protected visibleTrace(trace: ThreadTurn["trace"]): ThreadTurn["trace"] {
    return trace.filter((t) => t.tool !== "propose_note");
  }

  protected onFollowInput(event: Event): void {
    this.followDraft.set((event.target as HTMLInputElement).value);
  }

  /** Submit a follow-up into THIS thread (no @brain needed) → the agent. */
  protected submitFollow(event: Event): void {
    event.preventDefault();
    const text = this.followDraft().trim();
    if (!text || this.note().threadPending) return;
    this.followDraft.set("");
    void this.store.followUp(this.note().id, text).catch(() => {
      /* the store resolves the agent turn with an error in the thread */
    });
    this.followed.emit();
    // Keep the focus in the thread input for a fast back-and-forth.
    afterNextRender(() => this.followInput()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

  /** Human label for a tool-trace chip. */
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

  /** Short message for a resolved agent turn that carries no answer text. */
  protected emptyNote(status: ThreadTurn["status"]): string {
    switch (status) {
      case "nothing_heard":
        return "Nie usłyszałem — spróbuj jeszcze raz";
      case "needs_consent":
        return "Needs consent to answer.";
      case "unavailable":
        return "That isn't available yet.";
      case "unrecognized":
        return "I didn't catch a question there.";
      case "error":
        return "Something went wrong.";
      default:
        return "(no answer)";
    }
  }

  /**
   * Phase 5 — the human label for the BRAIN CASCADE tier that answered
   * (`answeredFrom`), e.g. "this meeting" / "your vault" / "connectors". Empty
   * for a turn that didn't run through the cascade (`null`) so the chip hides.
   */
  protected tierLabel(turn: ThreadTurn): string {
    switch (turn.answeredFrom) {
      case "current_meeting":
        return "this meeting";
      case "vault":
        return "your vault";
      case "connectors":
        return "connectors";
      default:
        return "";
    }
  }
}
