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
 * One turn: a run of consecutive same-speaker segments folded into a single
 * block (the biggest perceived-quality gap vs Otter/Descript). `startS`/`endS`
 * span the whole run; `segs` keeps the underlying segments for karaoke.
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

  // --- Outputs back to the shell (which owns the IPC) ---------------------
  /** Retry the timeline fetch (from the timeline's (retry)). */
  readonly retryTimeline = output<void>();
  /** Explicit on-device timeline generation (from the timeline's (generate) click). */
  readonly generateTimeline = output<void>();
  /** Pin the current moment (seconds) — the shell writes the block ref + link. */
  readonly pin = output<number>();
  /** Rename a timeline speaker lane. */
  readonly renameSpeaker = output<{ oldLabel: string; newLabel: string }>();

  // --- Audio playback state (driven by the <audio> event bindings) --------
  private readonly audio = viewChild<ElementRef<HTMLAudioElement>>("player");
  readonly currentTime = signal(0);
  readonly duration = signal(0);
  readonly playing = signal(false);
  /** Playback rate, cycled 1× → 1.25× → 1.5× → 2× → 1×. */
  readonly rate = signal(1);
  /** The rate as a trimmed label ("1", "1.25", "1.5", "2"). */
  readonly rateLabel = computed(() => String(this.rate()));

  // --- Transcript find + karaoke ------------------------------------------
  /** Live text filter for the transcript turns (case-insensitive substring). */
  readonly query = signal("");
  /**
   * Cap on how many turns render at once. A 1h meeting folds to hundreds of turns / thousands of
   * `<button>` fragments, so materializing them all is tens of MB of DOM + layout. We render the
   * first `RENDER_CAP` (always extended to include the turn the playhead is inside, so karaoke
   * auto-scroll never targets an un-rendered row) until the user asks for the whole transcript.
   */
  private readonly RENDER_CAP = 80;
  readonly transcriptExpanded = signal(false);
  /** The scrolling transcript container + one element per rendered turn. */
  private readonly scroller = viewChild<ElementRef<HTMLElement>>("scroller");
  private readonly turnRows =
    viewChildren<ElementRef<HTMLElement>>("turnRow");

  /**
   * Fold consecutive same-speaker segments into turn blocks. A single derived
   * `computed` over the input segments — the transcript's structural source.
   */
  readonly turns = computed<Turn[]>(() => {
    const segs = this.segments();
    const out: Turn[] = [];
    let cur: Turn | null = null;
    for (const s of segs) {
      const sp = s.speaker ?? null;
      if (cur && (cur.speaker ?? null) === sp) {
        cur.segs.push(s);
        cur.endS = s.endS;
        cur.text += (cur.text ? " " : "") + s.text;
      } else {
        cur = {
          key: `t${s.idx}`,
          speaker: sp,
          chip: this.speakerChip(sp),
          startS: s.startS,
          endS: s.endS,
          text: s.text,
          segs: [s],
        };
        out.push(cur);
      }
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
   * The turns actually rendered — the first `RENDER_CAP`, always extended to include the turn the
   * playhead is inside (so karaoke auto-scroll never targets an un-rendered row), or ALL turns once
   * the user expands or when the (possibly filtered) list is already within the cap. This bounds the
   * DOM node count for a long meeting without breaking the play/seek/highlight surfaces.
   */
  readonly renderedTurns = computed<Turn[]>(() => {
    const all = this.visibleTurns();
    if (this.transcriptExpanded() || all.length <= this.RENDER_CAP) {
      return all;
    }
    let cap = this.RENDER_CAP;
    const activeKey = this.activeTurnKey();
    if (activeKey) {
      const activeIdx = all.findIndex((t) => t.key === activeKey);
      if (activeIdx >= 0) {
        cap = Math.max(cap, activeIdx + 1);
      }
    }
    return all.slice(0, cap);
  });

  /** How many turns sit behind the "Show all" affordance (0 when the whole transcript is rendered). */
  readonly hiddenTurnCount = computed(
    () => this.visibleTurns().length - this.renderedTurns().length,
  );

  /**
   * The set of segment `idx`es whose [startS, endS) currently contains the playhead — the karaoke
   * highlight targets. A single `computed`, scanned ONCE per `currentTime` tick (O(n)); the template
   * then does an O(1) `.has(s.idx)` per fragment. Replaces the former `isActiveSegment()` METHOD
   * binding, which Angular re-ran O(n) per fragment on EVERY change-detection pass (~4×/s during
   * playback → an ~8k-eval/s storm for a 1h transcript). A Set (not a single key) preserves the
   * original behavior of highlighting EVERY active fragment when me/others segments overlap in
   * wall-clock time.
   */
  readonly activeSegKeys = computed<Set<number>>(() => {
    const t = this.currentTime();
    const out = new Set<number>();
    for (const s of this.segments()) {
      if (t >= s.startS && t < s.endS) {
        out.add(s.idx);
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
   * Collapse the transcript back to the windowed `RENDER_CAP` — called from
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
    el.currentTime = next;
    this.currentTime.set(next);
  }

  /**
   * Click-to-seek from a transcript turn/fragment or a timeline block: jump to
   * `startS` + play. With no audio element (audioPath null) we still advance the
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

  /** Find-box input handler → the `query` signal. */
  onQuery(event: Event): void {
    this.query.set((event.target as HTMLInputElement).value);
  }

  /** Clear the Find box. */
  clearQuery(): void {
    this.query.set("");
  }

  /** Reveal the full transcript (drops the `RENDER_CAP` window). */
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
