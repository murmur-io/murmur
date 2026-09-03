import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  EnvironmentInjector,
  Injector,
  OnInit,
  afterNextRender,
  computed,
  effect,
  inject,
  signal,
  untracked,
  viewChild,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { ActivatedRoute, Router, RouterLink } from "@angular/router";
import { convertFileSrc } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { IpcService } from "../../../core/ipc.service";
import { tabKeyFor } from "../../../core/tab-keys";
import { TabsService } from "../../../core/tabs.service";
import { meetingStatusPillClass } from "../../../design-system/meeting-status";
import { MurCopyIdComponent } from "../../../design-system/copy-id/copy-id.component";
import type {
  AppConfigDto,
  AssistantInteraction,
  ClaimAlignment,
  FolderNode,
  GraphPayload,
  MeetingDetail,
  MeetingOrgShareInfo,
  MeetingTimeline,
  NoteAttachmentDto,
  Segment,
  SpeakerSuggestion,
} from "../../../core/models";
import {
  FoldersService,
  type FolderExposure,
} from "../../../services/folders.service";
import { ToastService } from "../../../services/toast.service";
import { referencedNoteAttachments } from "../../../services/note-attachment.service";
import { LockBadgeComponent } from "../../folders/lock-badge/lock-badge.component";
import { AudioPanelComponent } from "../audio-panel/audio-panel.component";
import { MeetingChatComponent } from "../meeting-chat/meeting-chat.component";
import {
  DetailTabsComponent,
  type DetailTab,
  type DetailTabDef,
} from "../detail-tabs/detail-tabs.component";
import {
  NotePanelComponent,
  type AssistantQa,
  type NoteSection,
  type ParsedCitation,
  type ParsedNote,
} from "../note-panel/note-panel.component";
import { SharePanelComponent } from "../share-panel/share-panel.component";
import { VerifyPanelComponent } from "../verify-panel/verify-panel.component";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";
import { MeetingCommandBarComponent } from "../meeting-command-bar/meeting-command-bar.component";

/** One checklist entry parsed from a `- [ ]` / `- [x]` action-item line. */
interface ActionItem {
  done: boolean;
  text: string;
}

@Component({
  selector: "app-detail",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    RouterLink,
    LockBadgeComponent,
    DetailTabsComponent,
    NotePanelComponent,
    AudioPanelComponent,
    MeetingChatComponent,
    SharePanelComponent,
    VerifyPanelComponent,
    MeetingCommandBarComponent,
    MurCopyIdComponent,
  ],
  templateUrl: "./detail.component.html",
  styleUrl: "./detail.component.scss",
})
export class DetailComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);
  private readonly injector = inject(Injector);
  private readonly errorCopy = inject(ErrorCopyService);
  /** Environment (root) injector — hosts the detach-proof root lock effect. */
  private readonly envInjector = inject(EnvironmentInjector);
  private readonly destroyRef = inject(DestroyRef);
  private readonly folders = inject(FoldersService);
  private readonly toast = inject(ToastService);
  private readonly tabsService = inject(TabsService);

  readonly detail = signal<MeetingDetail | null>(null);

  /**
   * The header status pill's state modifier — DERIVED, not a template method
   * call (a method binding re-runs on every change-detection pass; a computed
   * is cached + dependency-tracked). Empty while nothing is loaded, which
   * renders the neutral pill.
   */
  readonly statusPillClass = computed(() =>
    meetingStatusPillClass(this.detail()?.meeting.status ?? ""),
  );
  readonly loading = signal(true);
  readonly busy = signal(false);
  /** A retry is in flight — the offer disables itself rather than allowing a second claim. */
  readonly retryingTranscription = signal(false);
  /**
   * Offer the retry only for a failed recording whose audio is still on disk. Both conditions come
   * straight from `retry_transcription_prep`'s refusals, so the button appears exactly where the
   * command can actually do something.
   */
  readonly canRetryTranscription = computed(() => {
    const meeting = this.detail()?.meeting;
    return (
      !!meeting &&
      meeting.status === "ERROR" &&
      !!meeting.audioPath &&
      meeting.audioPath.trim().length > 0
    );
  });
  private readonly conversionRequest = signal<{
    meetingId: string;
    sequence: number;
  } | null>(null);
  private conversionSequence = 0;
  readonly converting = computed(
    () => this.conversionRequest()?.meetingId === this.detail()?.meeting.id,
  );
  readonly msg = signal("");

  /**
   * Install config snapshot — gates the "Verify with Jira" panel (shown only when
   * `jiraEnabled && jiraConsented`, and never for a locked meeting). Loaded per
   * meeting alongside `keepsMasters`; null until the first load resolves.
   */
  readonly config = signal<AppConfigDto | null>(null);

  // --- Phase 0.5 lock gate -------------------------------------------------
  /**
   * True while the backend has MASKED this meeting (it lives in a sealed,
   * not-session-unlocked folder). The template renders the lock gate instead
   * of the note/transcript/audio/timeline/actions. Mirrors `detail()?.locked`.
   */
  readonly locked = computed(() => this.detail()?.locked === true);
  /** True while an `unlockMeeting` biometric call is in flight (pending state). */
  readonly unlocking = signal(false);
  /** Latched true once an unlock attempt has FAILED — reveals the "reset folder" escape hatch. */
  readonly unlockFailed = signal(false);
  /** Two-step inline confirm for the destructive reset (no browser dialog). */
  readonly confirmDiscard = signal(false);
  /** True while the discard-unrecoverable-lock IPC is in flight. */
  readonly discarding = signal(false);
  /** Focusable unlock button — focused after the gate renders (afterNextRender). */
  private readonly unlockButton =
    viewChild<ElementRef<HTMLButtonElement>>("unlockButton");

  /**
   * The Audio tab's player, when that tab is the active `@switch` case (null
   * otherwise). Used by `onTabBackgrounded` (tabs plan risk #3 + perf-audit
   * fix 2) and the lock-mask path (`maskLocally` → `stopAndUnload`).
   */
  private readonly audioPanel = viewChild<AudioPanelComponent>("audioPanel");

  /**
   * This meeting's TAB was backgrounded (detached-for-reuse). Called from
   * `AppShellComponent`'s `<router-outlet (detach)>` — which fires on every
   * tab switch, not just a real destroy — via a duck-typed check (the shell
   * can't import a lazy feature component). Two duties:
   *  - pause playback (a backgrounded tab must not keep narrating);
   *  - collapse an expanded transcript back to the windowed cap (perf-audit
   *    fix 2 — an expanded transcript kept ~21k detached DOM nodes alive).
   * A no-op when the Audio tab isn't the active case (no panel instance).
   */
  onTabBackgrounded(): void {
    const panel = this.audioPanel();
    panel?.pausePlayback();
    panel?.collapseTranscript();
  }

  // --- Ask-about-this-meeting slideout drawer -----------------------------
  /**
   * True while the right-side "Ask about this meeting" drawer is open. Hosted in
   * this shell (not `note-panel`) so the conversation survives Note/Audio/Share
   * tab switches AND the chat's `_prefill` runs once per meeting (not on every
   * Note-tab open). Default-closed, NOT persisted (a fresh ask each open).
   */
  private readonly _askDrawerOpen = signal(false);
  readonly askDrawerOpen = this._askDrawerOpen.asReadonly();

  // --- Move-to-folder popover ---------------------------------------------
  /** True while the folder-picker popover is open. */
  readonly moveOpen = signal(false);

  /**
   * Read-only folder badge for the header: the owning folder's name + exposure
   * (open / locked / session), or null when the note is at the vault root or the
   * folder isn't (yet) in the loaded tree. Reactive to both the meeting's
   * `folderId` and the folders store, so a move/lock updates it live.
   */
  readonly folderBadge = computed<{
    name: string;
    exposure: FolderExposure;
  } | null>(() => {
    const fid = this.detail()?.meeting.folderId ?? null;
    if (fid === null) {
      return null;
    }
    const node = this.findFolder(this.folders.tree(), fid);
    return node
      ? { name: node.name, exposure: this.folders.exposureOf(node) }
      : null;
  });

  // --- Inline title rename state ------------------------------------------
  /** True while the header title is swapped for an inline text input. */
  readonly renaming = signal(false);
  /** Working copy of the title (input (input) → signal); empty values ignored. */
  readonly titleDraft = signal("");
  /** Disables Save/Cancel while a renameMeeting IPC call is in flight. */
  readonly savingRename = signal(false);
  /** Focusable rename input — focused after it renders (afterNextRender). */
  private readonly renameInput =
    viewChild<ElementRef<HTMLInputElement>>("renameInput");

  // --- In-app delete confirmation state -----------------------------------
  /** True while the signal-driven delete-confirm panel is shown. */
  readonly confirmingDelete = signal(false);
  /** True while a deleteMeeting IPC call is in flight (moves it to the Trash). */
  readonly deleting = signal(false);
  /** Inline error surfaced when the delete fails. */
  readonly deleteError = signal("");

  // --- In-app note editor state -------------------------------------------
  /** True while the raw-markdown editor replaces the rendered analysis cards. */
  readonly editing = signal(false);
  /** Two-way working copy of the note's markdown (textarea (input) → signal). */
  readonly draft = signal("");
  /** Disables Save/Cancel while an updateNote IPC call is in flight. */
  readonly saving = signal(false);
  /** Inline error surfaced when a save fails. */
  readonly saveError = signal("");
  /** Drives the brief "Saved" confirmation badge after a successful write. */
  readonly justSaved = signal(false);
  /** Gated image DTOs for this meeting note; synchronously cleared on relock. */
  readonly attachments = signal<NoteAttachmentDto[]>([]);
  readonly meetingAttachmentBusy = signal(false);
  private attachmentSeq = 0;
  /** Exact attachment set present when the current edit session began. */
  private editAttachmentSnapshot: NoteAttachmentDto[] = [];

  /** Tracked so we can cancel the pending "Saved" reset on destroy (no leaks). */
  private savedResetTimer: ReturnType<typeof setTimeout> | null = null;

  // --- Export menu state ---------------------------------------------------
  /**
   * Transient success token for the export buttons. One of "", "md-copied",
   * "md-saved" or "audio-saved" — the matching button swaps its label briefly.
   */
  readonly exportMsg = signal("");
  /** True while a save dialog + export IPC call is in flight (disables saves). */
  readonly exporting = signal(false);
  /** Inline error surfaced when an export fails. */
  readonly exportError = signal("");
  /** Tracked so we can cancel the pending export-label reset on destroy. */
  private exportResetTimer: ReturnType<typeof setTimeout> | null = null;

  // --- Page tabs (Note · Audio · Share) ------------------------------------
  /** The tab bar entries (order = display order). Extensible: add an id + a
   *  shell `@switch` branch. Each id has a matching `@case` panel in the
   *  template, so no tab renders blank. */
  readonly detailTabs: DetailTabDef[] = [
    { id: "note", label: "Note" },
    { id: "audio", label: "Audio" },
    { id: "share", label: "Share" },
  ];
  /** The active detail tab (Note default). Reset per meeting in `loadMeeting`. */
  readonly activeTab = signal<DetailTab>("note");

  /**
   * Whether this install keeps high-fidelity per-stream master archives (the
   * "Keep high-fidelity masters" setting). Loaded best-effort in ngOnInit; gates
   * the master-export actions, since a meeting only has masters when it was
   * recorded with this on. Install-global (not per-meeting), so the backend
   * stays the source of truth — it rejects a stream with no master (InvalidArg)
   * or a sealed folder (Locked), both surfaced as friendly inline messages.
   */
  readonly keepsMasters = signal(false);

  // --- Org Brain badge (Shared Brain v1) -----------------------------------
  /**
   * Every org THIS meeting is actively shared into (`meetingOrgShares`, a real
   * meeting-id join — supersedes the earlier title-match heuristic). Empty when
   * never shared, OR when the meeting is locked (the backend gates this exactly
   * like `getMeetingDetail`). Drives the "Shared with…" header pill(s); loaded
   * best-effort and refreshed when the share-panel reports a change.
   */
  readonly orgShares = signal<MeetingOrgShareInfo[]>([]);
  /** True when this meeting is shared into at least one org — drives the pill's presence. */
  readonly orgShared = computed(() => this.orgShares().length > 0);

  // --- Export Canvas (Obsidian .canvas board) ------------------------------
  /** True while an exportCanvas IPC call is in flight (disables the button). */
  readonly exportingCanvas = signal(false);
  /** The written .canvas path, shown briefly as a "Canvas saved" confirmation. */
  readonly canvasMsg = signal("");
  /** Inline error surfaced when the canvas export fails (e.g. no timeline yet). */
  readonly canvasError = signal("");
  /** Tracked so we can cancel the pending canvas-confirmation reset on destroy. */
  private canvasResetTimer: ReturnType<typeof setTimeout> | null = null;

  // --- Meeting tags (editable; persisted via set/getMeetingTags) -----------
  /** The meeting's current tags (loaded in ngOnInit; updated optimistically). */
  readonly tags = signal<string[]>([]);
  /** Working copy of the add-tag input (input (input) → signal). */
  readonly tagDraft = signal("");
  /** Disables chips + input while a setMeetingTags IPC call is in flight. */
  readonly tagsBusy = signal(false);
  /** Inline error surfaced when a tag add/remove fails. */
  readonly tagsError = signal("");

  /**
   * Whether the "Copy path" button in the note panel just copied (feeds the
   * panel's `pathCopied` input). Playback state (currentTime/duration/playing)
   * now lives in `app-audio-panel`, which owns the `<audio>` element.
   */
  readonly copied = signal(false);

  /**
   * Asset-protocol URL for the recording, or null when there is no audio.
   * Passed to the audio panel (the player) and, as `!!audioSrc()`, to the note
   * panel (gates Save-audio / master exports).
   */
  readonly audioSrc = computed(() => {
    const path = this.detail()?.meeting.audioPath;
    return path ? convertFileSrc(path) : null;
  });

  /** The note's markdown decomposed into front-matter + body sections. */
  readonly note = computed<ParsedNote | null>(() => {
    const md = this.detail()?.note?.markdown;
    return md ? this.parseNote(md) : null;
  });
  /** Images actually referenced by the current canonical meeting-note markdown. */
  readonly referencedAttachments = computed(() =>
    referencedNoteAttachments(
      this.detail()?.note?.markdown ?? "",
      this.attachments(),
    ),
  );

  /** Load attachment bytes only for an unlocked meeting with a note. */
  private readonly _loadAttachments = effect(() => {
    const id = this.detail()?.meeting.id ?? null;
    const locked = this.locked();
    const hasNote = this.detail()?.note !== null;
    const seq = ++this.attachmentSeq;
    if (!id || locked || !hasNote) {
      this.attachments.set([]);
      return;
    }
    void this.fetchAttachments(id, seq);
  });

  private async fetchAttachments(id: string, seq: number): Promise<void> {
    try {
      const rows = await this.ipc.listNoteAttachments("meeting", id);
      if (seq !== this.attachmentSeq || this.locked()) {
        return;
      }
      this.attachments.set(Array.isArray(rows) ? rows : []);
    } catch {
      if (seq === this.attachmentSeq) {
        this.attachments.set([]);
      }
    }
  }

  onMeetingAttachmentAdded(attachment: NoteAttachmentDto): void {
    if (
      this.locked() ||
      attachment.ownerKind !== "meeting" ||
      attachment.ownerId !== this.detail()?.meeting.id
    ) {
      return;
    }
    this.attachments.update((rows) =>
      rows.some((row) => row.id === attachment.id)
        ? rows
        : [...rows, attachment],
    );
  }

  onDetailTabChange(tab: DetailTab): void {
    if (this.meetingAttachmentBusy()) {
      this.toast.info("Finish adding the image before switching tabs.");
      return;
    }
    this.activeTab.set(tab);
  }

  /**
   * The persisted in-meeting assistant Q&A for this meeting, citations parsed
   * into vault/web shapes for rendering. Empty when the meeting is locked (the
   * backend gates `assistantInteractions` exactly like `note`/`segments`).
   */
  readonly interactions = computed<AssistantQa[]>(() => {
    const raw = this.detail()?.assistantInteractions ?? [];
    return raw.map((i, idx) => this.parseInteraction(i, idx));
  });

  // Note↔note backlinks are now fetched + merged inside the self-loading
  // `app-connections` "Related" panel (with `kind="meeting"`), so the host no
  // longer owns a separate backlinks fetch (IA consolidation, 2026-07-19).

  // --- Receipts (Brain v3 PR-5): claim → second-of-audio ------------------
  /**
   * Per-claim audio receipts for the CURRENT note (backend `get_note_receipts`,
   * `meeting_is_unlocked`-gated → EMPTY for a locked meeting). Fed to the note
   * panel, which renders a chip per aligned claim. Empty until the first fetch
   * resolves and whenever the meeting is locked/masked (the fetch is skipped).
   */
  readonly receipts = signal<ClaimAlignment[]>([]);
  /** Stale-result guard token — a late reply for a superseded meeting is dropped. */
  private receiptsSeq = 0;

  /**
   * A pending receipt seek handed to the Audio tab's player (Brain v3 PR-5). The
   * panel is (re)created for the Audio `@switch` case, so a viewChild method call
   * from the Note tab would hit a not-yet-existing instance — instead we pass the
   * target as an INPUT the panel applies on mount. `seq` (bumped per click) makes
   * a repeat click on the same receipt re-fire the panel effect. Null when idle
   * AND after the panel acks consumption (`onSeekConsumed`) — a consumed target
   * must not replay on the next Audio-tab visit.
   */
  readonly seekTarget = signal<{
    startS: number;
    segId: number;
    seq: number;
  } | null>(null);

  /**
   * Fetch this meeting's note receipts whenever the loaded meeting (or note
   * body) changes, and SKIP the (gated) fetch while it is locked/masked — a
   * locked meeting must surface no receipts (WHEN/BY-WHOM leak), and the backend
   * returns an empty list there anyway. Re-fetches when the note markdown changes
   * (an edit/re-summarize) so the chips track the current note. Legitimate
   * signal-writing effect (T1): async IPC keyed on inputs with a stale-result guard.
   */
  private readonly _loadReceipts = effect(() => {
    const id = this.detail()?.meeting.id ?? null;
    const locked = this.locked();
    // Track the note body so an edit/re-summarize re-derives the receipts.
    const noteMd = this.detail()?.note?.markdown ?? null;
    const seq = ++this.receiptsSeq;
    if (!id || locked || !noteMd) {
      this.receipts.set([]);
      return;
    }
    void this.fetchReceipts(id, seq);
  });

  private async fetchReceipts(id: string, seq: number): Promise<void> {
    try {
      const rows = await this.ipc.getNoteReceipts(id);
      if (seq !== this.receiptsSeq) {
        return; // superseded by a newer meeting / lock / note-edit
      }
      this.receipts.set(Array.isArray(rows) ? rows : []);
    } catch {
      if (seq === this.receiptsSeq) {
        this.receipts.set([]);
      }
    }
  }

  /**
   * A receipt chip was clicked in the note panel: switch to the Audio tab and
   * hand the player the seek/flash target. Carries only audio coordinates — no
   * note/transcript text, no on-disk path. The Audio tab's panel is created by
   * the `@switch`, so the target is an input it applies on mount (see the panel's
   * `_applyReceiptSeek`); `seq` (already bumped by the note panel) survives so a
   * repeat click on the same chip re-fires.
   */
  onSeekReceipt(target: { startS: number; segId: number; seq: number }): void {
    this.seekTarget.set(target);
    this.activeTab.set("audio");
  }

  /**
   * Deep-link seek (Brain v3 audit PR-8): `/meeting/:id?seekS=<s>&seekSeg=<idx>`
   * — the entity-detail ledger's Source chip navigates here with the fact's
   * receipt coordinates (already computed by the GATED `get_fact_receipt`).
   * Applied only once the detail has loaded and ONLY while unmasked (`locked()`
   * skips WITHOUT stripping — the pending seek stays in the URL so a later
   * unlock still applies it; a sealed meeting never seeks, matching the
   * receipts blanking), then handed to the Audio tab through the exact
   * `onSeekReceipt` path a note chip uses. `queryParamMap` rides `toSignal`
   * because `TabRouteReuseStrategy` REUSES this component when the meeting tab
   * is already open — a snapshot read in `ngOnInit` would drop those
   * navigations.
   *
   * The params are ONE-SHOT: after an apply they are STRIPPED from the URL
   * (`replaceUrl`, other params preserved) instead of latched — so re-clicking
   * the SAME Source chip puts the params back and is a fresh apply (parity
   * with note-receipt chips, which re-fire via `seq`). `routeSeekInFlight`
   * only bridges the apply→strip window (a `detail()` re-set mid-strip must
   * not double-apply) and resets when the strip lands.
   */
  private readonly routeSeekParams = toSignal(this.route.queryParamMap);
  private routeSeekInFlight = false;
  /** Monotonic seq for route-driven seeks (own space; the panel keys on object identity). */
  private routeSeekSeq = 0;
  private readonly _applyRouteSeek = effect(() => {
    const qp = this.routeSeekParams();
    const d = this.detail();
    if (!qp) {
      return;
    }
    const sRaw = qp.get("seekS");
    const segRaw = qp.get("seekSeg");
    if (sRaw === null || segRaw === null) {
      // Params gone (the strip landed, or a plain navigation) → next arrival is fresh.
      this.routeSeekInFlight = false;
      return;
    }
    if (!d || this.locked()) {
      return; // not loaded / sealed: leave the params pending (see the doc above).
    }
    if (this.routeSeekInFlight) {
      return; // apply already done, strip still in flight — never double-apply.
    }
    this.routeSeekInFlight = true;
    const startS = Number(sRaw);
    const segId = Number(segRaw);
    // Malformed coords apply nothing but still fall through to the strip below,
    // so junk params don't sit in the URL forever.
    if (Number.isFinite(startS) && Number.isFinite(segId)) {
      this.onSeekReceipt({ startS, segId, seq: ++this.routeSeekSeq });
    }
    void this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { seekS: null, seekSeg: null },
      queryParamsHandling: "merge",
      replaceUrl: true,
    });
  });

  /**
   * The audio panel APPLIED the pending receipt seek: clear it so a later
   * Audio-tab revisit (which recreates the panel) never replays the consumed
   * seek/flash. The seq match guards the (theoretical) race where a newer chip
   * click landed between the panel applying and this ack — the newer target
   * survives to be applied. Repeat clicks on the SAME chip keep working: the
   * note panel bumps `seq` per click, so a fresh non-null target always arrives.
   */
  onSeekConsumed(seq: number): void {
    if (this.seekTarget()?.seq === seq) {
      this.seekTarget.set(null);
    }
  }

  // --- Phase 5 model-provenance badge -------------------------------------
  /**
   * Human-readable label for the model-provenance badge in the Analysis header.
   * Prefers `modelServed` (what the gateway actually ran) over `aiModel`
   * (what was requested). Returns null when no provenance is available (legacy
   * meetings, locked meetings, providers without `CallMeta`) — the badge is
   * hidden via `@if` in that case.
   */
  readonly provenanceLabel = computed<{ model: string; provider: string } | null>(() => {
    const d = this.detail();
    if (!d) return null;
    const model = d.modelServed ?? d.aiModel;
    const provider = d.aiProvider;
    if (!model && !provider) return null;
    return { model: model ?? "", provider: provider ?? "" };
  });

  // --- Lazy transcript segments (perf: off the Note tab) ------------------
  /**
   * The meeting's transcript segments — fetched LAZILY only when the Audio tab
   * first opens (mirrors the lazy-timeline latch below). `get_meeting_detail`
   * now returns an EMPTY `segments`, so a plain Note-tab open never ships the
   * whole transcript; the Audio panel reads this signal instead of `d.segments`.
   */
  readonly segments = signal<Segment[]>([]);
  /** True while the (gated) `get_meeting_segments` read is in flight. */
  readonly segmentsLoading = signal(false);
  /**
   * One-shot latch (mirror of `_timelineAttempted`): the meeting id whose
   * Audio-tab segments read has already been attempted this open, so the
   * `_segmentsOnAudioTab` effect can never re-enter for the same meeting even
   * when the backend returns an empty list (a legitimately transcript-less
   * meeting). Cleared per meeting in `loadMeeting` and on `maskLocally`.
   */
  private readonly _segmentsAttempted = signal<string | null>(null);

  // --- Interactive timeline (speaker + topic viz) -------------------------
  readonly timeline = signal<MeetingTimeline | null>(null);
  readonly timelineLoading = signal(false);

  /**
   * One-shot latch for the Audio-tab effect: the meeting id whose Audio-tab timeline read has
   * already been attempted (and resolved) this open. Set the moment `loadTimeline` starts, so the
   * effect can NEVER re-enter for the same meeting — even if the read resolves to an empty/falsy
   * backend timeline that leaves no terminal signal set (the #234 infinite-loop root cause: a
   * `get_timeline`/`generate_timeline` returning `{speakers:[],topics:[]}`/null left
   * `timeline==null && !error && !needsGeneration`, so the effect re-fired forever). Cleared per
   * meeting in `loadMeeting`. The explicit Retry / Generate clicks call `loadTimeline`/`generateTimeline`
   * directly (not via the effect), so a deliberate re-run is unaffected.
   */
  private readonly _timelineAttempted = signal<string | null>(null);

  /**
   * PERF/OOM (P0.1): generate the timeline LAZILY — only when the Audio tab (the only surface that
   * renders it) is first opened for an unlocked meeting and it isn't already loaded / in flight.
   * `loadMeeting`/`unlock` no longer kick it off on open, so a plain Note-tab open never triggers the
   * multi-GB on-device LLM pass that OOM-killed the Mac. Signal writes in the invoked `loadTimeline`
   * are fine in v22 (no flag) — the `_timelineAttempted` latch below makes the effect fire at most
   * once per meeting-open, so there is no loop. See docs/research/2026-07-07-perf-memory-audit.md.
   */
  private readonly _timelineOnAudioTab = effect(() => {
    const d = this.detail();
    const id = d?.meeting.id ?? null;
    if (
      this.activeTab() === "audio" &&
      d &&
      id &&
      !this.locked() &&
      !this.timeline() &&
      !this.timelineLoading() &&
      // Do NOT auto-retry after a failure: a persistent error would otherwise re-fire this effect
      // every time `timelineLoading` flips back to false → an infinite retry loop. A failed load
      // surfaces the Retry button, which clears `timelineError` and re-calls `loadTimeline`.
      !this.timelineError() &&
      // Do NOT re-fire once we've surfaced the "Generate" affordance (on-device, no cache): the
      // read leaves `timeline` null + `timelineLoading` false, so without this guard the effect
      // would loop calling `loadTimeline` forever. Cleared only by the user's Generate click.
      !this.timelineNeedsGeneration() &&
      // ONE-SHOT LATCH (#234): never re-attempt the Audio-tab read for a meeting id already tried
      // this open — an empty/falsy backend result that sets no terminal signal must NOT re-loop.
      this._timelineAttempted() !== id
    ) {
      void this.loadTimeline();
    }
  });

  /**
   * PERF (transcript off the Note tab): fetch the transcript segments LAZILY —
   * only when the Audio tab (the only surface that renders them) is first opened
   * for an unlocked meeting and they aren't already in flight. Same guard shape
   * as `_timelineOnAudioTab`; the `_segmentsAttempted` latch makes it fire at
   * most once per meeting-open (an empty backend result sets no other terminal
   * signal, so without the latch it would loop). Legitimate signal-writing
   * effect (T1): async IPC keyed on inputs with a stale-result guard in
   * `loadSegments`. A locked() meeting never fetches (the gate returns []).
   */
  private readonly _segmentsOnAudioTab = effect(() => {
    const d = this.detail();
    const id = d?.meeting.id ?? null;
    if (
      this.activeTab() === "audio" &&
      d &&
      id &&
      !this.locked() &&
      !this.segmentsLoading() &&
      // ONE-SHOT LATCH: never re-attempt the read for a meeting id already tried
      // this open — an empty (transcript-less) result must NOT re-loop.
      this._segmentsAttempted() !== id
    ) {
      void this.loadSegments();
    }
  });

  /**
   * Read the (gated) transcript segments for the current meeting. Sets the
   * one-shot latch FIRST so the Audio-tab effect can't re-enter, guards against
   * a concurrent in-flight read, and drops a stale result if the user navigated
   * to another meeting mid-flight (mirrors `loadTimeline`). A gated/locked read
   * resolves to `[]`. Never throws to the caller — a failure just leaves the
   * transcript empty.
   */
  async loadSegments(): Promise<void> {
    const id = this.detail()?.meeting.id;
    if (!id || this.segmentsLoading()) {
      return;
    }
    // Latch first (mirror of loadTimeline) so the effect never re-fires for this
    // meeting even if the read resolves to an empty list.
    this._segmentsAttempted.set(id);
    this.segmentsLoading.set(true);
    try {
      const rows = await this.ipc.getMeetingSegments(id);
      // STALE-RESULT + LOCK guard: drop this if the user switched meetings
      // mid-flight (never paint meeting A's transcript into meeting B), OR if the
      // meeting locked while the read was in flight — a late real-row reply must
      // never (even transiently) repopulate `segments()` after `maskLocally`
      // blanked it. It couldn't render (the `@if (locked())` teardown unmounts
      // the audio panel) and a later unlock re-fetches, but this keeps the signal
      // from ever holding sealed rows post-lock (lock-model belt-and-braces).
      if (this.detail()?.meeting.id !== id || this.locked()) {
        return;
      }
      this.segments.set(Array.isArray(rows) ? rows : []);
    } catch {
      if (this.detail()?.meeting.id === id) {
        this.segments.set([]);
      }
    } finally {
      this.segmentsLoading.set(false);
    }
  }

  /**
   * LOCK-REACTIVE re-mask (required by the tabs plan §6 — a real leak surface
   * once meetings can stay open in a BACKGROUNDED tab). Created in the
   * CONSTRUCTOR as a **ROOT effect** via the `EnvironmentInjector` — NOT a
   * component/view effect. A view effect (`VIEW_EFFECT_NODE`) only ever runs
   * inside `refreshView()`'s CD traversal, and a tab detached by
   * `TabRouteReuseStrategy` is REMOVED from its LContainer, so a view effect
   * is FROZEN the whole time the tab is backgrounded (and on reattach the
   * template executes BEFORE view effects — a stale-plaintext frame). A root
   * effect is scheduled on the root `EffectScheduler` and flushed by
   * `ApplicationRef.synchronizeOnce()` BEFORE the view-refresh loop — verified
   * against this repo's @angular/core 22.0.5 (`_pending_tasks-chunk.mjs`
   * `createRootEffect`/`ROOT_EFFECT_NODE.consumerMarkedDirty`,
   * `_debug_node-chunk.mjs` `synchronizeOnce`: `rootEffectScheduler.flush()`
   * precedes `detectChangesInternal`). So it fires on every
   * lock/unlock/relock/"Lock all"/screen-share auto-relock regardless of view
   * attachment, and its SYNCHRONOUS mask lands before any template paints.
   *
   * Because the effect hangs off the ENVIRONMENT injector, it is NOT
   * auto-destroyed with this component — see the constructor's explicit
   * `DestroyRef.onDestroy(() => ref.destroy())` (skipping that would leak a
   * live effect per closed tab).
   *
   * `untracked` wraps the handler so `folders.tree()` is the ONLY dependency —
   * never `detail()` (which changes on every note/tag/timeline edit).
   */
  private readonly _lockMaskHandler = (tree: FolderNode[]): void => {
    const d = this.detail();
    const id = d?.meeting.id;
    if (!d || !id) {
      return;
    }
    // PERF (perf-audit fix 1a — the O(N²) refetch stampede): every real
    // `folders.load()` produces a NEW tree array reference, so this effect
    // fires in EVERY open tab on EVERY tree change (folder create/rename/
    // move, another tab's open priming the tree, …). Refetching the full
    // ~568 KB-per-hour-meeting DTO unconditionally measured 27 fetches where
    // 6 suffice on a 6-tab session. So: derive THIS meeting's sealed state
    // from the new tree and SKIP only when it is consistent with the
    // in-memory `locked` flag — the skip is keyed EXCLUSIVELY on the derived
    // lock state, so an unlocked→locked transition can never be suppressed:
    //   sealed=true,  locked=false → ACT (sync mask + refetch)   ← the leak path
    //   sealed=false, locked=true  → ACT (refetch → unmasked)
    //   consistent                 → SKIP (nothing lock-relevant changed)
    //   indeterminate              → SAFE PATH (refetch; never skip)
    const fid = d.meeting.folderId;
    if (fid == null) {
      // Vault-root meeting (null/absent folderId): locks are per-folder, the
      // root can never be sealed → derived sealed = false. Skip when consistent.
      if (d.locked !== true) {
        return;
      }
      void this.refetchForLockChange(id);
      return;
    }
    const node = this.findFolder(tree, fid);
    if (!node) {
      // Folder unresolvable from this tree (not loaded yet / unknown id) —
      // INDETERMINATE → the safe path: refetch (the gated backend masks if
      // needed). Never the skip path (security constraint: when in doubt,
      // never suppress a possible unlocked→locked transition). No local mask
      // here — masking is only done on a POSITIVELY derived seal.
      void this.refetchForLockChange(id);
      return;
    }
    const sealed = node.locked && !node.unlocked;
    if (sealed === (d.locked === true)) {
      return; // consistent — no lock transition for THIS meeting
    }
    if (sealed) {
      // Locked transition: mask SYNCHRONOUSLY before any refetch/render, so
      // even a detached tab's signals hold no plaintext from this moment on.
      this.maskLocally(d);
    }
    // Converge on the backend's real (masked or unmasked) DTO.
    void this.refetchForLockChange(id);
  };

  /**
   * Synchronously blank every plaintext-bearing signal for a just-sealed
   * meeting (the masked shape mirrors the backend's masked DTO), stop+unload
   * any audio element (F4 hardening — a paused-but-resumable `<audio>` must
   * not outlive the lock), and re-title the tab strip so no real title
   * lingers in the strip or the persisted `murmur.tabs.v1` (F3).
   */
  private maskLocally(d: MeetingDetail): void {
    this.audioPanel()?.stopAndUnload();
    this.detail.set({
      ...d,
      meeting: { ...d.meeting, title: "🔒 Locked", audioPath: null },
      note: null,
      segments: [],
      assistantInteractions: [],
      locked: true,
    });
    // Plaintext-bearing side signals the lock gate doesn't unmount fast enough
    // to excuse: transcript segments, timeline topics/speakers, tags, graph
    // entities, editor draft. The audio binding now reads `segments()`, so a
    // lock transition MUST drop any fetched transcript (and reset the latch so a
    // later unlock re-fetches on the next Audio-tab open).
    this.segments.set([]);
    this._segmentsAttempted.set(null);
    this.timeline.set(null);
    this.speakerSuggestions.set([]);
    this.tags.set([]);
    this.graph.set(null);
    this.editing.set(false);
    this.draft.set("");
    this.attachments.set([]);
    this.meetingAttachmentBusy.set(false);
    // Close the Ask drawer on a lock transition so it doesn't reappear on a
    // later unlock (the `@if (askDrawerOpen() && !locked())` guard already hides
    // it while locked; this makes a re-summon deliberate, matching default-closed).
    this._askDrawerOpen.set(false);
    // Receipts leak WHEN/BY-WHOM: blank them (and any pending seek) synchronously
    // on the mask, matching the note/segments/audio the masked DTO already nulls.
    this.receipts.set([]);
    this.seekTarget.set(null);
    this.tabsService.setTitle(tabKeyFor("meeting", d.meeting.id), "🔒 Locked");
  }

  /** The lock-effect refetch body — re-fetch + swap `detail`, stale-guarded. */
  private async refetchForLockChange(id: string): Promise<void> {
    try {
      const fresh = await this.ipc.getMeetingDetail(id);
      // Stale-result guard: drop this if the user has since navigated elsewhere
      // within this same (possibly backgrounded) component instance.
      if (this.detail()?.meeting.id === id) {
        this.detail.set(fresh);
        // Keep the tab strip + persisted tab title truthful (F3): after a
        // lock this is the backend's "🔒 Locked", after an unlock the real one.
        if (fresh?.meeting.title) {
          this.tabsService.setTitle(tabKeyFor("meeting", id), fresh.meeting.title);
        }
      }
    } catch {
      // Best-effort — a failed re-check leaves the previous (possibly now
      // stale) detail in place rather than risk clobbering it with nothing.
    }
  }

  constructor() {
    // ROOT lock effect (see `_lockMaskHandler`'s doc) + its explicit teardown.
    const lockEffectRef = effect(
      () => {
        const tree = this.folders.tree();
        untracked(() => this._lockMaskHandler(tree));
      },
      { injector: this.envInjector },
    );
    this.destroyRef.onDestroy(() => lockEffectRef.destroy());
  }

  readonly timelineError = signal(false);
  /**
   * PERF/OOM: true when deriving THIS install's timeline would load a residency-bound on-device
   * model (local GGUF / Ollama / Apple FM). When true, generation is HEAVY and is NEVER auto-fired
   * on Audio-tab open — it hides behind an explicit "Generate timeline" click so a passive open can't
   * swap-death-beachball the Mac (perf-memory-audit). Cloud (false) auto-generates (cheap). Loaded
   * once per meeting-open from `timeline_generation_on_device` (install-global; best-effort).
   */
  readonly timelineOnDevice = signal(false);
  /**
   * True when the Audio tab opened, there is NO cached timeline, and generation is on-device (heavy)
   * — so we show the "Generate timeline" affordance instead of silently loading a multi-GB model.
   */
  readonly timelineNeedsGeneration = signal(false);
  /**
   * Speaker voiceprint suggestions (opt-in) — one per diarized `others-{n}` lane
   * the backend re-identified against a prior labeled voiceprint. Fed to the
   * timeline as the "Looks like [[Anna]]?" chip. Loaded best-effort alongside the
   * timeline; empty when the opt-in is off, the meeting is locked, or nothing matched.
   */
  readonly speakerSuggestions = signal<SpeakerSuggestion[]>([]);

  // --- Pin-this-moment (timeline (pin) → pinMoment IPC + clipboard) --------
  /** Transient confirmation after a successful pin, e.g. "Pinned 2:14 — …". */
  readonly pinMsg = signal("");
  /** Inline error surfaced when a pin (or its clipboard copy) fails. */
  readonly pinError = signal("");
  /** True while a pinMoment IPC call is in flight (debounces rapid clicks). */
  readonly pinning = signal(false);
  /** Tracked so we can cancel the pending pin-confirmation reset on destroy. */
  private pinResetTimer: ReturnType<typeof setTimeout> | null = null;

  // --- Connect-to-graph (linkMeetingEntities → People/ & Projects/ stubs) --
  /** True while a linkMeetingEntities IPC call is in flight. */
  readonly linking = signal(false);
  /** The resolved graph entities after a successful link (null until run). */
  readonly graph = signal<GraphPayload | null>(null);
  /** Inline error surfaced when the graph link fails. */
  readonly graphError = signal("");

  /**
   * Total length for the shared timeline scale: the meeting duration, falling
   * back to the furthest end across speakers / topics / transcript segments.
   */
  readonly timelineTotal = computed(() => {
    const dur = this.detail()?.meeting.durationS ?? 0;
    if (dur > 0) {
      return dur;
    }
    let max = 0;
    const tl = this.timeline();
    for (const s of tl?.speakers ?? []) {
      max = Math.max(max, s.endS);
    }
    for (const t of tl?.topics ?? []) {
      max = Math.max(max, t.endS);
    }
    for (const seg of this.segments()) {
      max = Math.max(max, seg.endS);
    }
    return max;
  });

  async ngOnInit(): Promise<void> {
    const id = this.route.snapshot.paramMap.get("id");
    if (!id) {
      this.loading.set(false);
      return;
    }
    await this.loadMeeting(id);
  }

  /**
   * Open a semantically-related meeting as its own tab. `TabRouteReuseStrategy`
   * gives every distinct `/meeting/:id` its OWN component instance (keyed by
   * id, not just the route config) — so unlike the old default
   * `RouteReuseStrategy` (which silently reused THIS component across ids and
   * forced a manual `loadMeeting` reload here), navigating away now spins up a
   * fresh `DetailComponent` whose own `ngOnInit` loads the related meeting.
   * Nothing to reload in place anymore.
   */
  async openRelated(id: string): Promise<void> {
    if (!id || this.detail()?.meeting.id === id) {
      return;
    }
    await this.tabsService.openMeeting(id);
  }

  /**
   * The Share-panel precondition gate's CTA — route to Settings (the Account
   * section hosts the sharing server / sign-in / unlock controls). Fired via the
   * panel's `setupSharing` output.
   */
  async goToSharingSettings(): Promise<void> {
    await this.router.navigate(["/settings"]);
  }

  /**
   * Refresh the "Shared with…" header pill from `meetingOrgShares` — a real
   * meeting-id join (gated backend-side exactly like `getMeetingDetail`), so a
   * locked meeting always resolves to `[]`. Fails closed to `[]` on any error.
   *
   * STALE-RESULT guard (FE failure mode #4): capture the meeting id at call time
   * and, after the await, drop the result if the user navigated to another
   * meeting mid-flight (`openRelated` reuses this component) — otherwise a late
   * response could paint the PREVIOUS meeting's org badges over the current one.
   */
  private async refreshOrgShared(): Promise<void> {
    const id = this.detail()?.meeting.id;
    if (!id) {
      this.orgShares.set([]);
      return;
    }
    try {
      const shares = await this.ipc.meetingOrgShares(id);
      // Drop late responses for a meeting the user has since navigated away from.
      if (this.detail()?.meeting.id !== id) {
        return;
      }
      // Defensive: `orgShared` reads `.length` unconditionally — never let a
      // malformed/non-array response (e.g. an unmocked IPC command resolving
      // to its generic `null` default) null-deref the computed.
      this.orgShares.set(Array.isArray(shares) ? shares : []);
    } catch {
      this.orgShares.set([]);
    }
  }

  /** The share-panel reported a create/revoke — refresh the org-shared pill. */
  async onShareChanged(): Promise<void> {
    await this.refreshOrgShared();
  }

  /**
   * Load (or reload) a meeting by id into the view. Resets the per-meeting
   * signals that aren't derived from `detail()` so an in-place reload never
   * shows the previous meeting's timeline/tags/graph or a stale open editor.
   * (Derived state — note/audio/interactions/folderBadge — recomputes off
   * `detail()` automatically.)
   */
  private async loadMeeting(id: string): Promise<void> {
    this.loading.set(true);
    // Clear non-derived per-meeting state for a clean same-route reload.
    this.timeline.set(null);
    this.timelineError.set(false);
    this.timelineNeedsGeneration.set(false);
    // Reset the Audio-tab one-shot latch so the next meeting-open may attempt its timeline read once.
    this._timelineAttempted.set(null);
    // Same for the lazy transcript segments (drop the previous meeting's rows +
    // reset its latch so the next Audio-tab open re-fetches for this meeting).
    this.segments.set([]);
    this._segmentsAttempted.set(null);
    this.speakerSuggestions.set([]);
    this.tags.set([]);
    this.graph.set(null);
    this.graphError.set("");
    this.attachments.set([]);
    this.meetingAttachmentBusy.set(false);
    this.editing.set(false);
    this.renaming.set(false);
    this.moveOpen.set(false);
    this.confirmingDelete.set(false);
    // Receipts (PR-5): drop the previous meeting's chips + any pending seek so a
    // same-route reload never carries a stale claim→audio mapping (the `_loadReceipts`
    // effect refetches for the new meeting once `detail()` resolves below).
    this.receipts.set([]);
    this.seekTarget.set(null);
    // Land on the Note tab for every meeting (identity-first default).
    this.activeTab.set("note");
    // (Audio-playback state now lives in <app-audio-panel>, which owns the
    // <audio> element + currentTime/duration/playing signals. The panel is
    // re-instantiated per active tab, so there is nothing to reset here.)
    try {
      this.detail.set(await this.ipc.getMeetingDetail(id));
    } finally {
      this.loading.set(false);
    }
    // Adopt the loaded title into the tab strip (a no-op if this meeting isn't
    // tab-tracked, e.g. a direct routerLink open elsewhere in the app).
    const loadedTitle = this.detail()?.meeting.title;
    if (loadedTitle) {
      this.tabsService.setTitle(tabKeyFor("meeting", id), loadedTitle);
    }
    // Whether this install keeps hi-res masters — gates the master-export
    // actions. Install-global, so load it regardless of lock state (best-effort;
    // a failure simply hides the actions). The backend remains the real gate.
    try {
      const cfg = await this.ipc.getConfig();
      this.config.set(cfg);
      this.keepsMasters.set(cfg.keepHiresMasters);
    } catch {
      this.config.set(null);
      this.keepsMasters.set(false);
    }
    // Org Brain badge (best-effort, non-blocking; hidden on any failure).
    void this.refreshOrgShared();
    // PERF/OOM: is timeline generation heavy (on-device) on this install? Decides auto-generate vs
    // the explicit "Generate" gate on the Audio tab. Best-effort; a failure defaults to "heavy"
    // (safer: never a surprise multi-GB load on open).
    try {
      this.timelineOnDevice.set(await this.ipc.timelineGenerationOnDevice());
    } catch {
      this.timelineOnDevice.set(true);
    }
    // Locked (masked) meetings render the lock gate only — skip priming the
    // timeline/tags (they're empty/masked) and focus the Unlock button instead.
    if (this.locked()) {
      afterNextRender(() => this.unlockButton()?.nativeElement.focus(), {
        injector: this.injector,
      });
      return;
    }
    // PERF/OOM (P0.1): do NOT generate the timeline on open. It only renders on the Audio tab
    // (default is Note), and `get_timeline` on a fresh meeting runs an on-device LLM over the WHOLE
    // transcript — with a local heavy model (Bielik-11B, 6.3 GB, never-evict) that multi-GB load
    // on every open OOM-killed the Mac. It is now generated LAZILY when the Audio tab first opens
    // (`_timelineOnAudioTab` effect below). See docs/research/2026-07-07-perf-memory-audit.md.
    if (this.detail()) {
      // Prime the folder tree so the read-only folder/lock badge + the move
      // picker have state on a direct navigation. `ensureLoaded` (NOT
      // `load()`) — the root component already loads the tree at boot, and an
      // unconditional reload here published a new tree per tab-open, firing
      // every open tab's lock effect (the perf-audit O(N²) refetch stampede,
      // fix 1b). Non-blocking — a failure just hides the badge.
      void this.folders.ensureLoaded();
      // Load the meeting's tags (best-effort; failure leaves the chips empty).
      try {
        this.tags.set(await this.ipc.getMeetingTags(id));
      } catch {
        this.tags.set([]);
      }
    }
  }

  // --- Ask drawer ----------------------------------------------------------

  /**
   * Toggle the "Ask about this meeting" slideout drawer. On OPEN, focus the
   * chat composer once it has rendered (zoneless-safe `afterNextRender` with the
   * injected `injector` — this handler runs outside the field-init context, so
   * the injector must be passed; no setTimeout). A no-op focus if the textarea
   * isn't found (e.g. reduced to nothing) — never throws.
   */
  toggleAskDrawer(): void {
    const willOpen = !this._askDrawerOpen();
    this._askDrawerOpen.set(willOpen);
    if (willOpen) {
      afterNextRender(
        () => {
          document
            .querySelector<HTMLTextAreaElement>(".ask-drawer .chat-input")
            ?.focus();
        },
        { injector: this.injector },
      );
    }
  }

  /** Close the Ask drawer (the chat's × / the reactive lock guard). */
  closeAskDrawer(): void {
    this._askDrawerOpen.set(false);
  }

  // --- Move to folder ------------------------------------------------------

  /** Open/close the folder-picker popover (closed while the detail is busy). */
  toggleMove(): void {
    if (this.busy()) {
      return;
    }
    this.moveOpen.update((v) => !v);
  }

  /** Dismiss the folder-picker popover. */
  closeMove(): void {
    this.moveOpen.set(false);
  }

  /**
   * Apply a completed move locally: patch the in-memory meeting's `folderId` so
   * the header badge updates immediately (the picker already moved it via the
   * service + reloaded the tree). Then close the popover.
   */
  onMoved(folderId: string | null): void {
    this.detail.update((d) =>
      d ? { ...d, meeting: { ...d.meeting, folderId } } : d,
    );
    this.closeMove();
  }

  /** Depth-first search for a folder node by id across the forest. */
  private findFolder(nodes: FolderNode[], id: string): FolderNode | null {
    for (const n of nodes) {
      if (n.id === id) {
        return n;
      }
      const hit = this.findFolder(n.children, id);
      if (hit) {
        return hit;
      }
    }
    return null;
  }

  // --- Phase 0.5 lock gate -------------------------------------------------

  /**
   * Unlock this meeting's owning folder via the biometric (Touch ID) path, then
   * RE-FETCH the now-unmasked detail and replace the `detail` signal in place so
   * the note/transcript/audio/timeline render. The IPC returning null (root /
   * already-open folder) is still treated as success — we re-fetch regardless.
   * On failure (biometric denied / cancelled / error) we surface a toast and
   * stay gated. Uses await (no subscribe-for-state); the button shows a pending
   * state while in flight. Once unmasked, the timeline + tags are primed too.
   */
  async unlock(): Promise<void> {
    const id = this.detail()?.meeting.id;
    if (!id || this.unlocking()) {
      return;
    }
    this.unlocking.set(true);
    try {
      // Run the biometric unlock_folder path for the meeting's folder.
      await this.ipc.unlockMeeting(id);
      this.unlockFailed.set(false);
      // Re-fetch the now-unmasked detail and swap it in place. A null detail
      // (deleted out from under us) keeps the not-found state honest.
      const fresh = await this.ipc.getMeetingDetail(id);
      this.detail.set(fresh);
      if (fresh && !fresh.locked) {
        // Refresh the folder tree so the header lock badge reflects the unlock,
        // then prime the tags the masked load skipped. Non-blocking. The timeline is
        // NOT generated here (P0.1) — it loads lazily when the Audio tab opens.
        void this.folders.load();
        try {
          this.tags.set(await this.ipc.getMeetingTags(id));
        } catch {
          this.tags.set([]);
        }
      }
    } catch (e) {
      // Biometric denied / cancelled, or the unlock errored — stay gated.
      //
      // This used to render the RAW backend error on purpose, so a field screenshot of a signed
      // build showed the keychain OSStatus or a "content-key unwrap failed". P3 ends that: the same
      // channel carries key-material vocabulary (KEK/CK/"mutex poisoned"), and the lock gate is the
      // last surface that may leak it. A cancel and an auth failure are still told apart — by the
      // `[touch-id-*]` code under the "unlock" context, not by prose — and the escape hatch below
      // is unchanged, so a genuinely lost key is still recoverable.
      this.toast.danger(this.errorCopy.humanize(e, "unlock"));
      // Reveal the reset escape hatch — the key may be genuinely gone (the backend still re-proves
      // non-recoverability before it will discard anything).
      this.unlockFailed.set(true);
    } finally {
      this.unlocking.set(false);
    }
  }

  /**
   * Discard an UNRECOVERABLE folder's lock (the escape hatch). The backend re-proves the key cannot
   * be recovered and REFUSES if it can (routing back to a normal unlock), so this never destroys
   * openable content. On success the folder reopens (emptied) and we re-fetch the now-open detail.
   */
  async discardLock(): Promise<void> {
    const id = this.detail()?.meeting.id;
    if (!id || this.discarding()) {
      return;
    }
    this.discarding.set(true);
    try {
      // The backend resolves the meeting's folder, RE-PROVES the key is unrecoverable, and REFUSES
      // if it is actually recoverable — so this can never destroy openable content.
      await this.ipc.discardUnrecoverableMeetingLock(id);
      this.unlockFailed.set(false);
      this.confirmDiscard.set(false);
      const fresh = await this.ipc.getMeetingDetail(id);
      this.detail.set(fresh);
      void this.folders.load();
      this.toast.success(
        "Folder reset — its locked contents were unrecoverable and have been cleared.",
      );
    } catch (e) {
      // Most importantly: the backend REFUSES when the folder is actually recoverable.
      this.toast.danger(this.errorCopy.because("Couldn’t reset", e));
    } finally {
      this.discarding.set(false);
    }
  }

  /**
   * READ the CACHED timeline on Audio-tab open, then decide how to derive a missing one:
   *   - cached content present → show it;
   *   - none + CLOUD provider (cheap) → generate inline;
   *   - none + ON-DEVICE provider (heavy) → surface the "Generate" affordance, NEVER auto-load a
   *     multi-GB model on a passive open (perf-memory-audit — the whole-Mac beachball).
   * Also the Retry path (a failed read/gen re-runs this).
   */
  async loadTimeline(): Promise<void> {
    const id = this.detail()?.meeting.id;
    // In-flight guard: never start a second read/generation while one is running (the Audio-tab
    // effect could otherwise re-fire). P0.4.
    if (!id || this.timelineLoading()) {
      return;
    }
    // ONE-SHOT LATCH (#234): record that this meeting's Audio-tab timeline read has been attempted,
    // so the `_timelineOnAudioTab` effect can never re-enter for it even when the read resolves to
    // an empty/falsy timeline that sets no terminal signal. A deliberate Retry clears the terminal
    // signals and re-calls this method directly; the latch is not consulted there.
    this._timelineAttempted.set(id);
    this.timelineError.set(false);
    this.timelineNeedsGeneration.set(false);
    this.timelineLoading.set(true);
    try {
      const cached = await this.ipc.getTimeline(id);
      // STALE-RESULT guard: if the user switched meetings mid-flight, drop this result so we never
      // paint meeting A's timeline over meeting B (mirrors `resummarize`). P0.4.
      if (this.detail()?.meeting.id !== id) {
        return;
      }
      if (cached && (cached.speakers.length > 0 || cached.topics.length > 0)) {
        this.timeline.set(cached);
      } else if (this.timelineOnDevice()) {
        // Heavy on-device generation is user-initiated only — show the Generate affordance.
        this.timeline.set(null);
        this.timelineNeedsGeneration.set(true);
        return;
      } else {
        // Cloud is cheap → derive now under the same in-flight flag.
        await this.deriveTimeline(id);
      }
    } catch {
      if (this.detail()?.meeting.id === id) {
        this.timeline.set(null);
        this.timelineError.set(true);
      }
    } finally {
      this.timelineLoading.set(false);
    }
    // Voiceprint speaker suggestions (opt-in) — best-effort, never blocks the
    // timeline. Empty when the feature is off / meeting locked / nothing matched.
    void this.loadSpeakerSuggestions();
  }

  /**
   * EXPLICIT heavy generation — the on-device "Generate timeline" click. Runs the multi-GB model
   * pass deliberately (user asked for it), stale-guarded, under the shared in-flight flag.
   */
  async generateTimeline(): Promise<void> {
    const id = this.detail()?.meeting.id;
    if (!id || this.timelineLoading()) {
      return;
    }
    this.timelineError.set(false);
    this.timelineNeedsGeneration.set(false);
    this.timelineLoading.set(true);
    try {
      await this.deriveTimeline(id);
    } finally {
      this.timelineLoading.set(false);
    }
    void this.loadSpeakerSuggestions();
  }

  /**
   * Run the backend `generate_timeline` (the heavy provider pass) and land the result, stale-guarded.
   * The CALLER owns `timelineLoading`; this only writes `timeline`/`timelineError`.
   */
  private async deriveTimeline(id: string): Promise<void> {
    try {
      const tl = await this.ipc.generateTimeline(id);
      const current = this.detail();
      if (current?.meeting.id !== id || current.locked) {
        return; // stale — the user moved on or screen-share/manual relock revoked the view
      }
      // TERMINAL-STATE guard (#234): a falsy/empty generate_timeline result must NOT leave
      // `timeline==null && !error && !needsGeneration` — that combination re-fired the Audio-tab
      // effect forever. An empty derivation is a resolved failure to produce content → surface the
      // Retry affordance (timelineError) rather than a silent, effect-re-triggering blank.
      if (tl && (tl.speakers.length > 0 || tl.topics.length > 0)) {
        this.timeline.set(tl);
      } else {
        this.timeline.set(null);
        this.timelineError.set(true);
      }
    } catch {
      if (this.detail()?.meeting.id === id) {
        this.timeline.set(null);
        this.timelineError.set(true);
      }
    }
  }

  /**
   * Load the opt-in voiceprint speaker suggestions for the current meeting into
   * `speakerSuggestions`. Best-effort: any failure (feature off, no models,
   * locked) just leaves the chips absent — never a crash, never blocks the view.
   */
  private async loadSpeakerSuggestions(): Promise<void> {
    const id = this.detail()?.meeting.id;
    if (!id) {
      this.speakerSuggestions.set([]);
      return;
    }
    try {
      this.speakerSuggestions.set(await this.ipc.suggestSpeakerLabels(id));
    } catch {
      this.speakerSuggestions.set([]);
    }
  }

  /**
   * Pin the timeline's current moment: derive a short label (the topic span
   * under the playhead, else "Pinned moment"), call `pinMoment` to write a
   * `^block-ref` + obsidian:// deep link, copy the link to the clipboard, then
   * flash a brief confirmation. Errors surface inline; nothing else is touched.
   */
  async onPin(seconds: number): Promise<void> {
    const id = this.detail()?.meeting.id;
    if (!id || this.pinning()) {
      return;
    }
    this.pinError.set("");
    this.pinning.set(true);
    try {
      const result = await this.ipc.pinMoment(
        id,
        seconds,
        this.pinLabel(seconds),
      );
      try {
        await navigator.clipboard.writeText(result.url);
      } catch {
        // Pin still landed in the note; only the clipboard copy was refused.
      }
      this.flashPin(`Pinned ${result.mmss} — Obsidian link copied`);
    } catch (e) {
      this.pinError.set(this.errorCopy.because("Couldn’t pin", e));
    } finally {
      this.pinning.set(false);
    }
  }

  /** Short pin label: the topic span containing `seconds`, else a default. */
  private pinLabel(seconds: number): string {
    const topic = this.timeline()?.topics.find(
      (t) => seconds >= t.startS && seconds < t.endS,
    );
    return topic?.label?.trim() || "Pinned moment";
  }

  /**
   * Apply a manual speaker re-label from the timeline legend (e.g. "User 1" →
   * "Sarah"): call `renameSpeaker`, then fold the returned timeline into the
   * `timeline` signal so the lanes + legend relabel immediately. Errors are
   * handled silently inline — the previous timeline stays put, no crash.
   */
  async onRenameSpeaker(change: {
    oldLabel: string;
    newLabel: string;
  }): Promise<void> {
    const id = this.detail()?.meeting.id;
    if (!id) {
      return;
    }
    try {
      this.timeline.set(
        await this.ipc.renameSpeaker(id, change.oldLabel, change.newLabel),
      );
      // The relabel enrols this cluster's voiceprint (opt-in) and clears its
      // suggestion — re-fetch so the accepted chip drops and any newly-resolvable
      // cluster surfaces. Best-effort; a failure just leaves the chips as they were.
      void this.loadSpeakerSuggestions();
    } catch {
      // Keep the existing timeline; the relabel simply didn't take.
    }
  }

  /** Show the pin confirmation for a moment (tracked timeout — cancelled on destroy). */
  private flashPin(message: string): void {
    this.pinMsg.set(message);
    if (this.pinResetTimer) {
      clearTimeout(this.pinResetTimer);
    }
    this.pinResetTimer = setTimeout(() => this.pinMsg.set(""), 3200);
    this.destroyRef.onDestroy(() => {
      if (this.pinResetTimer) {
        clearTimeout(this.pinResetTimer);
      }
    });
  }

  /**
   * Connect this meeting to the Obsidian vault graph: resolve its people +
   * projects into `People/` / `Projects/` stub notes with backlinks, then show
   * the resolved entities as chips. Gated on a note existing. Errors inline.
   */
  async linkGraph(): Promise<void> {
    const id = this.detail()?.meeting.id;
    if (!id || !this.note() || this.linking()) {
      return;
    }
    this.graphError.set("");
    this.linking.set(true);
    try {
      this.graph.set(await this.ipc.linkMeetingEntities(id));
    } catch (e) {
      this.graph.set(null);
      this.graphError.set(this.errorCopy.because("Couldn’t connect to graph", e));
    } finally {
      this.linking.set(false);
    }
  }

  /**
   * Re-run transcription for a recording that failed, from the audio still on disk.
   *
   * The backend command has existed all along, and both the ASR watchdog and the pipeline's
   * terminal guard tell the user in as many words to "use Retry transcription" — but nothing in the
   * app ever called it. A meeting that failed during transcription was therefore unrecoverable from
   * the interface, with its audio sitting intact on disk, and the only advice on screen pointed at
   * a control that did not exist.
   *
   * Every precondition is the backend's: no recording in flight, folder unlocked, status still
   * Error, plaintext audio present, and a single-flight claim that flips Error → Recording. This
   * only decides whether the offer is worth showing.
   */
  async retryTranscription(id: string): Promise<void> {
    if (this.retryingTranscription()) {
      return;
    }
    this.retryingTranscription.set(true);
    this.msg.set("Transcribing again…");
    try {
      await this.ipc.retryTranscription(id);
      const fresh = await this.ipc.getMeetingDetail(id);
      // Same stale-result guard as `resummarize`: transcription is long, and the user may have
      // navigated away and opened another meeting while it ran.
      if (this.detail()?.meeting?.id === id) {
        this.detail.set(fresh);
      }
      this.msg.set("Done.");
    } catch (e) {
      this.msg.set(this.errorCopy.because("Couldn’t transcribe this recording", e));
    } finally {
      this.retryingTranscription.set(false);
    }
  }

  async resummarize(id: string): Promise<void> {
    this.busy.set(true);
    this.msg.set("Re-summarizing…");
    try {
      await this.ipc.resummarize(id);
      const fresh = await this.ipc.getMeetingDetail(id);
      // Drop late responses: the user may have navigated (openRelated) mid-flight —
      // never clobber a different meeting's detail with this closure's re-fetch.
      if (this.detail()?.meeting?.id === id) {
        this.detail.set(fresh);
      }
      this.msg.set("Done.");
    } catch (e) {
      this.msg.set(this.errorCopy.because("Couldn’t rewrite the note", e));
    } finally {
      this.busy.set(false);
    }
  }

  async convertToNote(id: string, templateId: string | null): Promise<void> {
    if (this.converting()) {
      return;
    }
    const title = this.detail()?.meeting.title || "Meeting";
    const request = { meetingId: id, sequence: ++this.conversionSequence };
    this.conversionRequest.set(request);
    try {
      const result = await this.ipc.convertMeetingToNote(
        id,
        templateId ?? undefined,
      );
      if (!this.isCurrentConversion(request) || !this.isActiveMeeting(id)) {
        return;
      }
      this.toast.success("Meeting converted to a linked note");
      await this.tabsService.openNote(result.noteId, `${title} — note`);
    } catch (error) {
      if (this.isCurrentConversion(request) && this.isActiveMeeting(id)) {
        // The "convert" context, not the default "generic": every refusal on this path now
        // carries an errcode, and the context is what turns a locked folder or an active share
        // into the sentence that names the ONE thing the user has to do next.
        this.toast.danger(
          this.errorCopy.because("Couldn’t convert this meeting", error, "convert"),
        );
      }
    } finally {
      if (this.conversionRequest()?.sequence === request.sequence) {
        this.conversionRequest.set(null);
      }
    }
  }

  private isCurrentConversion(request: {
    meetingId: string;
    sequence: number;
  }): boolean {
    const current = this.conversionRequest();
    return (
      current?.sequence === request.sequence &&
      current.meetingId === request.meetingId &&
      this.detail()?.meeting.id === request.meetingId
    );
  }

  private isActiveMeeting(id: string): boolean {
    const primary = this.router.parseUrl(this.router.url).root.children["primary"];
    return primary?.segments[0]?.path === "meeting" && primary.segments[1]?.path === id;
  }

  // --- Inline title rename -------------------------------------------------

  /** Enter rename mode, seeding the draft with the meeting's current title. */
  startRename(): void {
    this.titleDraft.set(this.detail()?.meeting.title ?? "");
    this.renaming.set(true);
    // Focus the field once it has rendered (zoneless-safe; no setTimeout).
    afterNextRender(() => this.renameInput()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

  /** Mirror the rename input value into the `titleDraft` signal. */
  onTitleInput(event: Event): void {
    this.titleDraft.set((event.target as HTMLInputElement).value);
  }

  /** Leave rename mode without persisting. */
  cancelRename(): void {
    this.renaming.set(false);
  }

  /**
   * Persist the new title: ignore empty/whitespace values, await the rename
   * IPC, then fold the trimmed title into the in-memory meeting so the header
   * reflects it immediately. The rest of the page state is untouched.
   */
  async saveRename(): Promise<void> {
    const current = this.detail();
    const id = current?.meeting.id;
    const title = this.titleDraft().trim();
    if (!current || !id || !title) {
      return;
    }
    this.savingRename.set(true);
    try {
      await this.ipc.renameMeeting(id, title);
      this.detail.set({
        ...current,
        meeting: { ...current.meeting, title },
      });
      this.tabsService.setTitle(tabKeyFor("meeting", id), title);
      this.renaming.set(false);
    } catch (e) {
      this.msg.set(this.errorCopy.because("Couldn’t rename", e));
    } finally {
      this.savingRename.set(false);
    }
  }

  // --- In-app delete -------------------------------------------------------

  /** Open the signal-driven confirm panel (no window.confirm). */
  askDelete(): void {
    this.deleteError.set("");
    this.confirmingDelete.set(true);
  }

  /** Dismiss the confirm panel without deleting. */
  cancelDelete(): void {
    this.confirmingDelete.set(false);
  }

  /**
   * Irreversibly delete the meeting (recording, transcript, summary + the
   * exported vault note), then navigate back to the library. Errors surface
   * inline in the confirm panel and keep the user on the page.
   */
  async confirmDelete(id: string): Promise<void> {
    this.deleting.set(true);
    this.deleteError.set("");
    try {
      await this.ipc.deleteMeeting(id);
      await this.router.navigateByUrl("/library");
    } catch (e) {
      this.deleteError.set(this.errorCopy.because("Couldn’t delete", e));
      this.deleting.set(false);
    }
  }

  // --- In-app note editor --------------------------------------------------

  /** Enter edit mode, seeding the draft with the note's current raw markdown. */
  startEdit(): void {
    this.draft.set(this.detail()?.note?.markdown ?? "");
    this.editAttachmentSnapshot = [...this.attachments()];
    this.saveError.set("");
    this.editing.set(true);
  }

  /** Two-way bind: mirror the textarea value into the `draft` signal. */
  onDraftInput(event: Event): void {
    this.draft.set((event.target as HTMLTextAreaElement).value);
  }

  /** Discard the working copy and delete images imported only for this edit. */
  async cancelEdit(): Promise<void> {
    const meetingId = this.detail()?.meeting.id;
    if (!meetingId || this.saving() || this.meetingAttachmentBusy()) {
      return;
    }
    const originalIds = new Set(this.editAttachmentSnapshot.map((row) => row.id));
    const added = this.attachments().filter((row) => !originalIds.has(row.id));
    this.saving.set(true);
    this.saveError.set("");
    try {
      const results = await Promise.allSettled(
        added.map((row) =>
          this.ipc.deleteNoteAttachment("meeting", meetingId, row.id),
        ),
      );
      const failed = added.filter((_, index) => results[index].status === "rejected");
      if (failed.length > 0) {
        const failedIds = new Set(failed.map((row) => row.id));
        const removed = added.filter((row) => !failedIds.has(row.id));
        let reconciledDraft = this.draft();
        for (const row of removed) {
          reconciledDraft = this.removeAttachmentMarker(reconciledDraft, row.id);
        }
        this.draft.set(reconciledDraft);
        this.attachments.set([...this.editAttachmentSnapshot, ...failed]);
        this.saveError.set(
          `Couldn’t discard ${failed.length} added image${failed.length === 1 ? "" : "s"}. Retry Cancel.`,
        );
        return;
      }
      this.attachments.set([...this.editAttachmentSnapshot]);
      this.draft.set(this.detail()?.note?.markdown ?? "");
      this.editing.set(false);
    } catch (e) {
      this.saveError.set(this.errorCopy.because("Couldn’t discard added images", e));
    } finally {
      this.saving.set(false);
    }
  }

  private removeAttachmentMarker(markdown: string, attachmentId: string): string {
    const marker = new RegExp(
      `!\\[[^\\]\\r\\n]*\\]\\(murmur-attachment:\\/\\/${attachmentId}\\)`,
      "gi",
    );
    return markdown.replace(marker, "").replace(/\n{3,}/g, "\n\n");
  }

  /**
   * Persist the draft: re-write the vault file via `updateNote`, fold the
   * returned markdown back into the in-memory detail signal (so the `note()`
   * computed re-parses and the analysis cards re-render), exit edit mode, then
   * flash a brief "Saved" confirmation. Errors surface inline; the page state
   * (audio / timeline / transcript) is never touched.
   */
  async saveNote(): Promise<void> {
    const meetingId = this.detail()?.meeting.id;
    if (
      !meetingId ||
      this.meetingAttachmentBusy() ||
      this.draft().includes("murmur-pending://")
    ) {
      return;
    }
    this.saving.set(true);
    this.saveError.set("");
    try {
      const updated = await this.ipc.updateNote(meetingId, this.draft());
      const current = this.detail();
      if (!current || current.meeting.id !== meetingId || current.locked) {
        return; // a late IPC response must never repopulate content after relock/navigation
      }
      this.detail.set({ ...current, note: updated });
      const referenced = new Set(
        Array.from(
          updated.markdown.matchAll(
            /murmur-attachment:\/\/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})/gi,
          ),
          (match) => match[1].toLowerCase(),
        ),
      );
      this.attachments.update((rows) =>
        rows.filter((row) => referenced.has(row.id.toLowerCase())),
      );
      this.editAttachmentSnapshot = [...this.attachments()];
      this.editing.set(false);
      this.flashSaved();
    } catch (e) {
      this.saveError.set(this.errorCopy.because("Couldn’t save", e));
    } finally {
      this.saving.set(false);
    }
  }

  /**
   * Re-fetch the current meeting into the view — used after the Verify panel writes inline
   * markers so the rendered note reflects the newly-applied `> ` blockquotes.
   */
  async reloadDetail(): Promise<void> {
    const id = this.detail()?.meeting.id;
    if (id) {
      await this.loadMeeting(id);
    }
  }

  /** Show the "Saved" badge for a moment (tracked timeout — cancelled on destroy). */
  private flashSaved(): void {
    this.justSaved.set(true);
    if (this.savedResetTimer) {
      clearTimeout(this.savedResetTimer);
    }
    this.savedResetTimer = setTimeout(() => this.justSaved.set(false), 2200);
    this.destroyRef.onDestroy(() => {
      if (this.savedResetTimer) {
        clearTimeout(this.savedResetTimer);
      }
    });
  }

  // --- Meeting tags --------------------------------------------------------

  /** Mirror the add-tag input value into the `tagDraft` signal. */
  onTagInput(event: Event): void {
    this.tagDraft.set((event.target as HTMLInputElement).value);
  }

  /**
   * Add the typed tag: trim, ignore empty/duplicate (case-insensitive), then
   * persist the new array. Clears the input on a non-empty attempt.
   */
  async addTag(): Promise<void> {
    const tag = this.tagDraft().trim();
    if (!tag) {
      return;
    }
    const exists = this.tags().some(
      (t) => t.toLowerCase() === tag.toLowerCase(),
    );
    this.tagDraft.set("");
    if (exists) {
      return;
    }
    await this.persistTags([...this.tags(), tag]);
  }

  /** Remove a tag and persist the reduced array. */
  async removeTag(tag: string): Promise<void> {
    await this.persistTags(this.tags().filter((t) => t !== tag));
  }

  /**
   * Optimistically apply `next` to the `tags` signal, persist via
   * setMeetingTags, and roll back to the previous tags if the write fails.
   * Errors surface inline next to the editor.
   */
  private async persistTags(next: string[]): Promise<void> {
    const id = this.detail()?.meeting.id;
    if (!id) {
      return;
    }
    const previous = this.tags();
    this.tagsError.set("");
    this.tags.set(next);
    this.tagsBusy.set(true);
    try {
      await this.ipc.setMeetingTags(id, next);
    } catch (e) {
      this.tags.set(previous);
      this.tagsError.set(this.errorCopy.because("Couldn’t save tags", e));
    } finally {
      this.tagsBusy.set(false);
    }
  }

  /** Copy a path to the clipboard (no external <a href> navigation). */
  async copy(text: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(text);
      this.copied.set(true);
    } catch {
      this.copied.set(false);
    }
  }

  // --- Export menu ---------------------------------------------------------

  /**
   * Copy the note's raw markdown to the clipboard (the full source, not the
   * parsed analysis). Flashes a brief "Copied" confirmation on the button.
   */
  async copyMarkdown(): Promise<void> {
    if (this.editing()) {
      return;
    }
    const markdown = this.detail()?.note?.markdown;
    if (!markdown) {
      return;
    }
    this.exportError.set("");
    try {
      await navigator.clipboard.writeText(markdown);
      this.flashExport("md-copied");
    } catch (e) {
      this.exportError.set(this.errorCopy.because("Couldn’t copy", e));
    }
  }

  /**
   * Prompt for a destination via the native save dialog, then write the note
   * markdown there through `exportNote`. A dismissed dialog (null path) is a
   * no-op; failures surface inline.
   */
  async saveMarkdown(id: string, title: string | null): Promise<void> {
    if (this.editing() || this.exporting()) {
      return;
    }
    this.exportError.set("");
    this.exporting.set(true);
    try {
      const path = await save({
        defaultPath: `${this.sanitizeTitle(title)}.md`,
        filters: [{ name: "Markdown", extensions: ["md"] }],
      });
      if (path) {
        await this.ipc.exportNote(id, path);
        this.flashExport("md-saved");
      }
    } catch (e) {
      this.exportError.set(this.errorCopy.because("Couldn’t save markdown", e));
    } finally {
      this.exporting.set(false);
    }
  }

  /**
   * Prompt for a destination via the native save dialog, then copy the meeting
   * recording (WAV) there through `exportAudio`. Only reachable when the
   * meeting actually has audio (the button is gated on `audioSrc()`).
   */
  async saveAudio(id: string, title: string | null): Promise<void> {
    if (this.editing() || this.exporting()) {
      return;
    }
    this.exportError.set("");
    this.exporting.set(true);
    try {
      const path = await save({
        defaultPath: `${this.sanitizeTitle(title)}.wav`,
        filters: [{ name: "Audio", extensions: ["wav"] }],
      });
      if (path) {
        await this.ipc.exportAudio(id, path);
        this.flashExport("audio-saved");
      }
    } catch (e) {
      this.exportError.set(this.errorCopy.because("Couldn’t save audio", e));
    } finally {
      this.exporting.set(false);
    }
  }

  /**
   * Prompt for a destination via the native save dialog, then copy the meeting's
   * hi-res master archive (faithful per-stream float32 WAV) there through the
   * gated `exportMicMaster` / `exportSysMaster` commands — the ONLY way these
   * archives leave the app. A dismissed dialog (null path) is a no-op. The
   * backend fails closed: a sealed-and-not-unlocked folder rejects with Locked,
   * and a stream that was never archived rejects with "no master" — both are
   * mapped to a clear, actionable message (never a crash).
   */
  async exportMaster(
    stream: "mic" | "sys",
    id: string,
    title: string | null,
  ): Promise<void> {
    if (this.editing() || this.exporting()) {
      return;
    }
    this.exportError.set("");
    this.exporting.set(true);
    try {
      const path = await save({
        defaultPath: `${this.sanitizeTitle(title)}.${stream}.wav`,
        filters: [{ name: "Audio", extensions: ["wav"] }],
      });
      if (path) {
        if (stream === "mic") {
          await this.ipc.exportMicMaster(id, path);
          this.flashExport("mic-master-saved");
        } else {
          await this.ipc.exportSysMaster(id, path);
          this.flashExport("sys-master-saved");
        }
      }
    } catch (e) {
      this.exportError.set(this.masterErrorMessage(stream, e));
    } finally {
      this.exporting.set(false);
    }
  }

  /**
   * Map a master-export failure to a clear message: a Locked folder → unlock to
   * export; a missing per-stream archive → none was kept; anything else verbatim.
   */
  private masterErrorMessage(stream: "mic" | "sys", error: unknown): string {
    if (this.errorCopy.is(error, "meeting-locked")) {
      return "This meeting is locked — unlock it to export the master.";
    }
    // "no master for that stream" is an `InvalidArg` with no code (it is not a failure the user
    // can act on beyond knowing it), and it is the ONE remaining prose test on this path. It reads
    // an `export.rs` string that has no other consumer; recorded in `errcode.rs`'s prose-coupling
    // note so a future reword cannot break it silently.
    if (/no master/i.test(String(error))) {
      return stream === "mic"
        ? "No hi-res mic master was kept for this meeting."
        : "No hi-res system master was kept for this meeting.";
    }
    return this.errorCopy.because("Couldn’t export the master", error);
  }

  /**
   * Save-as-PDF via the OS print dialog. A body-level class flips on the print
   * stylesheet (isolating the note/analysis) for the duration of the synchronous
   * `window.print()` call, then is cleared so the live UI is untouched.
   */
  saveAsPdf(): void {
    if (this.editing()) {
      return;
    }
    document.body.classList.add("murmur-printing");
    try {
      window.print();
    } finally {
      document.body.classList.remove("murmur-printing");
    }
  }

  /**
   * Export this meeting as an Obsidian Canvas board: call `exportCanvas` (which
   * writes `vault/Canvas/<title>.canvas` and returns the path), then flash a
   * brief "Canvas saved" confirmation with that path. Gated on a parsed note
   * existing; errors (e.g. "open the meeting once to generate its timeline
   * first") surface inline and leave the rest of the page untouched.
   */
  async exportCanvas(id: string): Promise<void> {
    if (this.editing() || this.exportingCanvas() || !this.note()) {
      return;
    }
    this.canvasError.set("");
    this.exportingCanvas.set(true);
    try {
      const path = await this.ipc.exportCanvas(id);
      this.flashCanvas(path);
    } catch (e) {
      this.canvasError.set(this.errorCopy.because("Couldn’t export Canvas", e));
    } finally {
      this.exportingCanvas.set(false);
    }
  }

  /** Show the "Canvas saved" confirmation (tracked timeout — cancelled on destroy). */
  private flashCanvas(path: string): void {
    this.canvasMsg.set(path);
    if (this.canvasResetTimer) {
      clearTimeout(this.canvasResetTimer);
    }
    this.canvasResetTimer = setTimeout(() => this.canvasMsg.set(""), 4000);
    this.destroyRef.onDestroy(() => {
      if (this.canvasResetTimer) {
        clearTimeout(this.canvasResetTimer);
      }
    });
  }

  /**
   * Flash a transient success token on an export button (tracked timeout —
   * cancelled on destroy so we never poke a dead component).
   */
  private flashExport(token: string): void {
    this.exportMsg.set(token);
    if (this.exportResetTimer) {
      clearTimeout(this.exportResetTimer);
    }
    this.exportResetTimer = setTimeout(() => this.exportMsg.set(""), 2200);
    this.destroyRef.onDestroy(() => {
      if (this.exportResetTimer) {
        clearTimeout(this.exportResetTimer);
      }
    });
  }

  /** Build a filesystem-safe filename stem from a meeting title. */
  private sanitizeTitle(title: string | null): string {
    const cleaned = (title || "")
      .trim()
      .replace(/[\\/:*?"<>|]+/g, " ")
      .replace(/\s+/g, " ")
      .trim();
    return cleaned || "meeting-note";
  }

  // --- Markdown parsing ----------------------------------------------------

  /**
   * Strips a leading YAML front-matter block (between the first `---` and the
   * next `---`), pulls out `tags` + `participants`, then splits the remaining
   * body into `## ` sections. Falls back to raw markdown when no section is
   * found.
   */
  /**
   * Enrich a raw persisted interaction with a stable id + parsed citations. The
   * backend stores citations as plain strings: `[[Title]]` for a vault source,
   * or a bare URL / `(web)` marker for a web source. We split the two so the
   * template can render `[[vault]]` chips vs distinct "via web" links.
   */
  private parseInteraction(i: AssistantInteraction, idx: number): AssistantQa {
    return {
      id: `${i.createdAt}#${idx}`,
      command: i.command,
      answer: i.answer,
      citations: (i.citations ?? []).map((c) => this.parseCitation(c)),
      status: i.status,
      sourceLabel: i.sourceLabel,
      createdAt: i.createdAt,
    };
  }

  /** Split one persisted citation string into a vault- vs web-shaped chip. */
  private parseCitation(raw: string): ParsedCitation {
    const c = raw.trim();
    // A bare http(s) URL → web link.
    if (/^https?:\/\//i.test(c)) {
      return { kind: "web", label: this.hostOf(c) ?? c, url: c };
    }
    // `[[Title]]` (or `Title`) → vault chip; strip the wikilink brackets.
    const wiki = /^\[\[(.+?)\]\]$/.exec(c);
    if (wiki) {
      return { kind: "vault", label: wiki[1].trim(), url: null };
    }
    // `(web)` / `web` marker with no URL → a labelless web source.
    if (/^\(?web\)?$/i.test(c)) {
      return { kind: "web", label: "web", url: null };
    }
    // `Label (https://…)` form → web link with a friendly label.
    const labelled = /^(.*?)\s*\((https?:\/\/[^)]+)\)$/i.exec(c);
    if (labelled) {
      return {
        kind: "web",
        label: labelled[1].trim() || this.hostOf(labelled[2]) || labelled[2],
        url: labelled[2],
      };
    }
    // Fallback: treat as a vault title (no off-device origin implied).
    return { kind: "vault", label: c, url: null };
  }

  /** Best-effort host extraction for a web citation label; null if unparseable. */
  private hostOf(url: string): string | null {
    try {
      return new URL(url).host;
    } catch {
      return null;
    }
  }

  private parseNote(markdown: string): ParsedNote {
    const lines = markdown.replace(/\r\n/g, "\n").split("\n");

    let tags: string[] = [];
    let participants: string[] = [];
    let enhanced = false;
    let bodyStart = 0;

    // Front-matter must be the very first non-empty content.
    if (lines[0]?.trim() === "---") {
      const end = lines.findIndex((l, i) => i > 0 && l.trim() === "---");
      if (end > 0) {
        const fm = lines.slice(1, end);
        tags = this.readFrontMatterList(fm, "tags");
        participants = this.readFrontMatterList(fm, "participants");
        enhanced = fm.some((l) => /^murmur_enhanced\s*:\s*true\b/i.test(l.trim()));
        bodyStart = end + 1;
      }
    }

    const body = lines.slice(bodyStart);
    const sections: NoteSection[] = [];
    let current: { heading: string; lines: string[] } | null = null;

    for (const line of body) {
      const headingMatch = /^##\s+(.*)$/.exec(line);
      if (headingMatch) {
        if (current) {
          sections.push(this.buildSection(current.heading, current.lines));
        }
        current = { heading: headingMatch[1].trim(), lines: [] };
      } else if (current) {
        current.lines.push(line);
      }
    }
    if (current) {
      sections.push(this.buildSection(current.heading, current.lines));
    }

    if (sections.length === 0) {
      // No structured sections — surface the body (sans front-matter) raw.
      const raw = body.join("\n").trim();
      return { tags, participants, sections: [], raw: raw || markdown.trim(), enhanced };
    }

    return { tags, participants, sections, raw: null, enhanced };
  }

  /** Classify a section by its heading + content, then shape its data. */
  private buildSection(heading: string, lines: string[]): NoteSection {
    const trimmed = lines.map((l) => l.trim());

    // Action-items: lines like "- [ ] text" / "- [x] text".
    const actions: ActionItem[] = [];
    for (const l of trimmed) {
      const m = /^[-*]\s+\[( |x|X)\]\s+(.*)$/.exec(l);
      if (m) {
        actions.push({ done: m[1].toLowerCase() === "x", text: m[2].trim() });
      }
    }
    const headingIsActions = /action/i.test(heading);
    if (actions.length > 0 || headingIsActions) {
      return {
        heading,
        kind: "actions",
        paragraphs: [],
        bullets: [],
        actions,
      };
    }

    // Plain bullet list: "- text" / "* text" (strip the marker).
    const bullets: string[] = [];
    let nonBulletContent = false;
    for (const l of trimmed) {
      if (!l) {
        continue;
      }
      const m = /^[-*]\s+(.*)$/.exec(l);
      if (m) {
        bullets.push(m[1].trim());
      } else {
        nonBulletContent = true;
      }
    }
    if (bullets.length > 0 && !nonBulletContent) {
      return { heading, kind: "bullets", paragraphs: [], bullets, actions: [] };
    }

    // Otherwise prose: collapse blank-line-separated paragraphs.
    const paragraphs: string[] = [];
    let buf: string[] = [];
    const flush = (): void => {
      if (buf.length) {
        paragraphs.push(buf.join(" ").trim());
        buf = [];
      }
    };
    for (const l of trimmed) {
      if (l) {
        buf.push(l);
      } else {
        flush();
      }
    }
    flush();

    return { heading, kind: "prose", paragraphs, bullets: [], actions: [] };
  }

  /**
   * Reads a YAML list value for `key` — supports both inline
   * (`tags: [a, b]`) and block (`tags:` then `  - a`) styles.
   */
  private readFrontMatterList(fm: string[], key: string): string[] {
    const idx = fm.findIndex((l) =>
      new RegExp(`^${key}\\s*:`, "i").test(l.trim()),
    );
    if (idx === -1) {
      return [];
    }

    const line = fm[idx].trim();
    const inline = line.slice(line.indexOf(":") + 1).trim();

    if (inline) {
      // Inline list "[a, b]" or comma/space separated scalars.
      return inline
        .replace(/^\[/, "")
        .replace(/\]$/, "")
        .split(",")
        .map((s) => this.cleanScalar(s))
        .filter((s) => s.length > 0);
    }

    // Block list: subsequent "  - item" lines.
    const out: string[] = [];
    for (let i = idx + 1; i < fm.length; i++) {
      const m = /^\s*-\s+(.*)$/.exec(fm[i]);
      if (!m) {
        break;
      }
      const v = this.cleanScalar(m[1]);
      if (v) {
        out.push(v);
      }
    }
    return out;
  }

  /** Strip surrounding quotes/whitespace from a YAML scalar. */
  private cleanScalar(s: string): string {
    return s.trim().replace(/^["']/, "").replace(/["']$/, "").trim();
  }

  /** Presentational: stored timestamp → friendly local date. */
  formatDate(startedAt: string): string {
    const d = new Date(startedAt);
    if (Number.isNaN(d.getTime())) return startedAt;
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  }

  /** Presentational: seconds → compact "Hh Mm" / "Mm Ss" / "Ss". */
  formatDuration(durationS: number): string {
    const total = Math.max(0, Math.round(durationS));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    if (h > 0) return `${h}h ${m}m`;
    if (m > 0) return `${m}m ${s}s`;
    return `${s}s`;
  }
}
