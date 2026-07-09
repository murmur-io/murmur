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
import { ActivatedRoute, Router, RouterLink } from "@angular/router";
import { convertFileSrc } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { IpcService } from "../../../core/ipc.service";
import type {
  AppConfigDto,
  AssistantInteraction,
  FolderNode,
  GraphPayload,
  MeetingDetail,
  MeetingTimeline,
  SpeakerSuggestion,
} from "../../../core/models";
import {
  FoldersService,
  type FolderExposure,
} from "../../../services/folders.service";
import { ToastService } from "../../../services/toast.service";
import { LockBadgeComponent } from "../../folders/lock-badge/lock-badge.component";
import { AudioPanelComponent } from "../audio-panel/audio-panel.component";
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

/** One checklist entry parsed from a `- [ ]` / `- [x]` action-item line. */
interface ActionItem {
  done: boolean;
  text: string;
}

@Component({
  selector: "app-detail",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    RouterLink,
    LockBadgeComponent,
    DetailTabsComponent,
    NotePanelComponent,
    AudioPanelComponent,
    SharePanelComponent,
    VerifyPanelComponent,
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

  readonly detail = signal<MeetingDetail | null>(null);
  readonly loading = signal(true);
  readonly busy = signal(false);
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

  /**
   * PERF/OOM (P0.1): generate the timeline LAZILY — only when the Audio tab (the only surface that
   * renders it) is first opened for an unlocked meeting and it isn't already loaded / in flight.
   * `loadMeeting`/`unlock` no longer kick it off on open, so a plain Note-tab open never triggers the
   * multi-GB on-device LLM pass that OOM-killed the Mac. `allowSignalWrites` because `loadTimeline`
   * writes `timelineLoading` (which this effect reads) — the `!timelineLoading()` guard makes the
   * re-run a no-op, so there is no loop. See docs/research/2026-07-07-perf-memory-audit.md.
   */
  private readonly _timelineOnAudioTab = effect(
    () => {
      if (
        this.activeTab() === "audio" &&
        this.detail() &&
        !this.locked() &&
        !this.timeline() &&
        !this.timelineLoading() &&
        // Do NOT auto-retry after a failure: a persistent `get_timeline` error would otherwise
        // re-fire this effect every time `timelineLoading` flips back to false → an infinite retry
        // loop (and repeated multi-GB model loads). A failed load surfaces the Retry button, which
        // clears `timelineError` and re-calls `loadTimeline` explicitly.
        !this.timelineError()
      ) {
        void this.loadTimeline();
      }
    },
    { allowSignalWrites: true },
  );
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
   * The Share-panel precondition gate's CTA — route to Settings (the Account
   * section hosts the sharing server / sign-in / unlock controls). Fired via the
   * panel's `setupSharing` output.
   */
  async goToSharingSettings(): Promise<void> {
    await this.router.navigate(["/settings"]);
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
      // Biometric denied / cancelled, or the unlock errored — stay gated. Surface the REAL backend
      // error (AppError crosses the IPC as a string): a keychain OSStatus or a "content-key unwrap
      // failed" tells the user (and a field screenshot tells us) what actually broke — the old
      // generic apology made signed-build failures undiagnosable.
      this.toast.danger(`Couldn’t unlock — ${String(e)}`);
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
      this.toast.danger(`Couldn’t reset — ${String(e)}`);
    } finally {
      this.discarding.set(false);
    }
  }

  /** Fetch (or re-fetch, via Retry) the AI-derived speaker + topic timeline. */
  async loadTimeline(): Promise<void> {
    const id = this.detail()?.meeting.id;
    // In-flight guard: never start a second generation while one is running (the Audio-tab effect
    // could otherwise re-fire). P0.4.
    if (!id || this.timelineLoading()) {
      return;
    }
    this.timelineError.set(false);
    this.timelineLoading.set(true);
    try {
      const tl = await this.ipc.getTimeline(id);
      // STALE-RESULT guard: `get_timeline` can take many seconds (on-device LLM). If the user
      // switched meetings mid-flight, drop this result so we never paint meeting A's timeline over
      // meeting B (mirrors `resummarize`). P0.4.
      if (this.detail()?.meeting.id !== id) {
        return;
      }
      this.timeline.set(tl);
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
      this.tagsError.set("Couldn’t save tags: " + String(e));
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
