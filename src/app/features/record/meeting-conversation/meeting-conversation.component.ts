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
import { NoteEditorComponent } from "../../notes/note-editor/note-editor.component";
import {
  MurSegmentedComponent,
  type SegmentOption,
} from "../../../design-system/segmented/segmented.component";
import { AiOrbComponent } from "../ai-orb/ai-orb.component";
import { NoteItemComponent } from "../note-item/note-item.component";
import { ProactiveHintCardComponent } from "../proactive-hint-card/proactive-hint-card.component";
import { WhisperCardComponent } from "../whisper-card/whisper-card.component";

/** The two tabs of the recording panel. */
type PanelTab = "note" | "ask";

/**
 * The in-meeting panel — a DOCUMENT-FIRST two-tab surface (v2 redesign):
 *
 *  - **"Note" (default):** the meeting's ONE companion note rendered as ONE editable
 *    DOCUMENT via the embedded {@link NoteEditorComponent} (`[embedded]="true"`) — the
 *    real create-note experience (`/` blocks, `[[` links, selection toolbar, in-note
 *    Ask-Brain popover, autosave). The HOST eagerly gets-or-creates the companion note
 *    (via {@link MeetingConversationStore.ensureCompanionNote}) so the editor mounts on
 *    a stable id — there are NO per-jot "Saved" badges; the note IS the document.
 *  - **"Ask Brain":** the conversational `@brain` thread (reuses the store's
 *    open/run/resolve/follow-up/tool-trace/citation machinery). A plain single-line
 *    input at the foot opens a thread (everything here is a question — no `@brain`
 *    marker parsing). An answer's "Add to note" appends into the companion note.
 *
 * The embedded editor is ALWAYS MOUNTED for the whole recording — the Note tab is
 * HIDDEN (not destroyed) when the user is on Ask Brain, and returning to it calls the
 * editor's `reload()` (not a re-mount) so an Ask-Brain "Add to note" append / a
 * mid-session external edit is reflected. Keeping ONE live editor instance is what
 * makes the flush-before-Stop deterministic: {@link RecorderStore.stop} awaits that
 * single editor's durable save via {@link RecordingFlushService} before finalizing, so
 * a Stop fired inside the autosave debounce window can never lose the user's prose (an
 * earlier destroy/re-mount left an in-flight destroy-flush racing Stop when the user
 * clicked Stop from the Ask Brain tab). The reactions rail (recall hints / whisper
 * cards) lives in the Ask-Brain tab.
 *
 * IN-FLOW (not a floating overlay) — the frosted `.card` chrome is correct (trap T3 is
 * handled inside the editor's own `/`/`[[` menus + the Ask-Brain popover).
 */
@Component({
  selector: "app-meeting-conversation",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    AiOrbComponent,
    MurSegmentedComponent,
    NoteEditorComponent,
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
  private readonly askInput = viewChild<ElementRef<HTMLInputElement>>("askInput");
  /**
   * The ONE always-mounted embedded companion note editor. Used to `reload()` it on
   * return to the Note tab (in place of the retired re-mount nonce). It stays in the
   * DOM (hidden while on Ask Brain) so exactly one editor instance is live for the
   * whole recording — the flush-at-Stop target.
   */
  private readonly noteEditor = viewChild(NoteEditorComponent);

  /**
   * The active recording's meeting id (null when there's no meeting yet). Pushed
   * into the store so the companion note + Ask-Brain scope bind to THIS meeting.
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

  /** The active tab — the DOCUMENT-FIRST "Note" tab is the default. */
  protected readonly activeTab = signal<PanelTab>("note");

  /** The two-tab segmented control options. */
  protected readonly tabOptions: readonly SegmentOption[] = [
    { value: "note", label: "Note" },
    { value: "ask", label: "Ask Brain" },
  ];

  /** The active tab as the segmented control's two-way value (mirrors {@link activeTab}). */
  protected readonly tabValue = computed<string>(() => this.activeTab());

  /** During the enhance pass the orb shows its shipped 'processing' choreography. */
  readonly orbStateView = computed(() =>
    this.enhancing() ? ("processing" as const) : this.store.orbState(),
  );

  /** This session's Ask-Brain question draft (signal-backed — zoneless). */
  protected readonly askDraft = signal("");
  /** Send is allowed with non-blank text. */
  protected readonly canAsk = computed(() => this.askDraft().trim().length > 0);

  constructor() {
    // Keep the store pointed at the active meeting so the companion note + Ask-Brain
    // scope bind to the right meeting. Signal writes in effects are allowed (v19+).
    effect(() => {
      this.store.setMeetingId(this.meetingId());
    });

    // EAGERLY get-or-create the companion note whenever a meeting is set, so the
    // "Note" tab always has a document to mount on (the HOST owns eager creation;
    // the embedded editor never auto-creates). The effect only reads the input +
    // calls an async store method (the signal write happens inside the store).
    effect(() => {
      if (this.meetingId()) void this.store.ensureCompanionNote();
    });

    // Auto-scroll the Ask-Brain flow to the newest turn whenever the conversation
    // changes AND the Ask tab is showing. Tracks the notes signal; schedules the
    // DOM work via afterNextRender (zoneless-safe; no signal writes → no NG0600).
    effect(() => {
      this.store.notes();
      if (this.activeTab() === "ask") {
        afterNextRender(() => this.scrollToBottom(), { injector: this.injector });
      }
    });
  }

  ngOnInit(): void {
    // Subscribe once to the wake/result/tool streams (idempotent). The store is a
    // root singleton, so its subscriptions outlive this component.
    void this.store.init();
  }

  /** Switch tabs. Re-entering "Note" RELOADS the always-mounted editor (no re-mount)
   *  + refreshes the enhance-honesty content signal; entering "Ask" focuses the
   *  question input. */
  protected selectTab(tab: string): void {
    const next = tab as PanelTab;
    if (next === this.activeTab()) return;
    this.activeTab.set(next);
    if (next === "note") {
      // The editor stays mounted (hidden) while on Ask Brain, so returning re-reads
      // its body in place — reflecting an Ask-Brain "Add to note" append / external
      // edit — rather than destroying + recreating the one live flush target.
      this.noteEditor()?.reload();
      this.store.refreshCompanionContentNow();
    } else {
      afterNextRender(() => this.askInput()?.nativeElement.focus(), {
        injector: this.injector,
      });
    }
  }

  /** Scroll the Ask-Brain flow to its newest content. */
  protected scrollToBottom(): void {
    const el = this.flow()?.nativeElement;
    if (el) el.scrollTop = el.scrollHeight;
  }

  protected onAskInput(event: Event): void {
    this.askDraft.set((event.target as HTMLInputElement).value);
  }

  /**
   * Submit the Ask-Brain question → open a thread (everything in this tab is a
   * question — no `@brain` marker parsing). Self-clears; a no-op for blank text.
   */
  protected submitAsk(event: Event): void {
    event.preventDefault();
    const q = this.askDraft().trim();
    if (!q) return;
    this.askDraft.set("");
    void this.store.openThread(q).catch(() => {
      /* the store resolves the agent turn with an error in the thread */
    });
    // Keep the focus in the ask input for a fast back-and-forth.
    afterNextRender(() => this.askInput()?.nativeElement.focus(), {
      injector: this.injector,
    });
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
