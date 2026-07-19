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
import { NoteItemComponent } from "../note-item/note-item.component";
import { ProactiveHintCardComponent } from "../proactive-hint-card/proactive-hint-card.component";
import { WhisperCardComponent } from "../whisper-card/whisper-card.component";

/**
 * The in-meeting panel — a DOCUMENT-FIRST surface (v3 redesign, 2026-07-18):
 *
 *  - The meeting's ONE companion note is the always-visible HERO, rendered as ONE
 *    editable DOCUMENT via the embedded {@link NoteEditorComponent} (`[embedded]`) —
 *    the real create-note experience (`/` blocks, `[[` links, selection toolbar,
 *    in-note Ask-Brain popover, autosave). The HOST eagerly gets-or-creates the
 *    companion note so the editor mounts on a stable id.
 *  - **Ask Brain** — the conversational `@brain` thread — lives in a RIGHT-SIDE
 *    DRAWER toggled from the head (the same note+Brain pattern as routed notes,
 *    replacing the retired Note|Ask segmented tabs). Opening it SHRINKS the
 *    document column (never covers it).
 *  - Live brain REACTIONS (recall hints + contradiction whisper cards + the
 *    shadow-mode calibration prompt) stay AMBIENT above the body so they surface
 *    even with the drawer closed — never buried behind a toggle.
 *
 * The embedded editor is ALWAYS MOUNTED (and now always visible) for the whole
 * recording — one live editor instance is the flush-at-Stop target:
 * {@link RecorderStore.stop} awaits its durable save via {@link RecordingFlushService}
 * before finalizing, so a Stop fired inside the autosave debounce window can never
 * lose the user's prose. Closing the Ask drawer RELOADS the editor (not a re-mount)
 * so an Ask-Brain "Add to note" append / a mid-session external edit is reflected.
 *
 * The card is OPAQUE (`--surface-solid`), not the frosted glass `.card` — glass is
 * chrome, never a content panel wrapping overlays (T3/T4); the floating overlays the
 * editor spawns (`/`/`[[` menus, the Ask-Brain popover, the selection toolbar) all
 * teleport to <body> so their position:fixed anchors to the viewport.
 */
@Component({
  selector: "app-meeting-conversation",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
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
   * drawer close. Exactly one editor instance is live for the whole recording — the
   * flush-at-Stop target.
   */
  private readonly noteEditor = viewChild(NoteEditorComponent);

  readonly meetingId = input<string | null>(null);
  readonly hintsEnabled = input<boolean>(true);
  readonly enhancing = input(false);
  readonly settled = input(false);
  readonly enhanceAware = input(false);

  /**
   * Preset one-tap Ask-Brain starters shown in the summoned panel's empty state —
   * the antidote to the "lonely Ask Brain link" (Fireflies/Zoom live-assist).
   * Biased to what the on-device Brain answers well DURING a meeting (your notes +
   * prior-meeting retrieval), not a pretend two-speaker live transcript.
   */
  protected readonly askPresets: readonly string[] = [
    "Summarize what I've noted so far",
    "What did we decide?",
    "Draft a follow-up message",
    "Pull related notes on this meeting",
  ];

  /** Whether any live reaction is showing (drives the ambient rail's presence). */
  protected readonly hasReactions = computed(
    () =>
      (this.hintsEnabled() && !!this.store.hint()) ||
      this.store.whisperCards().length > 0 ||
      this.store.showShadowCalibration(),
  );

  /** True while the enhance hint should surface (quiet, centered, above the note). */
  protected readonly enhanceHintVisible = computed(
    () =>
      this.enhancing() ||
      this.settled() ||
      (this.enhanceAware() && this.store.hasPersistedNotes()),
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

    // EAGERLY get-or-create the companion note whenever a meeting is set, so the note
    // hero always has a document to mount on (the HOST owns eager creation; the
    // embedded editor never auto-creates).
    effect(() => {
      if (this.meetingId()) void this.store.ensureCompanionNote();
    });

    // Auto-scroll the Ask flow to the newest turn whenever the conversation changes
    // AND the panel is open. afterNextRender is zoneless-safe (no signal writes).
    effect(() => {
      this.store.notes();
      if (this.store.askPanelOpen()) {
        afterNextRender(() => this.scrollToBottom(), { injector: this.injector });
      }
    });

    // Ask-Brain panel lifecycle (Calm-Notepad, 2026-07-19). The open/closed state
    // lives in the ROOT store (summoned by the footer ✦ / the `/` slash "Ask Brain"
    // entry / a preset chip). On OPEN, focus the question input for a fast ask. On
    // CLOSE, reload the always-mounted editor + refresh the enhance-honesty content
    // so an Ask-Brain "Add to note" append is reflected in the hero document — the
    // retired toggleDrawer's close semantics, now driven by the shared signal. The
    // prev flag is closure-local: on a remount both values start false (the panel is
    // closed on mount), so no spurious reload/focus fires.
    let prevPanelOpen = false;
    effect(() => {
      const open = this.store.askPanelOpen();
      if (open && !prevPanelOpen) {
        afterNextRender(() => this.askInput()?.nativeElement.focus(), {
          injector: this.injector,
        });
      } else if (!open && prevPanelOpen) {
        this.noteEditor()?.reload();
        this.store.refreshCompanionContentNow();
      }
      prevPanelOpen = open;
    });
  }

  ngOnInit(): void {
    // Subscribe once to the wake/result/tool streams (idempotent). The store is a
    // root singleton, so its subscriptions outlive this component.
    void this.store.init();
  }

  /** Dismiss the summoned Ask-Brain panel (close × / Esc). The editor reload +
   * content refresh runs on the closed edge in the panel-lifecycle effect. */
  protected closeAsk(): void {
    this.store.closeAskPanel();
  }

  /** Fire a preset starter → ensure the panel is open, then open a thread. */
  protected askPreset(question: string): void {
    this.store.openAskPanel();
    void this.store.openThread(question).catch(() => {
      /* the store resolves the agent turn with an error in the thread */
    });
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
   * Submit the Ask-Brain question → open a thread (everything in the drawer is a
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
