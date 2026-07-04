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
} from "../../core/models";
import { MeetingTimelineComponent } from "./meeting-timeline.component";

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
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MeetingTimelineComponent],
  template: `
    <div class="audio-panel">
      <!-- 1) SLIM AUDIO PLAYER — sticky at the top of the tab while you scroll
              the transcript. Progress bar (not a waveform: no samples kept). -->
      @if (audioSrc(); as src) {
        <div class="player panel-card">
          <audio
            #player
            [src]="src"
            preload="metadata"
            (loadedmetadata)="onLoaded()"
            (timeupdate)="onTimeUpdate()"
            (play)="playing.set(true)"
            (pause)="playing.set(false)"
            (ended)="onEnded()"
          ></audio>

          <div class="player-main">
            <button
              type="button"
              class="play"
              (click)="togglePlay()"
              [attr.aria-label]="playing() ? 'Pause' : 'Play'"
              [class.is-playing]="playing()"
            >
              @if (playing()) {
                <span class="icon-pause" aria-hidden="true"></span>
              } @else {
                <span class="icon-play" aria-hidden="true"></span>
              }
            </button>

            <div class="player-body">
              <div
                class="track"
                role="slider"
                tabindex="0"
                aria-label="Seek"
                [attr.aria-valuemin]="0"
                [attr.aria-valuemax]="Math.round(duration())"
                [attr.aria-valuenow]="Math.round(currentTime())"
                (click)="seekFromEvent($event)"
                (keydown)="onTrackKey($event)"
              >
                <div class="track-fill" [style.width.%]="progressPct()">
                  <span class="track-knob"></span>
                </div>
              </div>
              <div class="times">
                <span class="time">{{ fmt(currentTime()) }}</span>
                <span class="time time-total">{{ fmt(duration()) }}</span>
              </div>
            </div>
          </div>

          <!-- Transport row: ±15s skip + a playback-rate cycle. -->
          <div class="transport">
            <button
              type="button"
              class="chip-btn"
              aria-label="Back 15 seconds"
              (click)="skip(-15)"
            >
              <span aria-hidden="true">⏪</span> 15s
            </button>
            <button
              type="button"
              class="chip-btn"
              aria-label="Forward 15 seconds"
              (click)="skip(15)"
            >
              15s <span aria-hidden="true">⏩</span>
            </button>
            <button
              type="button"
              class="chip-btn rate"
              aria-label="Playback speed"
              (click)="cycleRate()"
            >
              {{ rateLabel() }}×
            </button>
          </div>
        </div>
      } @else {
        <div class="empty-state panel-card">
          <span class="empty-mark" aria-hidden="true"></span>
          <p class="empty-title">This meeting has no recording</p>
          <p class="empty">
            The player, timeline and transcript appear here when audio is
            available.
          </p>
        </div>
      }

      <!-- 2) INTERACTIVE TIMELINE (speakers + topics, shared playhead) ------ -->
      <section class="block">
        <h3 class="section-label">Speakers</h3>
        <app-meeting-timeline
          [timeline]="timeline()"
          [total]="timelineTotal()"
          [currentTime]="currentTime()"
          [loading]="timelineLoading()"
          [error]="timelineError()"
          [hasAudio]="!!audioSrc()"
          [suggestions]="speakerSuggestions()"
          (seek)="seekTo($event)"
          (retry)="retryTimeline.emit()"
          (pin)="pin.emit($event)"
          (renameSpeaker)="renameSpeaker.emit($event)"
        />

        <!-- Pin confirmation / error (driven by the timeline's (pin) output). -->
        @if (pinMsg(); as m) {
          <div class="saved-toast pin-toast" role="status">
            <span class="pin-toast-dot" aria-hidden="true"></span>
            {{ m }}
          </div>
        }
        @if (pinError(); as err) {
          <div class="saved-toast pin-toast pin-toast--error" role="alert">
            {{ err }}
          </div>
        }
      </section>

      <!-- 3) TURN-GROUPED, CLICK-TO-SEEK TRANSCRIPT ------------------------- -->
      <section class="block">
        <div class="block-head">
          <h3 class="section-label">Transcript</h3>
          @if (turns().length) {
            <span class="count">{{ turns().length }}</span>
          }
          <div class="spacer"></div>
          @if (segments().length) {
            <label class="find">
              <span class="find-icon" aria-hidden="true">🔍</span>
              <input
                type="text"
                class="find-input"
                placeholder="Find in transcript"
                aria-label="Find in transcript"
                autocapitalize="off"
                autocomplete="off"
                spellcheck="false"
                [value]="query()"
                (input)="onQuery($event)"
              />
              @if (query()) {
                <button
                  type="button"
                  class="find-clear"
                  aria-label="Clear search"
                  (click)="clearQuery()"
                >
                  ×
                </button>
              }
            </label>
          }
        </div>

        @if (visibleTurns().length) {
          <div #scroller class="transcript-card panel-card">
            <ul class="turns">
              @for (t of visibleTurns(); track t.key) {
                <li
                  #turnRow
                  class="turn"
                  [attr.data-turn]="t.key"
                  [class.is-active]="t.key === activeTurnKey()"
                >
                  <div class="turn-head">
                    @if (t.chip; as chip) {
                      <span
                        class="turn-speaker"
                        [style.background]="chip.bg"
                        [style.color]="chip.fg"
                        >{{ chip.label }}</span
                      >
                    }
                    <button
                      type="button"
                      class="turn-time"
                      [disabled]="!audioSrc()"
                      (click)="seekTo(t.startS)"
                    >
                      {{ fmt(t.startS) }}
                    </button>
                  </div>
                  <p class="turn-text">
                    @for (s of t.segs; track s.idx) {
                      <button
                        type="button"
                        class="frag"
                        [class.is-active]="isActiveSegment(s.startS, s.endS)"
                        [disabled]="!audioSrc()"
                        (click)="seekTo(s.startS)"
                      >{{ s.text }} </button>
                    }
                  </p>
                </li>
              } @empty {
                <li class="turn-empty">
                  <p class="empty">No turns match “{{ query() }}”.</p>
                </li>
              }
            </ul>
          </div>
        } @else if (segments().length) {
          <div class="empty-card panel-card">
            <p class="empty">No turns match “{{ query() }}”.</p>
          </div>
        } @else {
          <div class="empty-card panel-card">
            <p class="empty">No transcript.</p>
          </div>
        }
      </section>
    </div>
  `,
  styles: [
    `
      :host {
        display: block;
      }
      .audio-panel {
        display: flex;
        flex-direction: column;
        gap: var(--space-6);
        animation: rise 320ms var(--transition) both;
      }

      /* 1) Slim, sticky player -------------------------------------------- */
      .player {
        position: sticky;
        top: var(--space-3);
        z-index: 3;
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        padding: var(--space-4) var(--space-5);
      }
      .player-main {
        display: flex;
        align-items: center;
        gap: var(--space-4);
      }
      .play {
        flex: none;
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 44px;
        height: 44px;
        border: none;
        border-radius: var(--radius-pill);
        background: var(--accent-gradient);
        color: var(--text-on-accent);
        cursor: pointer;
        box-shadow: var(--glass-highlight);
        transition:
          transform var(--transition-fast),
          filter var(--transition);
      }
      .play:hover {
        filter: brightness(1.08);
        transform: translateY(-1px);
      }
      .play:active {
        transform: translateY(0) scale(0.96);
      }
      .play:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .play.is-playing {
        box-shadow: 0 0 0 1px var(--accent-ring);
      }
      .icon-play {
        width: 0;
        height: 0;
        margin-left: 2px;
        border-style: solid;
        border-width: 7px 0 7px 12px;
        border-color: transparent transparent transparent currentColor;
      }
      .icon-pause {
        width: 11px;
        height: 13px;
        border-left: 4px solid currentColor;
        border-right: 4px solid currentColor;
        box-sizing: content-box;
      }
      .player-body {
        flex: 1 1 auto;
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        min-width: 0;
      }
      .track {
        position: relative;
        height: 6px;
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
        cursor: pointer;
        transition: height var(--transition);
      }
      .track:hover,
      .track:focus-visible {
        height: 9px;
        outline: none;
      }
      .track:focus-visible {
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .track-fill {
        position: absolute;
        inset: 0 auto 0 0;
        height: 100%;
        min-width: 2px;
        border-radius: var(--radius-pill);
        background: var(--accent-gradient);
      }
      .track-knob {
        position: absolute;
        right: 0;
        top: 50%;
        width: 12px;
        height: 12px;
        transform: translate(50%, -50%);
        border-radius: 50%;
        background: var(--text-on-accent);
        box-shadow: var(--shadow-sm);
        opacity: 0;
        transition: opacity var(--transition);
      }
      .track:hover .track-knob,
      .track:focus-visible .track-knob {
        opacity: 1;
      }
      .times {
        display: flex;
        justify-content: space-between;
        gap: var(--space-3);
      }
      .time {
        color: var(--text-secondary);
        font-family: var(--font-mono);
        font-size: 0.8125rem;
        font-variant-numeric: tabular-nums;
        letter-spacing: -0.01em;
      }
      .time-total {
        color: var(--text-muted);
      }

      /* Transport chips (±15s + rate) */
      .transport {
        display: flex;
        gap: var(--space-2);
        padding-left: calc(44px + var(--space-4));
      }
      .chip-btn {
        display: inline-flex;
        align-items: center;
        gap: 4px;
        padding: 4px var(--space-3);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        color: var(--text-secondary);
        font-family: inherit;
        font-size: 0.75rem;
        font-weight: 600;
        cursor: pointer;
        transition:
          background var(--transition),
          color var(--transition),
          border-color var(--transition);
      }
      .chip-btn:hover {
        background: var(--surface-hover);
        color: var(--text-primary);
        border-color: var(--border-strong);
      }
      .chip-btn:focus-visible {
        outline: none;
        box-shadow: 0 0 0 2px var(--accent-ring);
      }
      .chip-btn.rate {
        margin-left: auto;
        font-family: var(--font-mono);
        font-variant-numeric: tabular-nums;
      }

      /* Pin toast (accent variant of the shared .saved-toast box). */
      .saved-toast {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        align-self: flex-start;
        margin-top: var(--space-3);
        padding: var(--space-1) var(--space-3);
        min-height: 28px;
        border-radius: var(--radius-pill);
        background: var(--accent-soft);
        color: var(--accent-hover);
        font-size: 0.8125rem;
        font-weight: 600;
        animation: rise 280ms var(--transition) both;
      }
      .pin-toast--error {
        background: var(--danger-soft);
        color: var(--danger);
      }
      .pin-toast-dot {
        flex: none;
        width: 8px;
        height: 8px;
        border-radius: 50%;
        background: var(--accent);
        box-shadow: 0 0 0 4px var(--accent-soft);
      }

      /* Section blocks --------------------------------------------------- */
      .block {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .block-head {
        display: flex;
        align-items: center;
        gap: var(--space-3);
      }
      .spacer {
        flex: 1 1 auto;
      }
      .find {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        padding: 5px var(--space-3);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-pill);
        background: var(--surface-input);
      }
      .find:focus-within {
        border-color: var(--border-strong);
        box-shadow: 0 0 0 2px var(--accent-ring);
      }
      .find-icon {
        font-size: 0.75rem;
        opacity: 0.7;
      }
      .find-input {
        width: 160px;
        border: none;
        background: transparent;
        color: var(--text-primary);
        font-family: inherit;
        font-size: 0.8125rem;
      }
      .find-input:focus {
        outline: none;
      }
      .find-clear {
        border: none;
        background: transparent;
        color: var(--text-muted);
        font-size: 1rem;
        line-height: 1;
        cursor: pointer;
        padding: 0 2px;
      }
      .find-clear:hover {
        color: var(--text-primary);
      }

      /* Turn-grouped transcript ------------------------------------------ */
      .transcript-card {
        padding: var(--space-2);
        max-height: 520px;
        overflow: auto;
      }
      .turns {
        list-style: none;
        padding: 0;
        margin: 0;
      }
      .turn {
        padding: var(--space-3);
        border-radius: var(--radius-md);
        scroll-margin: var(--space-4);
        transition: background var(--transition);
      }
      .turn + .turn {
        margin-top: 2px;
      }
      .turn.is-active {
        background: var(--accent-soft);
      }
      .turn-head {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        margin-bottom: 4px;
      }
      .turn-speaker {
        flex: none;
        padding: 2px var(--space-2);
        border-radius: var(--radius-pill);
        font-size: 0.6875rem;
        font-weight: 700;
        line-height: 1.5;
      }
      .turn-time {
        border: none;
        background: transparent;
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-size: 0.8125rem;
        font-variant-numeric: tabular-nums;
        cursor: pointer;
        padding: 0;
      }
      .turn-time:hover:not(:disabled) {
        color: var(--accent-hover);
      }
      .turn-time:disabled {
        cursor: default;
      }
      .turn.is-active .turn-time {
        color: var(--accent-hover);
      }
      .turn-text {
        margin: 0;
        line-height: 1.65;
        color: var(--text-secondary);
      }
      .turn.is-active .turn-text {
        color: var(--text-primary);
      }
      /* Each segment is a click-to-seek fragment; the playing one karaokes.
         Rendered inline inside the turn's <p> so words wrap naturally. */
      .frag {
        display: inline;
        border: none;
        background: transparent;
        color: inherit;
        font: inherit;
        line-height: inherit;
        text-align: left;
        cursor: pointer;
        padding: 0;
        margin: 0;
        vertical-align: baseline;
        border-radius: var(--radius-sm);
        transition:
          color var(--transition),
          background var(--transition);
      }
      .frag:hover:not(:disabled) {
        color: var(--text-primary);
      }
      .frag:focus-visible {
        outline: none;
        box-shadow: 0 0 0 2px var(--accent-ring);
      }
      .frag:disabled {
        cursor: default;
      }
      .frag.is-active {
        color: var(--text-primary);
        background: var(--accent-soft);
        box-shadow: 0 0 0 3px var(--accent-soft);
      }
      .turn-empty,
      .empty-card {
        padding: var(--space-5);
        text-align: center;
      }
      .empty-card {
        padding: var(--space-6);
      }

      @media (max-width: 720px) {
        .player-main {
          flex-wrap: wrap;
        }
        .find-input {
          width: 110px;
        }
      }
      @media (prefers-reduced-motion: reduce) {
        .audio-panel,
        .saved-toast {
          animation: none;
        }
      }
    `,
  ],
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
  /** Opt-in voiceprint speaker suggestions for the timeline legend. */
  readonly speakerSuggestions = input<SpeakerSuggestion[]>([]);
  /** Transient pin confirmation ("Pinned 2:14 — …"). */
  readonly pinMsg = input("");
  /** Inline pin error. */
  readonly pinError = input("");

  // --- Outputs back to the shell (which owns the IPC) ---------------------
  /** Retry the timeline fetch (from the timeline's (retry)). */
  readonly retryTimeline = output<void>();
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

  /** True when playback is inside [startS, endS) — highlights the live fragment. */
  isActiveSegment(startS: number, endS: number): boolean {
    const t = this.currentTime();
    return t >= startS && t < endS;
  }

  /** Find-box input handler → the `query` signal. */
  onQuery(event: Event): void {
    this.query.set((event.target as HTMLInputElement).value);
  }

  /** Clear the Find box. */
  clearQuery(): void {
    this.query.set("");
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
