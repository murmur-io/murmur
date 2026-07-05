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
import { ActivatedRoute, Router, RouterLink } from "@angular/router";
import { convertFileSrc } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { IpcService } from "../../../core/ipc.service";
import type {
  AssistantInteraction,
  FolderNode,
  GraphPayload,
  MeetingDetail,
  MeetingTimeline,
  RecipientPreview,
  Segment,
  SpeakerSuggestion,
} from "../../../core/models";
import {
  FoldersService,
  type FolderExposure,
} from "../../../services/folders.service";
import { ToastService } from "../../../services/toast.service";
import { MarkdownComponent } from "../../../shared/markdown/markdown.component";
import { AssistantSourcesComponent } from "../../../shared/assistant-sources/assistant-sources.component";
import { LockBadgeComponent } from "../../folders/lock-badge/lock-badge.component";
import { MoveToMenuComponent } from "../../folders/move-to-menu/move-to-menu.component";
import { MeetingActionsComponent } from "../meeting-actions/meeting-actions.component";
import { MeetingChatComponent } from "../meeting-chat/meeting-chat.component";
import { MeetingRecipesComponent } from "../meeting-recipes/meeting-recipes.component";
import { MeetingTimelineComponent } from "../meeting-timeline/meeting-timeline.component";
import { RelatedMeetingsComponent } from "../related-meetings/related-meetings.component";
import {
  ShareVerifySheetComponent,
  type ShareVerifyMode,
} from "../share-verify-sheet/share-verify-sheet.component";

/** One checklist entry parsed from a `- [ ]` / `- [x]` action-item line. */
interface ActionItem {
  done: boolean;
  text: string;
}

/**
 * M5-CLIENT — the step the in-flow "Share with a person" panel is showing.
 * The floating fingerprint sheet is tracked separately (`verifyMode`).
 */
type PersonShareStep = "email" | "suggest-link" | "consent" | "result";

/** A parsed `## Heading` section of the note body. */
interface NoteSection {
  heading: string;
  /** Normalised kind drives which renderer the template uses. */
  kind: "actions" | "bullets" | "prose";
  /** Plain prose paragraphs (kind === 'prose'). */
  paragraphs: string[];
  /** Bullet lines, leading marker stripped (kind === 'bullets'). */
  bullets: string[];
  /** Checklist entries (kind === 'actions'). */
  actions: ActionItem[];
}

/**
 * One grounding citation, parsed from the persisted `string[]` the backend
 * stores per interaction. The backend writes `[[Title]]` for a vault source and
 * a `(web)` / `(https://…)` form for a web source — we split the two so the FE
 * can render `[[vault]]` chips vs distinct "via web" links (mirroring the live
 * assistant-actions card, whose live store carries structured citations).
 */
interface ParsedCitation {
  kind: "vault" | "web";
  /** Display label (vault title, or the host/label for a web source). */
  label: string;
  /** Resolved URL for a web source; null for a vault citation. */
  url: string | null;
}

/** A persisted assistant Q&A interaction enriched with parsed citations. */
interface AssistantQa {
  /** Stable id for `@for` tracking (createdAt + index — interactions are append-only). */
  id: string;
  command: string;
  answer: string;
  citations: ParsedCitation[];
  status: string;
  sourceLabel: string | null;
  createdAt: string;
}

/** The whole note, decomposed into front-matter + body sections. */
interface ParsedNote {
  tags: string[];
  participants: string[];
  sections: NoteSection[];
  /** Set only when the body contained no `## ` sections — raw fallback. */
  raw: string | null;
  /** ENHANCE-MY-NOTES: true when the backend stamped `murmur_enhanced: true` (the note's
   *  skeleton was the user's typed notes). Derived ONLY from note.markdown — lock-safe. */
  enhanced: boolean;
}

@Component({
  selector: "app-detail",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    RouterLink,
    MeetingTimelineComponent,
    MeetingActionsComponent,
    MeetingChatComponent,
    MeetingRecipesComponent,
    LockBadgeComponent,
    MoveToMenuComponent,
    MarkdownComponent,
    AssistantSourcesComponent,
    RelatedMeetingsComponent,
    ShareVerifySheetComponent,
  ],
  templateUrl: "./detail.component.html",
  styleUrl: "./detail.component.scss",
})
export class DetailComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);
  private readonly injector = inject(Injector);
  private readonly destroyRef = inject(DestroyRef);
  private readonly folders = inject(FoldersService);
  private readonly toast = inject(ToastService);

  /** Exposed so the template can format aria values. */
  protected readonly Math = Math;

  readonly detail = signal<MeetingDetail | null>(null);
  readonly loading = signal(true);
  readonly busy = signal(false);
  readonly msg = signal("");

  // --- Phase 0.5 lock gate -------------------------------------------------
  /**
   * True while the backend has MASKED this meeting (it lives in a sealed,
   * not-session-unlocked folder). The template renders the lock gate instead
   * of the note/transcript/audio/timeline/actions. Mirrors `detail()?.locked`.
   */
  readonly locked = computed(() => this.detail()?.locked === true);
  /** True while an `unlockMeeting` biometric call is in flight (pending state). */
  readonly unlocking = signal(false);
  /** Focusable unlock button — focused after the gate renders (afterNextRender). */
  private readonly unlockButton =
    viewChild<ElementRef<HTMLButtonElement>>("unlockButton");

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
  /** True while a deleteMeeting IPC call is in flight (irreversible). */
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

  // --- Share as link (M3-CLIENT: zero-knowledge note link share) -----------
  /** True while a `shareNoteToLink` IPC call is in flight (disables the button). */
  readonly sharing = signal(false);
  /** The created share URL; the `#…` fragment holds the decryption key (kept local). */
  readonly shareUrl = signal<string | null>(null);
  /** Optional password for the NEXT link share — recipients must enter it in the viewer to decrypt.
   * Transient: mixed into the link key (Argon2id) at share time and cleared right after. */
  readonly sharePassword = signal("");
  /** True when the last-created link was password-protected (drives the "share the password
   * separately" hint). */
  readonly sharedWithPassword = signal(false);
  /** Inline error surfaced when sharing fails or is refused (not logged in, etc.). */
  readonly shareError = signal<string | null>(null);
  /**
   * True after the first-share check finds share-egress consent NOT yet granted:
   * the template shows an inline one-time consent panel before the upload runs.
   * The meeting id is stashed so Confirm can proceed without re-plumbing it.
   */
  readonly needsShareConsent = signal(false);
  private pendingShareMeetingId: string | null = null;
  /** Brief "Copied" confirmation for the share-link copy button. */
  readonly shareLinkCopied = signal(false);

  // --- Share with a person (M5-CLIENT: Murmur↔Murmur, mode B) ---------------
  /** The meeting being shared (captured when the person flow opens). */
  private personShareMeetingId: string | null = null;
  /** Whether the in-flow person-share panel (email/suggest-link/consent/result) is open. */
  readonly personShareOpen = signal(false);
  /** The panel's current step. `verifyMode` drives the separate floating sheet. */
  readonly personStep = signal<PersonShareStep>("email");
  /** The recipient email being entered/shared (input event → signal). */
  readonly personEmail = signal("");
  /** The last recipient preview (carries the fingerprint for the verify sheet). */
  readonly personPreview = signal<RecipientPreview | null>(null);
  /** True while a preview / share IPC call for the person flow is in flight. */
  readonly personBusy = signal(false);
  /** Inline error for the person flow (also fed to the verify sheet). */
  readonly personError = signal<string | null>(null);
  /** The success line shown at the 'result' step ("Sent" / "Invited — …"). */
  readonly personResult = signal<string | null>(null);
  /**
   * When non-null, the floating OPAQUE fingerprint verification SHEET is open in
   * this mode (first contact vs a changed key). Null = closed.
   */
  readonly verifyMode = signal<ShareVerifyMode | null>(null);

  /**
   * Whether this install keeps high-fidelity per-stream master archives (the
   * "Keep high-fidelity masters" setting). Loaded best-effort in ngOnInit; gates
   * the master-export actions, since a meeting only has masters when it was
   * recorded with this on. Install-global (not per-meeting), so the backend
   * stays the source of truth — it rejects a stream with no master (InvalidArg)
   * or a sealed folder (Locked), both surfaced as friendly inline messages.
   */
  readonly keepsMasters = signal(false);

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

  // --- Audio player state (driven by the <audio> event bindings) ----------
  private readonly audio = viewChild<ElementRef<HTMLAudioElement>>("player");
  readonly currentTime = signal(0);
  readonly duration = signal(0);
  readonly playing = signal(false);
  readonly copied = signal(false);

  /** Asset-protocol URL for the recording, or null when there is no audio. */
  readonly audioSrc = computed(() => {
    const path = this.detail()?.meeting.audioPath;
    return path ? convertFileSrc(path) : null;
  });

  /** Progress as a 0–100 percentage for the seek-bar fill. */
  readonly progressPct = computed(() => {
    const dur = this.duration();
    if (dur <= 0) {
      return 0;
    }
    return Math.min(100, (this.currentTime() / dur) * 100);
  });

  /** The note's markdown decomposed into front-matter + body sections. */
  readonly note = computed<ParsedNote | null>(() => {
    const md = this.detail()?.note?.markdown;
    return md ? this.parseNote(md) : null;
  });

  /**
   * The persisted in-meeting assistant Q&A for this meeting, citations parsed
   * into vault/web shapes for rendering. Empty when the meeting is locked (the
   * backend gates `assistantInteractions` exactly like `note`/`segments`).
   */
  readonly interactions = computed<AssistantQa[]>(() => {
    const raw = this.detail()?.assistantInteractions ?? [];
    return raw.map((i, idx) => this.parseInteraction(i, idx));
  });

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

  // --- Interactive timeline (speaker + topic viz) -------------------------
  readonly timeline = signal<MeetingTimeline | null>(null);
  readonly timelineLoading = signal(false);
  readonly timelineError = signal(false);
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
    for (const seg of this.detail()?.segments ?? []) {
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
   * Navigate to a semantically-related meeting and reload the view in place.
   * The `/meeting/:id` route reuses THIS component (the default
   * RouteReuseStrategy keeps it when only the param changes), so a same-route
   * navigation does NOT re-run `ngOnInit` — we reload explicitly. The related
   * section then re-fetches via its `meetingId` input.
   */
  async openRelated(id: string): Promise<void> {
    if (!id || this.detail()?.meeting.id === id) {
      return;
    }
    await this.router.navigate(["/meeting", id]);
    await this.loadMeeting(id);
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
    this.speakerSuggestions.set([]);
    this.tags.set([]);
    this.graph.set(null);
    this.graphError.set("");
    this.editing.set(false);
    this.renaming.set(false);
    this.moveOpen.set(false);
    this.confirmingDelete.set(false);
    // Reset audio-playback signals so an in-place meeting→meeting nav never shows the
    // previous meeting's position/play-state until a media event self-corrects.
    this.playing.set(false);
    this.currentTime.set(0);
    this.duration.set(0);
    try {
      this.detail.set(await this.ipc.getMeetingDetail(id));
    } finally {
      this.loading.set(false);
    }
    // Whether this install keeps hi-res masters — gates the master-export
    // actions. Install-global, so load it regardless of lock state (best-effort;
    // a failure simply hides the actions). The backend remains the real gate.
    try {
      this.keepsMasters.set((await this.ipc.getConfig()).keepHiresMasters);
    } catch {
      this.keepsMasters.set(false);
    }
    // Locked (masked) meetings render the lock gate only — skip priming the
    // timeline/tags (they're empty/masked) and focus the Unlock button instead.
    if (this.locked()) {
      afterNextRender(() => this.unlockButton()?.nativeElement.focus(), {
        injector: this.injector,
      });
      return;
    }
    // Kick the timeline off after the detail load; never blocks the page and
    // tolerates the first-call LLM latency (backend caches the result).
    if (this.detail()) {
      void this.loadTimeline();
      // Prime the folder tree so the read-only folder/lock badge + the move
      // picker have state on a direct navigation (idempotent; the root component
      // also loads it). Non-blocking — a failure just hides the badge.
      void this.folders.load();
      // Load the meeting's tags (best-effort; failure leaves the chips empty).
      try {
        this.tags.set(await this.ipc.getMeetingTags(id));
      } catch {
        this.tags.set([]);
      }
    }
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
      // Re-fetch the now-unmasked detail and swap it in place. A null detail
      // (deleted out from under us) keeps the not-found state honest.
      const fresh = await this.ipc.getMeetingDetail(id);
      this.detail.set(fresh);
      if (fresh && !fresh.locked) {
        // Refresh the folder tree so the header lock badge reflects the unlock,
        // then prime the timeline + tags the masked load skipped. Non-blocking.
        void this.folders.load();
        void this.loadTimeline();
        try {
          this.tags.set(await this.ipc.getMeetingTags(id));
        } catch {
          this.tags.set([]);
        }
      }
    } catch {
      // Biometric denied / cancelled, or the unlock errored — stay gated.
      this.toast.danger(
        "Couldn’t unlock — authentication failed or cancelled.",
      );
    } finally {
      this.unlocking.set(false);
    }
  }

  /** Fetch (or re-fetch, via Retry) the AI-derived speaker + topic timeline. */
  async loadTimeline(): Promise<void> {
    const id = this.detail()?.meeting.id;
    if (!id) {
      return;
    }
    this.timelineError.set(false);
    this.timelineLoading.set(true);
    try {
      this.timeline.set(await this.ipc.getTimeline(id));
    } catch {
      this.timeline.set(null);
      this.timelineError.set(true);
    } finally {
      this.timelineLoading.set(false);
    }
    // Voiceprint speaker suggestions (opt-in) — best-effort, never blocks the
    // timeline. Empty when the feature is off / meeting locked / nothing matched.
    void this.loadSpeakerSuggestions();
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
      this.pinError.set("Couldn’t pin: " + String(e));
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
      this.graphError.set("Couldn’t connect to graph: " + String(e));
    } finally {
      this.linking.set(false);
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
      this.msg.set("Error: " + String(e));
    } finally {
      this.busy.set(false);
    }
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
      this.renaming.set(false);
    } catch (e) {
      this.msg.set("Couldn’t rename: " + String(e));
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
      this.deleteError.set("Couldn’t delete: " + String(e));
      this.deleting.set(false);
    }
  }

  // --- In-app note editor --------------------------------------------------

  /** Enter edit mode, seeding the draft with the note's current raw markdown. */
  startEdit(): void {
    this.draft.set(this.detail()?.note?.markdown ?? "");
    this.saveError.set("");
    this.editing.set(true);
  }

  /** Two-way bind: mirror the textarea value into the `draft` signal. */
  onDraftInput(event: Event): void {
    this.draft.set((event.target as HTMLTextAreaElement).value);
  }

  /** Discard the working copy and leave edit mode unchanged. */
  cancelEdit(): void {
    this.editing.set(false);
    this.saveError.set("");
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
    if (!meetingId) {
      return;
    }
    this.saving.set(true);
    this.saveError.set("");
    try {
      const updated = await this.ipc.updateNote(meetingId, this.draft());
      const current = this.detail();
      if (current) {
        this.detail.set({ ...current, note: updated });
      }
      this.editing.set(false);
      this.flashSaved();
    } catch (e) {
      this.saveError.set("Couldn’t save: " + String(e));
    } finally {
      this.saving.set(false);
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
      this.tagsError.set("Couldn’t save tags: " + String(e));
    } finally {
      this.tagsBusy.set(false);
    }
  }

  // --- Audio player controls ----------------------------------------------

  private get el(): HTMLAudioElement | null {
    return this.audio()?.nativeElement ?? null;
  }

  togglePlay(): void {
    const el = this.el;
    if (!el) {
      return;
    }
    if (el.paused) {
      void el.play();
    } else {
      el.pause();
    }
  }

  onLoaded(): void {
    const el = this.el;
    if (el && Number.isFinite(el.duration)) {
      this.duration.set(el.duration);
    }
  }

  onTimeUpdate(): void {
    const el = this.el;
    if (el) {
      this.currentTime.set(el.currentTime);
    }
  }

  onEnded(): void {
    this.playing.set(false);
    this.currentTime.set(this.duration());
  }

  /** Seek to a click position on the progress track. */
  seekFromEvent(event: MouseEvent): void {
    const el = this.el;
    const dur = this.duration();
    if (!el || dur <= 0) {
      return;
    }
    const bar = event.currentTarget as HTMLElement;
    const rect = bar.getBoundingClientRect();
    const ratio = Math.min(
      1,
      Math.max(0, (event.clientX - rect.left) / rect.width),
    );
    el.currentTime = ratio * dur;
    this.currentTime.set(el.currentTime);
  }

  /** Keyboard seeking on the focusable track (← / → by 5s, Home/End). */
  onTrackKey(event: KeyboardEvent): void {
    const el = this.el;
    const dur = this.duration();
    if (!el || dur <= 0) {
      return;
    }
    let next: number | null = null;
    switch (event.key) {
      case "ArrowLeft":
        next = Math.max(0, el.currentTime - 5);
        break;
      case "ArrowRight":
        next = Math.min(dur, el.currentTime + 5);
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = dur;
        break;
      case " ":
      case "Enter":
        event.preventDefault();
        this.togglePlay();
        return;
      default:
        return;
    }
    event.preventDefault();
    el.currentTime = next;
    this.currentTime.set(next);
  }

  /**
   * Click-to-seek from a transcript row or a timeline block: jump to `startS`
   * + play. With no audio element (audioPath null) we still advance the
   * `currentTime` signal so the timeline highlight + playhead respond.
   */
  seekTo(startS: number): void {
    const el = this.el;
    if (!el) {
      const total = this.timelineTotal();
      const clamped = total > 0 ? Math.min(total, Math.max(0, startS)) : startS;
      this.currentTime.set(clamped);
      return;
    }
    el.currentTime = startS;
    this.currentTime.set(startS);
    void el.play();
  }

  /** True when playback is inside [startS, endS) — highlights the live row. */
  isActiveSegment(startS: number, endS: number): boolean {
    const t = this.currentTime();
    return t >= startS && t < endS;
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
      this.exportError.set("Couldn’t copy: " + String(e));
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
      this.exportError.set("Couldn’t save markdown: " + String(e));
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
      this.exportError.set("Couldn’t save audio: " + String(e));
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
    const raw = String(error);
    if (/locked/i.test(raw)) {
      return "This meeting is locked — unlock it to export the master.";
    }
    if (/no master/i.test(raw)) {
      return stream === "mic"
        ? "No hi-res mic master was kept for this meeting."
        : "No hi-res system master was kept for this meeting.";
    }
    return "Couldn’t export the master: " + raw;
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
      this.canvasError.set("Couldn’t export Canvas: " + String(e));
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

  // --- Share as link (zero-knowledge note link share) ----------------------

  /**
   * Create a zero-knowledge link share of this note and copy the URL. Guards
   * first on the sharing-account session (server set? logged in + unlocked?),
   * then on the one-time share-egress consent: the FIRST share pauses on an
   * inline consent panel ({@link needsShareConsent}) until the user confirms.
   *
   * The returned URL is NEVER logged — its `#…` fragment is the decryption key
   * and only ever lands in the signal + clipboard.
   */
  async shareAsLink(meetingId: string): Promise<void> {
    if (this.editing() || this.sharing()) {
      return;
    }
    this.shareError.set(null);
    this.shareUrl.set(null);
    let st;
    try {
      st = await this.ipc.accountStatus();
    } catch (e) {
      this.shareError.set(String(e));
      return;
    }
    if (!st.serverConfigured) {
      this.shareError.set("Set a sharing server in Settings → Account first.");
      return;
    }
    if (!st.loggedIn || !st.unlockedForSharing) {
      this.shareError.set(
        "Sign in to your sharing account first (Settings → Account).",
      );
      return;
    }
    if (!st.shareConsented) {
      // First share — surface the inline one-time consent panel and stop here.
      this.pendingShareMeetingId = meetingId;
      this.needsShareConsent.set(true);
      return;
    }
    await this.doShare(meetingId);
  }

  /** Confirm the one-time share-egress consent, then proceed with the pending share. */
  async confirmShareConsent(): Promise<void> {
    const id = this.pendingShareMeetingId;
    this.needsShareConsent.set(false);
    this.pendingShareMeetingId = null;
    if (!id) {
      return;
    }
    this.shareError.set(null);
    try {
      await this.ipc.consentToShareEgress();
    } catch (e) {
      this.shareError.set(String(e));
      return;
    }
    await this.doShare(id);
  }

  /** Cancel the pending first-share (dismiss the consent panel, upload nothing). */
  cancelShareConsent(): void {
    this.needsShareConsent.set(false);
    this.pendingShareMeetingId = null;
  }

  /**
   * Perform the actual upload + copy. Consent/login are already verified by the
   * caller. The URL goes to the signal + clipboard only (never the console).
   */
  private async doShare(meetingId: string): Promise<void> {
    this.sharing.set(true);
    const pw = this.sharePassword().trim();
    try {
      const url = await this.ipc.shareNoteToLink(
        meetingId,
        undefined,
        pw.length > 0 ? pw : undefined,
      );
      this.shareUrl.set(url);
      this.sharedWithPassword.set(pw.length > 0);
      this.sharePassword.set(""); // clear the transient password once it's baked into the link
      try {
        await navigator.clipboard.writeText(url);
        this.shareLinkCopied.set(true);
      } catch {
        // Clipboard unavailable — the URL stays visible + selectable to copy.
      }
    } catch (e) {
      this.shareError.set(String(e));
    } finally {
      this.sharing.set(false);
    }
  }

  /** Copy the created share link to the clipboard (the readonly-field button). */
  async copyShareLink(): Promise<void> {
    const url = this.shareUrl();
    if (!url) {
      return;
    }
    try {
      await navigator.clipboard.writeText(url);
      this.shareLinkCopied.set(true);
    } catch {
      // Clipboard unavailable — the URL stays visible + selectable.
    }
  }

  // --- Share with a person (M5-CLIENT: invite a colleague by email) ---------

  /** Open the in-flow person-share panel at the email step (resets prior state). */
  openPersonShare(meetingId: string): void {
    if (this.editing()) {
      return;
    }
    this.personShareMeetingId = meetingId;
    this.personEmail.set("");
    this.personPreview.set(null);
    this.personError.set(null);
    this.personResult.set(null);
    this.personStep.set("email");
    this.verifyMode.set(null);
    this.personShareOpen.set(true);
  }

  /** Fully close the person flow (panel + any floating sheet) and clear its state. */
  closePersonShare(): void {
    this.personShareOpen.set(false);
    this.verifyMode.set(null);
    this.personStep.set("email");
    this.personEmail.set("");
    this.personPreview.set(null);
    this.personError.set(null);
    this.personResult.set(null);
  }

  /** Bind the recipient-email input into its signal. */
  onPersonEmailInput(event: Event): void {
    this.personEmail.set((event.target as HTMLInputElement).value);
  }

  /**
   * Preview the recipient, then branch: unregistered → suggest a protected
   * link; first contact / changed key → the floating verification sheet;
   * otherwise send straight away. Gates on the sharing account first (server
   * configured + signed in + unlocked), mirroring {@link shareAsLink}.
   */
  async submitPersonEmail(): Promise<void> {
    if (this.personBusy()) {
      return;
    }
    const email = this.personEmail().trim();
    if (!email) {
      this.personError.set("Enter an email address.");
      return;
    }
    this.personError.set(null);
    this.personBusy.set(true);
    try {
      const st = await this.ipc.accountStatus();
      if (!st.serverConfigured) {
        this.personError.set(
          "Set a sharing server in Settings → Account first.",
        );
        return;
      }
      if (!st.loggedIn || !st.unlockedForSharing) {
        this.personError.set(
          "Sign in to your sharing account first (Settings → Account).",
        );
        return;
      }
      const preview = await this.ipc.previewShareRecipient(email);
      this.personPreview.set(preview);
      if (!preview.registered) {
        this.personStep.set("suggest-link");
      } else if (preview.keyChanged) {
        // BLOCKING re-verify: never a silent send on a changed key.
        this.personShareOpen.set(false);
        this.verifyMode.set("key-changed");
      } else if (preview.firstContact) {
        this.personShareOpen.set(false);
        this.verifyMode.set("first-contact");
      } else {
        // Known, verified recipient — share directly.
        await this.sendToUser();
      }
    } catch (e) {
      this.personError.set(String(e));
    } finally {
      this.personBusy.set(false);
    }
  }

  /** The "suggest-link" primary: fall back to the existing protected-link flow. */
  sendProtectedLinkInstead(): void {
    const id = this.personShareMeetingId;
    this.closePersonShare();
    if (id) {
      void this.shareAsLink(id);
    }
  }

  /** The "suggest-link" secondary: invite the (unregistered) recipient anyway. */
  async inviteAnyway(): Promise<void> {
    if (this.personBusy()) {
      return;
    }
    this.personBusy.set(true);
    try {
      await this.sendToUser();
    } finally {
      this.personBusy.set(false);
    }
  }

  /** The verify-sheet confirm: the user verified out of band → send. */
  async confirmVerifiedSend(): Promise<void> {
    if (this.personBusy()) {
      return;
    }
    this.personBusy.set(true);
    try {
      await this.sendToUser();
    } finally {
      this.personBusy.set(false);
    }
  }

  /** Grant the one-time share-egress consent, then retry the pending person share. */
  async confirmPersonShareConsent(): Promise<void> {
    if (this.personBusy()) {
      return;
    }
    this.personBusy.set(true);
    this.personError.set(null);
    try {
      await this.ipc.consentToShareEgress();
      await this.sendToUser();
    } catch (e) {
      this.personError.set(String(e));
    } finally {
      this.personBusy.set(false);
    }
  }

  /**
   * Perform the actual `shareNoteToUser` call and render the outcome. Callers own
   * the `personBusy` toggle. A thrown "not consented" surfaces the in-flow
   * consent step (mirroring the link flow's one-time consent); any other throw
   * (a locked meeting, a server-side changed-key BLOCK) surfaces inline —
   * never a silent proceed.
   */
  private async sendToUser(): Promise<void> {
    const id = this.personShareMeetingId;
    if (!id) {
      return;
    }
    const email = this.personEmail().trim();
    this.personError.set(null);
    try {
      const res = await this.ipc.shareNoteToUser(id, email);
      this.verifyMode.set(null);
      this.personShareOpen.set(true);
      this.personStep.set("result");
      this.personResult.set(
        res.status === "invited"
          ? `Invited — they'll get it when they join Murmur. Ask them to install Murmur (macOS) and sign in with ${email}.`
          : "Sent.",
      );
    } catch (e) {
      const msg = String(e);
      if (/consent/i.test(msg)) {
        // First share needs the one-time egress consent — surface it in-flow.
        this.verifyMode.set(null);
        this.personShareOpen.set(true);
        this.personStep.set("consent");
        this.personError.set(null);
      } else {
        this.personError.set(msg);
      }
    }
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

  /** Map an interaction status to a global `.pill` variant (mirrors the live card). */
  protected qaStatusPillClass(status: string): string {
    switch (status) {
      case "ok":
        return "is-success";
      case "needs_consent":
        return "is-warning";
      case "unavailable":
      case "unrecognized":
        return "is-accent";
      case "nothing_heard":
        return "";
      default:
        return "is-danger";
    }
  }

  /** Short human label for the status pill. */
  protected qaStatusLabel(status: string): string {
    switch (status) {
      case "ok":
        return "Odpowiedziano";
      case "needs_consent":
        return "Wymaga zgody";
      case "unavailable":
        return "Niedostępne";
      case "unrecognized":
        return "Nierozpoznane";
      case "nothing_heard":
        return "Nic nie usłyszano";
      case "error":
        return "Błąd";
      default:
        return status;
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

  /**
   * Map a transcript segment's `speaker` to a small presentational chip:
   * "Me" (the local mic, accent) vs "Others" (captured system audio, neutral/
   * violet). Returns null for legacy / mic-only segments (`null` / unknown) so
   * they render unlabeled exactly as before. This is independent of the AI
   * timeline's manual speaker-rename — that feature relabels timeline lanes, not
   * these per-segment Me/Others tags.
   */
  speakerChip(
    speaker: Segment["speaker"],
  ): { label: string; bg: string; fg: string } | null {
    switch (speaker) {
      case "me":
        // Local mic — the calm accent.
        return {
          label: "Me",
          bg: "var(--accent-soft)",
          fg: "var(--accent-hover)",
        };
      case "others":
        // Captured system audio — a neutral violet, distinct from "Me".
        return {
          label: "Others",
          bg: "rgba(157, 123, 255, 0.16)",
          fg: "#b9a4ff",
        };
      default: {
        // A diarized remote cluster tag ("others-{n}") → a "Speaker {n+1}" chip (same neutral
        // violet as "Others"). Presentational only; independent of the timeline lanes' LLM/renamed
        // labels. Any other/legacy value stays unlabeled (null).
        const m = /^others-(\d+)$/.exec(speaker ?? "");
        if (m) {
          return {
            label: `Speaker ${Number(m[1]) + 1}`,
            bg: "rgba(157, 123, 255, 0.16)",
            fg: "#b9a4ff",
          };
        }
        return null;
      }
    }
  }

  /** Seconds → m:ss for timestamps + player times. */
  fmt(s: number): string {
    const total = Math.max(0, Math.floor(s || 0));
    const m = Math.floor(total / 60);
    const sec = total % 60;
    return `${m}:${sec.toString().padStart(2, "0")}`;
  }

  /** Maps a meeting status to a status-pill state modifier (presentation only). */
  statusPillClass(status: string): string {
    switch (status) {
      case "RECORDING":
      case "ERROR":
        return "is-danger";
      case "TRANSCRIBED":
      case "SUMMARIZED":
        return "is-accent";
      case "EXPORTED":
        return "is-success";
      default:
        return "";
    }
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
