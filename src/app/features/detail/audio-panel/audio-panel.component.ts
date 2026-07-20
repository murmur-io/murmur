import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
  viewChild,
  viewChildren,
  afterNextRender,
} from "@angular/core";
import type {
  MeetingTimeline,
  Segment,
  SpeakerSuggestion,
} from "../../../core/models";
import { MeetingTimelineComponent } from "../meeting-timeline/meeting-timeline.component";

/** A speaker chip's presentational colours + label ("Me" / "Others" / …). */
interface SpeakerChip {
  label: string;
  bg: string;
  fg: string;
}

/**
 * One bounded display turn: consecutive same-speaker segments folded into a
 * paragraph, split before one monologue can create an unbounded fragment DOM.
 * `startS`/`endS` span the block; `segs` keeps the underlying segments for
 * karaoke and click-to-seek.
 */
interface Turn {
  key: string;
  speaker: Segment["speaker"];
  chip: SpeakerChip | null;
  startS: number;
  endS: number;
  text: string;
  segs: Segment[];
}

/**
 * The AUDIO tab: the recording surface's three synced views, adjacent at last —
 * a slim (sticky) player, the speaker/topic timeline, and a turn-grouped,
 * click-to-seek transcript that karaoke-highlights + auto-scrolls to the
 * playing turn. Owns playback state (the `<audio>` element + `currentTime`/
 * `duration`/`playing`/`rate`), so the shared playhead lives with the player it
 * drives; the shell hands it the meeting's segments/timeline and owns the IPC
 * (retry / pin / rename).
 *
 * Lives in its own file so its inline styles get their own per-component
 * `anyComponentStyle` budget — the reason the giant detail component is split.
 */
@Component({
  selector: "app-audio-panel",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MeetingTimelineComponent],
  templateUrl: "./audio-panel.component.html",
  styleUrl: "./audio-panel.component.scss",
})
export class AudioPanelComponent {
  /** Exposed so the template can format aria values. */
  protected readonly Math = Math;
  private readonly injector = inject(Injector);

  // --- Inputs from the shell (the meeting's audio-side data) --------------
  /** Asset-protocol URL for the recording, or null when there is no audio. */
  readonly audioSrc = input<string | null>(null);
  /** The click-to-seek transcript segments. */
  readonly segments = input<Segment[]>([]);
  /** The AI speaker + topic timeline (null while loading / on error). */
  readonly timeline = input<MeetingTimeline | null>(null);
  /** Total length for the shared timeline scale. */
  readonly timelineTotal = input(0);
  /** True while the timeline fetch is in flight. */
  readonly timelineLoading = input(false);
  /** True when the timeline fetch failed (shows Retry). */
  readonly timelineError = input(false);
  /**
   * True when there's no cached timeline and generation is on-device (heavy) — the panel shows a
   * "Generate timeline" affordance instead of auto-loading a multi-GB model (perf/OOM). Cloud
   * installs never see this (they auto-generate).
   */
  readonly timelineNeedsGeneration = input(false);
  /** Opt-in voiceprint speaker suggestions for the timeline legend. */
  readonly speakerSuggestions = input<SpeakerSuggestion[]>([]);
  /** Transient pin confirmation ("Pinned 2:14 — …"). */
  readonly pinMsg = input("");
  /** Inline pin error. */
  readonly pinError = input("");
  /**
   * A note-receipt seek request (Brain v3 PR-5 "Receipts"): the second of audio
   * a note claim derives from, plus the transcript `Segment.idx` to flash. The
   * shell sets this (and switches to the Audio tab) when the user clicks a receipt
   * chip; the panel is (re)created for the Audio `@switch` case, so this arrives as
   * an INPUT the panel applies on mount/change — a viewChild method call from the
   * Note tab would hit a not-yet-existing panel. `seq` is bumped by the shell so
   * clicking the SAME receipt twice re-fires the effect (a net-zero value wouldn't).
   * Null when there is no pending receipt seek. Carries only audio-coordinate ints —
   * no note/transcript text, no on-disk path.
   */
  readonly seekTarget = input<{ startS: number; segId: number; seq: number } | null>(
    null,
  );

  // --- Outputs back to the shell (which owns the IPC) ---------------------
  /** Retry the timeline fetch (from the timeline's (retry)). */
  readonly retryTimeline = output<void>();
  /** Explicit on-device timeline generation (from the timeline's (generate) click). */
  readonly generateTimeline = output<void>();
  /** Pin the current moment (seconds) — the shell writes the block ref + link. */
  readonly pin = output<number>();
  /** Rename a timeline speaker lane. */
  readonly renameSpeaker = output<{ oldLabel: string; newLabel: string }>();
  /**
   * The pending receipt seek was APPLIED (carries its `seq`): the shell clears
   * `seekTarget` on this ack, making consumption ONE-SHOT — without it the
   * still-set input replays the seek + flash every time the Audio tab is
   * revisited (this panel is recreated per `@switch` case, so the mount effect
   * re-fires on a stale target). Repeat clicks on the SAME chip still work: the
   * note panel bumps `seq` per click, so a fresh target always arrives.
   */
  readonly seekConsumed = output<number>();

  // --- Audio playback state (driven by the <audio> event bindings) --------
  private readonly audio = viewChild<ElementRef<HTMLAudioElement>>("player");
  readonly currentTime = signal(0);
  readonly duration = signal(0);
  readonly playing = signal(false);
  /**
   * The `Segment.idx` to briefly PULSE (Brain v3 PR-5 "Receipts"): when a note
   * receipt chip drives a seek, `seekTo(startS)` already lands the playhead inside
   * that segment so `activeSegKeys` gives the persistent karaoke highlight — this
   * adds a one-shot flash animation over it so the eye is drawn to the exact line
   * that proves the claim. A bump `flashSeq` re-arms the pure-CSS animation when the
   * SAME segment is receipted twice in a row (a net-zero id write wouldn't restart
   * it). Cleared to null on any user-driven seek so a stray flash never lingers.
   */
  readonly flashSegId = signal<number | null>(null);
  private readonly flashSeq = signal(0);
  /** Composite the flash targets so the template re-arms on a repeat receipt. */
  readonly flashKey = computed(
    () => `${this.flashSegId() ?? ""}#${this.flashSeq()}`,
  );
  /** Playback rate, cycled 1× → 1.25× → 1.5× → 2× → 1×. */
  readonly rate = signal(1);
  /** The rate as a trimmed label ("1", "1.25", "1.5", "2"). */
  readonly rateLabel = computed(() => String(this.rate()));

  // --- Transcript find + karaoke ------------------------------------------
  /** Live text filter for the transcript turns (case-insensitive substring). */
  readonly query = signal("");
  /**
   * Hard render budgets for the collapsed transcript. `RENDER_CAP` bounds rows; `FRAGMENT_CAP`
   * bounds the inner seek buttons even when a 1.5h single-speaker recording would otherwise fold
   * into one giant turn. `TURN_FRAGMENT_CAP` splits only the display paragraph, never the source
   * segments, so search/seek/karaoke keep their original coordinates and stable ids.
   */
  private readonly RENDER_CAP = 80;
  private readonly FRAGMENT_CAP = 160;
  private readonly TURN_FRAGMENT_CAP = 16;
  readonly transcriptExpanded = signal(false);
  /** The scrolling transcript container + one element per rendered turn. */
  private readonly scroller = viewChild<ElementRef<HTMLElement>>("scroller");
  private readonly turnRows =
    viewChildren<ElementRef<HTMLElement>>("turnRow");

  /**
   * Fold consecutive same-speaker segments into bounded display turns. The prior unbounded fold
   * had two long-meeting failure modes: a single-speaker recording still rendered every fragment
   * despite `RENDER_CAP`, and repeated `cur.text += ...` copied a growing monologue string. Chunking
   * plus one final `join` makes construction linear and gives the renderer a hard fragment budget.
   */
  readonly turns = computed<Turn[]>(() => {
    const segs = this.segments();
    const out: Turn[] = [];
    let cur: Turn | null = null;
    for (const s of segs) {
      const sp = s.speaker ?? null;
      if (
        cur &&
        (cur.speaker ?? null) === sp &&
        cur.segs.length < this.TURN_FRAGMENT_CAP
      ) {
        cur.segs.push(s);
        cur.endS = s.endS;
      } else {
        cur = {
          key: `t${s.idx}`,
          speaker: sp,
          chip: this.speakerChip(sp),
          startS: s.startS,
          endS: s.endS,
          text: "",
          segs: [s],
        };
        out.push(cur);
      }
    }
    for (const turn of out) {
      turn.text = turn.segs.map((s) => s.text).join(" ");
    }
    return out;
  });

  /** Turns filtered by the Find box (whole-turn text match). */
  readonly visibleTurns = computed<Turn[]>(() => {
    const q = this.query().trim().toLowerCase();
    if (!q) {
      return this.turns();
    }
    return this.turns().filter((t) => t.text.toLowerCase().includes(q));
  });

  /** The `key` of the turn containing the playhead — karaoke highlight target. */
  readonly activeTurnKey = computed<string | null>(() => {
    const t = this.currentTime();
    for (const turn of this.turns()) {
      if (t >= turn.startS && t < turn.endS) {
        return turn.key;
      }
    }
    return null;
  });

  /**
   * The turns actually rendered: a contiguous, hard-bounded window around the active turn. The old
   * prefix window used `slice(0, activeIdx + 1)`, so seeking near the end of a 1.5h recording grew
   * the nominal 80-turn cap back to the full transcript. This window grows out from the playhead
   * while BOTH row and fragment budgets allow; at the start/search fallback it naturally grows
   * forward. Expanding remains the explicit opt-in to render everything.
   */
  readonly renderedTurns = computed<Turn[]>(() => {
    const all = this.visibleTurns();
    if (this.transcriptExpanded()) {
      return all;
    }

    const fragmentCount = all.reduce((sum, turn) => sum + turn.segs.length, 0);
    if (
      all.length <= this.RENDER_CAP &&
      fragmentCount <= this.FRAGMENT_CAP
    ) {
      return all;
    }

    // A receipt names the exact source segment. Prefer it over the first
    // time-overlapping turn so heavily overlapping dual-stream segments cannot
    // leave the requested proof outside the bounded window.
    const flashSeg = this.flashSegId();
    const flashIdx =
      flashSeg === null
        ? -1
        : all.findIndex((turn) =>
            turn.segs.some((segment) => segment.idx === flashSeg),
          );
    const activeKey = this.activeTurnKey();
    const activeIdx =
      flashIdx >= 0
        ? flashIdx
        : activeKey
          ? all.findIndex((turn) => turn.key === activeKey)
          : -1;
    let start = activeIdx >= 0 ? activeIdx : 0;
    let end = Math.min(all.length, start + 1);
    let renderedFragments = all[start]?.segs.length ?? 0;

    while (end - start < this.RENDER_CAP) {
      let grew = false;
      if (start > 0) {
        const before = all[start - 1].segs.length;
        if (renderedFragments + before <= this.FRAGMENT_CAP) {
          start -= 1;
          renderedFragments += before;
          grew = true;
        }
      }
      if (end < all.length && end - start < this.RENDER_CAP) {
        const after = all[end].segs.length;
        if (renderedFragments + after <= this.FRAGMENT_CAP) {
          end += 1;
          renderedFragments += after;
          grew = true;
        }
      }
      if (!grew) {
        break;
      }
    }

    return all.slice(start, end);
  });

  /** How many turns sit behind the "Show all" affordance (0 when the whole transcript is rendered). */
  readonly hiddenTurnCount = computed(
    () => this.visibleTurns().length - this.renderedTurns().length,
  );

  /**
   * The set of segment `idx`es whose [startS, endS) currently contains the playhead — the karaoke
   * highlight targets. A single `computed`, scanned ONCE per `currentTime` tick over the BOUNDED
   * render window; the template then does an O(1) `.has(s.idx)` per fragment. Replaces the former
   * `isActiveSegment()` METHOD binding, which Angular re-ran O(n) per fragment on EVERY
   * change-detection pass (~4×/s during playback → an ~8k-eval/s storm for a 1h transcript). A Set
   * (not a single key) preserves highlighting every overlapping fragment that is actually rendered.
   */
  readonly activeSegKeys = computed<Set<number>>(() => {
    const t = this.currentTime();
    const out = new Set<number>();
    for (const turn of this.renderedTurns()) {
      for (const s of turn.segs) {
        if (t >= s.startS && t < s.endS) {
          out.add(s.idx);
        }
      }
    }
    return out;
  });

  /** Progress as a 0–100 percentage for the seek-bar fill. */
  readonly progressPct = computed(() => {
    const dur = this.duration();
    if (dur <= 0) {
      return 0;
    }
    return Math.min(100, (this.currentTime() / dur) * 100);
  });

  /**
   * Auto-scroll the active turn into view as the playhead advances. Runs a
   * one-shot `afterNextRender` per active-turn change (zoneless-safe; no raw
   * scrollIntoView timer) so the row is laid out before we scroll. Skips when
   * the user is typing in the Find box (a filtered view shouldn't yank).
   */
  private readonly _karaokeScroll = effect(() => {
    const key = this.activeTurnKey();
    if (!key || this.query().trim()) {
      return;
    }
    afterNextRender(
      () => {
        const row = this.turnRows().find(
          (r) => r.nativeElement.getAttribute("data-turn") === key,
        );
        const box = this.scroller()?.nativeElement;
        if (!row || !box) {
          return;
        }
        const rTop = row.nativeElement.offsetTop;
        const rBot = rTop + row.nativeElement.offsetHeight;
        // Only scroll when the active turn is outside the visible band — a calm
        // nudge, never a fight with the user's own scroll.
        if (rTop < box.scrollTop || rBot > box.scrollTop + box.clientHeight) {
          box.scrollTo({
            top: rTop - box.clientHeight / 2 + row.nativeElement.offsetHeight / 2,
            behavior: "smooth",
          });
        }
      },
      { injector: this.injector },
    );
  });

  /**
   * Apply a pending note-receipt seek (Brain v3 PR-5): when the shell sets
   * `seekTarget` (and switches to this tab), seek the player to the claim's
   * second of audio and PULSE the matching transcript segment so the eye lands
   * on the exact line that proves the claim. Tracks `seq` (bumped by the shell)
   * so re-clicking the SAME receipt re-arms; `flashSeq` re-arms the pure-CSS
   * animation when the SAME segment is receipted twice (a net-zero `flashSegId`
   * write wouldn't restart it). Consumption is ONE-SHOT: after applying, the
   * effect acks via `seekConsumed` so the shell nulls the input — a later
   * Audio-tab revisit (a fresh panel instance) must not replay the seek/flash.
   * Legitimate signal-writing effect (T1): it reacts to an input and drives the
   * player — no async fetch, no stale race.
   */
  private readonly _applyReceiptSeek = effect(() => {
    const target = this.seekTarget();
    if (!target) {
      return;
    }
    // `seq` in the dependency set makes a repeat of the same receipt re-fire.
    void target.seq;
    this.seekTo(target.startS); // (also clears any prior flash)
    this.flashSegId.set(target.segId);
    this.flashSeq.update((n) => n + 1);
    // Ack consumption (the shell nulls `seekTarget`; the flash/karaoke state
    // above is panel-local and survives — only the REPLAY trigger is retired).
    this.seekConsumed.emit(target.seq);
    // Restart the pure-CSS pulse deterministically (a repeat of the SAME segment
    // keeps the `.is-flash` class, so the animation wouldn't retrigger on its own)
    // and bring the flashed fragment into view. One-shot, zoneless-safe: no timer.
    const key = this.flashKey();
    afterNextRender(
      () => {
        const box = this.scroller()?.nativeElement;
        const frag = box?.querySelector<HTMLElement>(
          `.frag[data-flash="${CSS.escape(key)}"]`,
        );
        if (!frag) {
          return;
        }
        // Force the browser to drop the running animation, then re-apply it on
        // the next frame — the canonical restart with no `@angular/animations`.
        frag.style.animation = "none";
        void frag.offsetWidth; // reflow — commits the "none" before re-enabling
        frag.style.animation = "";
        frag.scrollIntoView({ block: "center", behavior: "smooth" });
      },
      { injector: this.injector },
    );
  });

  /**
   * Release the receipt-specific window anchor after its pulse completes. The
   * regular playhead can then resume moving the bounded window. Reduced-motion
   * mode has no animation event, intentionally retaining its static proof
   * highlight until the next user seek clears it.
   */
  onReceiptFlashEnd(segId: number): void {
    if (this.flashSegId() === segId) {
      this.clearReceiptFlash();
    }
  }

  private clearReceiptFlash(): void {
    this.flashSegId.set(null);
  }

  /**
   * Reduced-motion disables the receipt animation, so no `animationend` can
   * release its window anchor. Release it once playback leaves the exact source
   * segment; until then the static reduced-motion highlight remains visible.
   */
  private clearReceiptFlashOutside(timeS: number): void {
    const flashSeg = this.flashSegId();
    if (flashSeg === null) {
      return;
    }
    const target = this.segments().find((segment) => segment.idx === flashSeg);
    if (!target || timeS < target.startS || timeS >= target.endS) {
      this.clearReceiptFlash();
    }
  }

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

  /**
   * Pause playback — called from `DetailComponent.onTabBackgrounded` when
   * this meeting's TAB is backgrounded (the shell's `<router-outlet (detach)>`
   * fires on every tab switch, not just a real destroy — see
   * `AppShellComponent.onOutletDetach`). A still-playing closed/backgrounded
   * tab would otherwise keep narrating from off-screen: WKWebView's own
   * detach-pauses-media behavior is real but browser/version-dependent, so
   * this is an explicit safety net rather than relying on it.
   */
  pausePlayback(): void {
    this.el?.pause();
  }

  /**
   * Collapse the transcript back to the bounded row/fragment window — called from
   * `DetailComponent.onTabBackgrounded` when this tab is detached (perf-audit
   * fix 2): a backgrounded tab with "Show all N turns" expanded retains the
   * FULL turn DOM off-screen (measured ~21k nodes + ~4k listeners on a
   * 2000-segment meeting) — a few such tabs is WKWebView-jettison territory.
   * Only the unbounded DOM collapses; the data signals stay, and the user
   * re-expands with one click on return (same as browsers discarding heavy
   * background-tab content).
   */
  collapseTranscript(): void {
    this.transcriptExpanded.set(false);
  }

  /**
   * Hard-stop AND unload the recording — called by the detail shell's
   * lock-mask path (`DetailComponent.maskLocally`) the moment this meeting's
   * folder is sealed. `pause()` alone leaves a resumable element still holding
   * the (now-locked) asset URL; removing `src` + `load()` releases it so a
   * stale `<audio>` can never outlive the lock, even in a detached
   * (backgrounded) tab whose template won't re-render until reattach. The
   * declarative `[src]="audioSrc()"` binding converges to null on the next
   * refresh (the masked detail nulls `audioPath`); this covers the frozen-CD
   * window before that.
   */
  stopAndUnload(): void {
    this.clearReceiptFlash();
    const el = this.el;
    if (!el) {
      return;
    }
    el.pause();
    el.removeAttribute("src");
    el.load();
    this.playing.set(false);
    this.currentTime.set(0);
  }

  /** Skip forward/back by `delta` seconds, clamped to [0, duration]. */
  skip(delta: number): void {
    const el = this.el;
    const dur = this.duration();
    if (!el || dur <= 0) {
      return;
    }
    this.clearReceiptFlash();
    const next = Math.min(dur, Math.max(0, el.currentTime + delta));
    el.currentTime = next;
    this.currentTime.set(next);
  }

  /** Cycle the playback rate 1× → 1.25× → 1.5× → 2× → 1×. */
  cycleRate(): void {
    const steps = [1, 1.25, 1.5, 2];
    const i = steps.indexOf(this.rate());
    const next = steps[(i + 1) % steps.length];
    this.rate.set(next);
    const el = this.el;
    if (el) {
      el.playbackRate = next;
    }
  }

  onLoaded(): void {
    const el = this.el;
    if (el) {
      if (Number.isFinite(el.duration)) {
        this.duration.set(el.duration);
      }
      // Reassert the chosen rate after a (re)load resets it to 1.
      el.playbackRate = this.rate();
    }
  }

  onTimeUpdate(): void {
    const el = this.el;
    if (el) {
      this.clearReceiptFlashOutside(el.currentTime);
      this.currentTime.set(el.currentTime);
    }
  }

  onEnded(): void {
    this.clearReceiptFlash();
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
    this.clearReceiptFlash();
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
    let next: number;
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
    this.clearReceiptFlash();
    el.currentTime = next;
    this.currentTime.set(next);
  }

  /**
   * Click-to-seek from a transcript turn/fragment or a timeline block: jump to
   * `startS` + play. With no audio element (audioPath null) we still advance the
   * `currentTime` signal so the timeline highlight + playhead respond.
   */
  seekTo(startS: number): void {
    // Clear any lingering receipt pulse so a user-driven seek never leaves a
    // stray flash on an unrelated segment. The receipt path (`_applyReceiptSeek`)
    // re-arms the flash AFTER this call, so its own pulse survives.
    this.clearReceiptFlash();
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

  /** Find-box input handler → the `query` signal. */
  onQuery(event: Event): void {
    this.query.set((event.target as HTMLInputElement).value);
  }

  /** Clear the Find box. */
  clearQuery(): void {
    this.query.set("");
  }

  /** Reveal the full transcript (explicitly drops both render budgets). */
  showAllTurns(): void {
    this.transcriptExpanded.set(true);
  }

  /**
   * Map a transcript segment's `speaker` to a small presentational chip:
   * "Me" (the local mic, accent) vs "Others" (captured system audio). Returns
   * null for legacy / mic-only segments so they render unlabeled.
   */
  speakerChip(speaker: Segment["speaker"]): SpeakerChip | null {
    switch (speaker) {
      case "me":
        return {
          label: "Me",
          bg: "var(--accent-soft)",
          fg: "var(--accent-hover)",
        };
      case "others":
        return {
          label: "Others",
          bg: "rgba(157, 123, 255, 0.16)",
          fg: "#b9a4ff",
        };
      default: {
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
}
