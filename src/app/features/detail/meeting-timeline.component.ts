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
import type { MeetingTimeline as MeetingTimelineData } from "../../core/models";

/** A speaker turn resolved to render geometry + a stable lane + palette index. */
interface SpeakerBlock {
  speaker: string;
  startS: number;
  endS: number;
  /** left edge, 0–100 (% of total). */
  left: number;
  /** width, 0–100 (% of total). */
  width: number;
  /** index into the categorical palette (stable per speaker). */
  hue: number;
  /** delay seed for the staggered grow-in. */
  order: number;
}

/** One lane (= one unique speaker) of the speaker track. */
interface SpeakerLane {
  speaker: string;
  hue: number;
  /** total talk time across all this speaker's turns, seconds. */
  talkS: number;
  blocks: SpeakerBlock[];
}

/** A topic span resolved to render geometry + palette index. */
interface TopicBlock {
  label: string;
  startS: number;
  endS: number;
  left: number;
  width: number;
  hue: number;
  order: number;
  /**
   * Block is too narrow to carry a readable label → render colour-only (the
   * chapter chip row above + the hover tooltip still name it). Guards the dense
   * case so short/adjacent topics don't show overlapping truncated text.
   */
  narrow: boolean;
}

/** An axis tick: position (0–100) + its mono label. */
interface AxisTick {
  pct: number;
  label: string;
}

/**
 * "Who spoke when / what was discussed" — two stacked, horizontally-aligned
 * time tracks sharing one scale + one playhead. PURE CSS/HTML (no chart lib).
 *
 * It is a presentational sibling of the audio player: the parent owns the
 * `<audio>` element + its signals. This component renders geometry from the
 * AI-derived {@link MeetingTimelineData} and emits `seek` (seconds) / `retry`;
 * the live playhead + active states are driven by the `currentTime` input.
 *
 * Lives in its own file so its inline styles get their own per-component
 * `anyComponentStyle` budget (the detail component's styles are near the cap).
 */
@Component({
  selector: "app-meeting-timeline",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="tl card" [class.is-busy]="loading()">
      <div class="tl-head">
        <h3 class="tl-title">Timeline</h3>
        @if (ready()) {
          <span class="tl-sub">Who spoke when &amp; what was discussed</span>
        }
      </div>

      @if (loading()) {
        <!-- Shimmer skeleton while the LLM derives + caches the timeline. -->
        <div class="tl-skeleton" aria-busy="true" aria-live="polite">
          <span class="sk-label">Analysing speakers &amp; topics…</span>
          <div class="sk-track">
            @for (b of skeletonBars; track $index) {
              <span
                class="sk-bar"
                [style.left.%]="b.left"
                [style.width.%]="b.width"
                [style.--i]="$index"
              ></span>
            }
          </div>
          <div class="sk-track sk-track--thin">
            @for (b of skeletonRibbon; track $index) {
              <span
                class="sk-bar"
                [style.left.%]="b.left"
                [style.width.%]="b.width"
                [style.--i]="$index + 4"
              ></span>
            }
          </div>
        </div>
      } @else if (unavailable()) {
        <!-- Quiet, non-blocking fallback with a retry affordance. -->
        <div class="tl-unavailable">
          <span class="tl-unavailable-dot" aria-hidden="true"></span>
          <span class="tl-unavailable-text">Timeline unavailable</span>
          <button
            type="button"
            class="btn btn-ghost tl-retry"
            (click)="retry.emit()"
          >
            Retry
          </button>
        </div>
      } @else {
        <!-- Shared axis: a thin baseline with 3–4 mono time ticks. -->
        <div
          class="tl-axis"
          role="slider"
          tabindex="0"
          aria-label="Seek timeline"
          [attr.aria-valuemin]="0"
          [attr.aria-valuemax]="totalRounded()"
          [attr.aria-valuenow]="currentRounded()"
          (click)="seekFromTrack($event)"
          (mousemove)="onScrubMove($event)"
          (mouseleave)="onScrubLeave()"
          (keydown)="onAxisKey($event)"
        >
          @for (t of ticks(); track t.pct) {
            <span class="tl-tick" [style.left.%]="t.pct">
              <span class="tl-tick-mark" aria-hidden="true"></span>
              <span class="tl-tick-label">{{ t.label }}</span>
            </span>
          }
          <!-- Hover-scrub preview: a thin line + a m:ss bubble at the cursor. -->
          @if (hoverPct(); as hp) {
            <span class="tl-scrub" [style.left.%]="hp" aria-hidden="true">
              <span class="tl-scrub-bubble">{{ hoverLabel() }}</span>
            </span>
          }
          <!-- Shared playhead — sits in the axis, spans up over both tracks. -->
          @if (showPlayhead()) {
            <span
              class="tl-playhead"
              [style.left.%]="playheadPct()"
              aria-hidden="true"
            >
              <span class="tl-playhead-knob"></span>
            </span>
            <!-- Pin-this-moment: emits the current playhead time (seconds). -->
            <button
              type="button"
              class="tl-pin"
              [style.left.%]="playheadPct()"
              [attr.aria-label]="'Pin this moment at ' + fmt(currentTime())"
              (click)="onPin($event)"
            >
              <span class="tl-pin-glyph" aria-hidden="true">📌</span>
              <span class="tl-pin-tip" aria-hidden="true">Pin moment</span>
            </button>
          }
        </div>

        <!-- SPEAKER TIMELINE — one lane per unique speaker. -->
        @if (lanes().length) {
          <div class="tl-group">
            <div class="tl-group-head">
              <span class="tl-group-label">Speakers</span>
              <div class="tl-legend">
                @for (lane of lanes(); track lane.speaker) {
                  <span class="legend-item">
                    <span
                      class="legend-dot"
                      [style.background]="dotColor(lane.hue)"
                      aria-hidden="true"
                    ></span>
                    @if (editingLabel() === lane.speaker) {
                      <!-- Inline rename field — commits on Enter/blur, cancels on Escape. -->
                      <input
                        #renameInput
                        type="text"
                        class="legend-edit"
                        aria-label="Speaker name"
                        autocapitalize="words"
                        autocomplete="off"
                        spellcheck="false"
                        [value]="labelDraft()"
                        (input)="onRenameInput($event)"
                        (keydown.enter)="commitRename(lane.speaker)"
                        (keydown.escape)="cancelRename($event)"
                        (blur)="commitRename(lane.speaker)"
                        (click)="$event.stopPropagation()"
                      />
                    } @else {
                      <button
                        type="button"
                        class="legend-rename"
                        [attr.aria-label]="'Rename ' + lane.speaker"
                        (click)="startRename($event, lane.speaker)"
                      >
                        <span class="legend-name">{{ lane.speaker }}</span>
                        <span class="legend-pencil" aria-hidden="true">✎</span>
                      </button>
                    }
                    <span class="legend-time">{{ fmt(lane.talkS) }}</span>
                  </span>
                }
              </div>
            </div>

            <div
              class="tl-track tl-track--lanes"
              role="slider"
              tabindex="0"
              aria-label="Seek speaker timeline"
              [attr.aria-valuemin]="0"
              [attr.aria-valuemax]="totalRounded()"
              [attr.aria-valuenow]="currentRounded()"
              (click)="seekFromTrack($event)"
              (mousemove)="onScrubMove($event)"
              (mouseleave)="onScrubLeave()"
              (keydown)="onAxisKey($event)"
            >
              @if (hoverPct(); as hp) {
                <span
                  class="tl-track-scrub"
                  [style.left.%]="hp"
                  aria-hidden="true"
                ></span>
              }
              @for (lane of lanes(); track lane.speaker) {
                <div class="tl-lane">
                  @for (blk of lane.blocks; track blk.order) {
                    <button
                      type="button"
                      class="tl-block"
                      [class.is-active]="isActive(blk.startS, blk.endS)"
                      [style.left.%]="blk.left"
                      [style.width.%]="blk.width"
                      [style.--i]="blk.order"
                      [style.background]="blockColor(blk.hue)"
                      [style.border-color]="edgeColor(blk.hue)"
                      [attr.aria-label]="
                        lane.speaker + ', ' + range(blk.startS, blk.endS)
                      "
                      (click)="onBlock($event, blk.startS)"
                    >
                      <span class="tl-tip" aria-hidden="true">
                        <span class="tl-tip-name">{{ lane.speaker }}</span>
                        <span class="tl-tip-time">{{
                          range(blk.startS, blk.endS)
                        }}</span>
                      </span>
                    </button>
                  }
                </div>
              }
              @if (showPlayhead()) {
                <span
                  class="tl-track-playhead"
                  [style.left.%]="playheadPct()"
                  aria-hidden="true"
                ></span>
              }
            </div>
          </div>
        }

        <!-- TOPIC TIMELINE — a single ribbon of labelled spans, same scale. -->
        @if (topics().length) {
          <div class="tl-group">
            <div class="tl-group-head">
              <span class="tl-group-label">Topics</span>
              <span class="tl-group-hint">Jump to a chapter</span>
            </div>

            <!-- CHAPTERS — the topic spans as a clickable chapter list. -->
            <div class="tl-chapters" role="list" aria-label="Chapters">
              @for (ch of chapters(); track ch.order) {
                <button
                  type="button"
                  class="tl-chapter"
                  role="listitem"
                  [class.is-active]="isActiveChapter(ch.startS, ch.endS)"
                  [style.--i]="ch.order"
                  [attr.aria-label]="
                    'Chapter ' + ch.label + ', starts ' + fmt(ch.startS)
                  "
                  (click)="onBlock($event, ch.startS)"
                >
                  <span
                    class="tl-chapter-dot"
                    [style.background]="dotColor(ch.hue)"
                    aria-hidden="true"
                  ></span>
                  <span class="tl-chapter-label">{{ ch.label }}</span>
                  <span class="tl-chapter-time">{{ fmt(ch.startS) }}</span>
                </button>
              }
            </div>

            <div
              class="tl-track tl-track--ribbon"
              role="slider"
              tabindex="0"
              aria-label="Seek topic timeline"
              [attr.aria-valuemin]="0"
              [attr.aria-valuemax]="totalRounded()"
              [attr.aria-valuenow]="currentRounded()"
              (click)="seekFromTrack($event)"
              (mousemove)="onScrubMove($event)"
              (mouseleave)="onScrubLeave()"
              (keydown)="onAxisKey($event)"
            >
              @if (hoverPct(); as hp) {
                <span
                  class="tl-track-scrub"
                  [style.left.%]="hp"
                  aria-hidden="true"
                ></span>
              }
              @for (top of topics(); track top.order) {
                <button
                  type="button"
                  class="tl-topic"
                  [class.is-active]="isActive(top.startS, top.endS)"
                  [style.left.%]="top.left"
                  [style.width.%]="top.width"
                  [style.--i]="top.order"
                  [style.background]="topicColor(top.hue)"
                  [style.border-color]="edgeColor(top.hue)"
                  [attr.aria-label]="
                    top.label + ', ' + range(top.startS, top.endS)
                  "
                  [attr.title]="top.label"
                  (click)="onBlock($event, top.startS)"
                >
                  @if (!top.narrow) {
                    <span class="tl-topic-label">{{ top.label }}</span>
                  }
                  <span class="tl-tip" aria-hidden="true">
                    <span class="tl-tip-name">{{ top.label }}</span>
                    <span class="tl-tip-time">{{
                      range(top.startS, top.endS)
                    }}</span>
                  </span>
                </button>
              }
              @if (showPlayhead()) {
                <span
                  class="tl-track-playhead"
                  [style.left.%]="playheadPct()"
                  aria-hidden="true"
                ></span>
              }
            </div>
          </div>
        }
      }
    </div>
  `,
  styles: [
    `
      :host {
        display: block;
      }

      .tl {
        position: relative;
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
        padding: var(--space-5);
        overflow: hidden;
        animation: rise 420ms var(--transition) both;
      }
      /* A faint aurora wash to lift the glass above the page surface. */
      .tl::before {
        content: "";
        position: absolute;
        inset: 0;
        pointer-events: none;
        background: radial-gradient(
          120% 90% at 12% -10%,
          rgba(110, 118, 255, 0.1),
          transparent 60%
        );
      }
      .tl > * {
        position: relative;
        z-index: 1;
      }

      /* --- Head --- */
      .tl-head {
        display: flex;
        align-items: baseline;
        flex-wrap: wrap;
        gap: var(--space-2) var(--space-3);
      }
      .tl-title {
        margin: 0;
      }
      .tl-sub {
        color: var(--text-muted);
        font-size: 0.8125rem;
      }

      /* --- Shared axis + ticks --- */
      .tl-axis {
        position: relative;
        height: 30px;
        margin-top: var(--space-1);
        border-radius: var(--radius-sm);
        cursor: pointer;
        outline: none;
      }
      .tl-axis::after {
        content: "";
        position: absolute;
        left: 0;
        right: 0;
        top: 0;
        height: 1px;
        background: var(--border);
      }
      .tl-axis:focus-visible {
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .tl-tick {
        position: absolute;
        top: 0;
        transform: translateX(-50%);
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: var(--space-1);
        pointer-events: none;
      }
      .tl-tick:first-child {
        transform: translateX(0);
        align-items: flex-start;
      }
      .tl-tick:last-child {
        transform: translateX(-100%);
        align-items: flex-end;
      }
      .tl-tick-mark {
        width: 1px;
        height: 6px;
        background: var(--border-strong);
      }
      .tl-tick-label {
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-size: 0.6875rem;
        font-variant-numeric: tabular-nums;
        letter-spacing: -0.02em;
      }

      /* --- Track groups --- */
      .tl-group {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .tl-group-head {
        display: flex;
        align-items: center;
        justify-content: space-between;
        flex-wrap: wrap;
        gap: var(--space-2) var(--space-3);
      }
      .tl-group-label {
        color: var(--text-muted);
        font-size: 0.6875rem;
        font-weight: 600;
        letter-spacing: 0.06em;
        text-transform: uppercase;
      }

      /* --- Legend --- */
      .tl-legend {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-2) var(--space-4);
      }
      .legend-item {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        min-width: 0;
      }
      .legend-dot {
        width: 9px;
        height: 9px;
        border-radius: 50%;
        flex: none;
        box-shadow: 0 0 0 3px rgba(255, 255, 255, 0.04);
      }
      .legend-name {
        color: var(--text-secondary);
        font-size: 0.8125rem;
        font-weight: 550;
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
        max-width: 16ch;
      }
      .legend-time {
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-size: 0.75rem;
        font-variant-numeric: tabular-nums;
      }

      /* --- Click-to-rename a speaker (manual labelling) --- */
      .legend-rename {
        display: inline-flex;
        align-items: center;
        gap: var(--space-1);
        min-width: 0;
        max-width: 100%;
        padding: 2px var(--space-2);
        margin: -2px 0;
        border: 1px solid transparent;
        border-radius: var(--radius-sm);
        background: transparent;
        color: inherit;
        font: inherit;
        cursor: text;
        transition:
          background var(--transition),
          border-color var(--transition);
      }
      .legend-rename:hover {
        background: var(--surface-hover);
        border-color: var(--border-subtle);
      }
      .legend-rename:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .legend-pencil {
        color: var(--text-muted);
        font-size: 0.6875rem;
        line-height: 1;
        opacity: 0;
        transform: translateY(0.5px);
        transition: opacity var(--transition);
        pointer-events: none;
      }
      .legend-rename:hover .legend-pencil,
      .legend-rename:focus-visible .legend-pencil {
        opacity: 1;
      }
      .legend-edit {
        width: auto;
        max-width: 16ch;
        height: 24px;
        padding: 0 var(--space-2);
        border-radius: var(--radius-sm);
        font-size: 0.8125rem;
        font-weight: 550;
      }

      /* --- The shared scale tracks --- */
      .tl-track {
        position: relative;
        border-radius: var(--radius-md);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
        cursor: pointer;
        overflow: hidden;
      }
      .tl-track:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .tl-track--lanes {
        display: flex;
        flex-direction: column;
        gap: 6px;
        padding: 6px;
      }
      .tl-track--ribbon {
        height: 40px;
      }
      .tl-lane {
        position: relative;
        height: 26px;
        border-radius: var(--radius-sm);
        background: rgba(255, 255, 255, 0.02);
      }

      /* --- Speaker blocks --- */
      .tl-block {
        position: absolute;
        top: 2px;
        bottom: 2px;
        min-width: 3px;
        padding: 0;
        border: 1px solid transparent;
        border-radius: var(--radius-sm);
        cursor: pointer;
        transform-origin: left center;
        animation: grow-in 460ms var(--ease-spring) both;
        animation-delay: calc(var(--i, 0) * 22ms + 120ms);
        transition:
          filter var(--transition),
          box-shadow var(--transition),
          transform var(--transition-fast);
      }
      .tl-block:hover {
        filter: brightness(1.14) saturate(1.1);
        z-index: 5;
      }
      .tl-block:active {
        transform: scaleY(0.92);
      }
      .tl-block:focus-visible {
        outline: none;
        box-shadow:
          0 0 0 2px var(--surface-base),
          0 0 0 4px var(--accent-ring);
        z-index: 6;
      }
      .tl-block.is-active {
        filter: brightness(1.22) saturate(1.15);
        box-shadow:
          0 0 0 1px rgba(255, 255, 255, 0.55),
          0 6px 18px rgba(0, 0, 0, 0.45);
        z-index: 7;
      }

      /* --- Topic ribbon spans --- */
      .tl-topic {
        position: absolute;
        top: 4px;
        bottom: 4px;
        /* Floor short/adjacent chapters so they never collapse to an
           unreadable sliver (label is hidden below the width threshold). */
        min-width: 14px;
        display: flex;
        align-items: center;
        padding: 0 var(--space-2);
        border: 1px solid transparent;
        border-radius: var(--radius-sm);
        cursor: pointer;
        overflow: hidden;
        transform-origin: left center;
        animation: grow-in 460ms var(--ease-spring) both;
        animation-delay: calc(var(--i, 0) * 34ms + 200ms);
        transition:
          filter var(--transition),
          box-shadow var(--transition),
          transform var(--transition-fast);
      }
      .tl-topic:hover {
        filter: brightness(1.12) saturate(1.08);
        z-index: 5;
      }
      .tl-topic:active {
        transform: scaleY(0.94);
      }
      .tl-topic:focus-visible {
        outline: none;
        box-shadow:
          0 0 0 2px var(--surface-base),
          0 0 0 4px var(--accent-ring);
        z-index: 6;
      }
      .tl-topic.is-active {
        filter: brightness(1.18) saturate(1.12);
        box-shadow:
          0 0 0 1px rgba(255, 255, 255, 0.5),
          0 6px 18px rgba(0, 0, 0, 0.45);
        z-index: 7;
      }
      .tl-topic-label {
        color: var(--text-primary);
        font-size: 0.75rem;
        font-weight: 600;
        letter-spacing: -0.01em;
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
        text-shadow: 0 1px 2px rgba(0, 0, 0, 0.45);
        pointer-events: none;
      }

      /* --- Hover tooltip (pure CSS, shared by blocks + topics) --- */
      .tl-tip {
        position: absolute;
        left: 50%;
        bottom: calc(100% + 8px);
        transform: translateX(-50%) translateY(4px);
        display: flex;
        flex-direction: column;
        gap: 1px;
        padding: var(--space-2) var(--space-3);
        border-radius: var(--radius-sm);
        background: var(--surface-overlay);
        border: 1px solid var(--border);
        box-shadow: var(--shadow-md);
        white-space: nowrap;
        opacity: 0;
        pointer-events: none;
        z-index: 20;
        transition:
          opacity var(--transition),
          transform var(--transition);
      }
      .tl-tip::after {
        content: "";
        position: absolute;
        left: 50%;
        top: 100%;
        transform: translateX(-50%);
        border: 5px solid transparent;
        border-top-color: var(--surface-overlay);
      }
      .tl-block:hover .tl-tip,
      .tl-topic:hover .tl-tip,
      .tl-block:focus-visible .tl-tip,
      .tl-topic:focus-visible .tl-tip {
        opacity: 1;
        transform: translateX(-50%) translateY(0);
      }
      .tl-tip-name {
        color: var(--text-primary);
        font-size: 0.8125rem;
        font-weight: 600;
      }
      .tl-tip-time {
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-size: 0.6875rem;
        font-variant-numeric: tabular-nums;
      }

      /* --- Shared playhead --- */
      .tl-playhead {
        position: absolute;
        top: -6px;
        bottom: -2px;
        width: 0;
        transform: translateX(-50%);
        pointer-events: none;
        z-index: 8;
        transition: left 120ms linear;
      }
      .tl-playhead-knob {
        position: absolute;
        top: -3px;
        left: 0;
        width: 9px;
        height: 9px;
        transform: translateX(-50%);
        border-radius: 50%;
        background: var(--text-primary);
        box-shadow:
          0 0 0 3px var(--surface-base),
          var(--shadow-accent);
      }
      .tl-track-playhead {
        position: absolute;
        top: 0;
        bottom: 0;
        width: 2px;
        transform: translateX(-1px);
        background: linear-gradient(
          to bottom,
          rgba(246, 246, 250, 0.95),
          rgba(110, 118, 255, 0.6)
        );
        box-shadow: 0 0 10px rgba(110, 118, 255, 0.6);
        pointer-events: none;
        z-index: 8;
        transition: left 120ms linear;
      }

      /* --- Skeleton (loading) --- */
      .tl-skeleton {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .sk-label {
        color: var(--text-secondary);
        font-size: 0.875rem;
      }
      .sk-track {
        position: relative;
        height: 58px;
        border-radius: var(--radius-md);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
        overflow: hidden;
      }
      .sk-track--thin {
        height: 40px;
      }
      .sk-bar {
        position: absolute;
        top: 8px;
        bottom: 8px;
        border-radius: var(--radius-sm);
        background: linear-gradient(
          100deg,
          rgba(255, 255, 255, 0.04) 30%,
          rgba(255, 255, 255, 0.11) 50%,
          rgba(255, 255, 255, 0.04) 70%
        );
        background-size: 220% 100%;
        animation: shimmer 1.5s ease-in-out infinite;
        animation-delay: calc(var(--i, 0) * 120ms);
      }

      /* --- Unavailable note --- */
      .tl-unavailable {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-2) 0;
      }
      .tl-unavailable-dot {
        width: 8px;
        height: 8px;
        border-radius: 50%;
        background: var(--text-muted);
        opacity: 0.55;
        flex: none;
      }
      .tl-unavailable-text {
        color: var(--text-muted);
        font-size: 0.875rem;
        flex: 1 1 auto;
      }
      .tl-retry {
        flex: none;
        height: 32px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
      }

      /* --- Hover-scrub preview (axis bubble + per-track guide line) --- */
      .tl-group-hint {
        color: var(--text-muted);
        font-size: 0.6875rem;
        font-weight: 500;
        letter-spacing: 0.02em;
      }
      .tl-scrub {
        position: absolute;
        top: -2px;
        bottom: -2px;
        width: 1px;
        transform: translateX(-50%);
        background: var(--border-strong);
        pointer-events: none;
        z-index: 9;
      }
      .tl-scrub-bubble {
        position: absolute;
        left: 50%;
        bottom: calc(100% + 4px);
        transform: translateX(-50%);
        padding: 2px var(--space-2);
        border-radius: var(--radius-sm);
        background: var(--surface-overlay);
        border: 1px solid var(--border);
        box-shadow: var(--shadow-sm);
        color: var(--text-secondary);
        font-family: var(--font-mono);
        font-size: 0.6875rem;
        font-variant-numeric: tabular-nums;
        white-space: nowrap;
      }
      .tl-track-scrub {
        position: absolute;
        top: 0;
        bottom: 0;
        width: 1px;
        transform: translateX(-50%);
        background: rgba(246, 246, 250, 0.28);
        pointer-events: none;
        z-index: 6;
      }

      /* --- Pin-this-moment (rides the axis playhead) --- */
      .tl-pin {
        position: absolute;
        top: -30px;
        transform: translateX(-50%);
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 24px;
        height: 24px;
        padding: 0;
        border: 1px solid var(--border);
        border-radius: var(--radius-pill);
        background: var(--surface-overlay);
        box-shadow: var(--shadow-sm);
        cursor: pointer;
        z-index: 10;
        transition:
          transform var(--transition-fast),
          box-shadow var(--transition),
          border-color var(--transition);
      }
      .tl-pin:hover {
        transform: translateX(-50%) translateY(-1px) scale(1.08);
        border-color: var(--accent);
        box-shadow: var(--shadow-accent);
      }
      .tl-pin:active {
        transform: translateX(-50%) scale(0.94);
      }
      .tl-pin:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .tl-pin-glyph {
        font-size: 0.75rem;
        line-height: 1;
      }
      .tl-pin-tip {
        position: absolute;
        left: 50%;
        bottom: calc(100% + 6px);
        transform: translateX(-50%) translateY(3px);
        padding: 2px var(--space-2);
        border-radius: var(--radius-sm);
        background: var(--surface-overlay);
        border: 1px solid var(--border);
        box-shadow: var(--shadow-sm);
        color: var(--text-primary);
        font-size: 0.6875rem;
        font-weight: 600;
        white-space: nowrap;
        opacity: 0;
        pointer-events: none;
        transition:
          opacity var(--transition),
          transform var(--transition);
      }
      .tl-pin:hover .tl-pin-tip,
      .tl-pin:focus-visible .tl-pin-tip {
        opacity: 1;
        transform: translateX(-50%) translateY(0);
      }

      /* --- Chapters (topic spans as a clickable list) --- */
      .tl-chapters {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-2);
      }
      .tl-chapter {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        max-width: 100%;
        padding: var(--space-1) var(--space-3);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        color: var(--text-secondary);
        font: inherit;
        font-size: 0.75rem;
        cursor: pointer;
        animation: rise 320ms var(--transition) both;
        animation-delay: calc(var(--i, 0) * 30ms + 180ms);
        transition:
          background var(--transition),
          border-color var(--transition),
          color var(--transition),
          transform var(--transition-fast);
      }
      .tl-chapter:hover {
        background: var(--surface-hover);
        border-color: var(--border-strong);
        color: var(--text-primary);
        transform: translateY(-1px);
      }
      .tl-chapter:active {
        transform: translateY(0);
      }
      .tl-chapter:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .tl-chapter.is-active {
        background: var(--accent-soft);
        border-color: transparent;
        color: var(--text-primary);
      }
      .tl-chapter-dot {
        width: 7px;
        height: 7px;
        border-radius: 50%;
        flex: none;
      }
      .tl-chapter-label {
        font-weight: 600;
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
        max-width: 22ch;
      }
      .tl-chapter-time {
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-size: 0.6875rem;
        font-variant-numeric: tabular-nums;
      }
      .tl-chapter.is-active .tl-chapter-time {
        color: var(--accent-hover);
      }

      @keyframes grow-in {
        from {
          opacity: 0;
          transform: scaleX(0);
        }
        to {
          opacity: 1;
          transform: scaleX(1);
        }
      }
      @keyframes shimmer {
        from {
          background-position: 130% 0;
        }
        to {
          background-position: -130% 0;
        }
      }

      @media (prefers-reduced-motion: reduce) {
        .tl-block,
        .tl-topic,
        .tl-chapter {
          animation: none;
        }
        .tl-playhead,
        .tl-track-playhead {
          transition: none;
        }
      }
    `,
  ],
})
export class MeetingTimelineComponent {
  /** AI-derived speakers + topics; null until the first load resolves. */
  readonly timeline = input<MeetingTimelineData | null>(null);
  /** Meeting length in seconds — the denominator for every block's geometry. */
  readonly total = input<number>(0);
  /** Live playback head, seconds (mirrors the parent's audio currentTime). */
  readonly currentTime = input<number>(0);
  /** True while getTimeline() is in flight. */
  readonly loading = input<boolean>(false);
  /** True when getTimeline() errored or resolved empty. */
  readonly error = input<boolean>(false);
  /** Whether a seek will actually move audio (false when audioPath is null). */
  readonly hasAudio = input<boolean>(false);

  /** Seek request in seconds — the parent applies it to its `<audio>`. */
  readonly seek = output<number>();
  /** Re-run getTimeline(). */
  readonly retry = output<void>();
  /**
   * Pin request carrying the CURRENT playhead position in seconds. Purely
   * presentational — the parent is responsible for the IPC pin + clipboard.
   */
  readonly pin = output<number>();
  /**
   * Manual speaker re-labelling — fired when an inline legend edit is committed
   * with a non-empty, changed name (e.g. "User 1" → "Sarah"). Purely
   * presentational: the parent owns the IPC call + timeline refresh.
   */
  readonly renameSpeaker = output<{ oldLabel: string; newLabel: string }>();

  private readonly injector = inject(Injector);

  /** The speaker currently being inline-renamed (its original label), or null. */
  readonly editingLabel = signal<string | null>(null);
  /** Working copy of the inline rename field (input → signal). */
  readonly labelDraft = signal("");
  /** Focusable rename field — focused after it renders (afterNextRender). */
  private readonly renameInput =
    viewChild<ElementRef<HTMLInputElement>>("renameInput");

  /**
   * Live hover-scrub position on the shared time track, 0–100 (% of total),
   * or null when the pointer is not over a track/axis. Drives a thin preview
   * line + a floating time read-out without disturbing the real playhead.
   */
  readonly hoverPct = signal<number | null>(null);

  /** The hovered position formatted m:ss — shown in the scrub bubble. */
  readonly hoverLabel = computed(() => {
    const pct = this.hoverPct();
    if (pct === null) {
      return "";
    }
    return this.fmt((pct / 100) * this.span());
  });

  /** Static decoration for the loading skeleton (no data yet). */
  protected readonly skeletonBars = [
    { left: 1, width: 18 },
    { left: 22, width: 30 },
    { left: 55, width: 14 },
    { left: 72, width: 26 },
  ];
  protected readonly skeletonRibbon = [
    { left: 1, width: 34 },
    { left: 38, width: 24 },
    { left: 64, width: 35 },
  ];

  /**
   * Largest *meaningful* content time across every speaker turn + topic span
   * (seconds); 0 when there is no data. This — not the full recording length —
   * is what the geometry scales to, so a meeting with speech only in its first
   * stretch isn't bunched into a sliver of the track while a silent tail eats
   * the rest of the width.
   */
  private readonly contentEnd = computed(() => {
    const tl = this.timeline();
    let max = 0;
    for (const s of tl?.speakers ?? []) {
      max = Math.max(max, s.endS);
    }
    for (const t of tl?.topics ?? []) {
      max = Math.max(max, t.endS);
    }
    return max;
  });

  /**
   * Effective denominator for ALL geometry (blocks, topics, ticks, playhead,
   * scrub). Scales to the content end + a little head-room (×1.06) so the
   * meaningful content spreads across the full width — but NEVER exceeds the
   * real recording length (`total`), and falls back to `total` unchanged when
   * the content already (nearly) fills the recording (content ≥ 0.9×total, i.e.
   * the tail isn't mostly silent). With no recording length known it scales to
   * the raw content; with no content at all it is `total` (or 0).
   */
  private readonly span = computed(() => {
    const total = this.total();
    const content = this.contentEnd();
    if (content <= 0) {
      // No speakers/topics → nothing to scale to; use the recording length.
      return total;
    }
    if (total <= 0) {
      // Recording length unknown → scale to content with head-room.
      return content * 1.06;
    }
    if (content >= total * 0.9) {
      // Content fills (almost) the whole recording → keep the true duration.
      return total;
    }
    // Silent tail: scale to content + head-room, capped at the real duration.
    return Math.min(total, content * 1.06);
  });

  /** Stable colour index for each distinct name (order of first appearance). */
  private hueIndex(names: string[]): Map<string, number> {
    const map = new Map<string, number>();
    for (const n of names) {
      if (!map.has(n)) {
        map.set(n, map.size % PALETTE.length);
      }
    }
    return map;
  }

  /** Speaker turns grouped into per-speaker lanes with geometry + talk totals. */
  readonly lanes = computed<SpeakerLane[]>(() => {
    const turns = this.timeline()?.speakers ?? [];
    const total = this.span();
    if (turns.length === 0 || total <= 0) {
      return [];
    }
    const hues = this.hueIndex(turns.map((t) => t.speaker));
    const bySpeaker = new Map<string, SpeakerLane>();
    let order = 0;
    for (const t of turns) {
      const start = clamp(t.startS, 0, total);
      const end = clamp(t.endS, start, total);
      const hue = hues.get(t.speaker) ?? 0;
      let lane = bySpeaker.get(t.speaker);
      if (!lane) {
        lane = { speaker: t.speaker, hue, talkS: 0, blocks: [] };
        bySpeaker.set(t.speaker, lane);
      }
      lane.talkS += Math.max(0, t.endS - t.startS);
      lane.blocks.push({
        speaker: t.speaker,
        startS: t.startS,
        endS: t.endS,
        left: (start / total) * 100,
        width: Math.max(0.4, ((end - start) / total) * 100),
        hue,
        order: order++,
      });
    }
    return [...bySpeaker.values()];
  });

  /** Topic spans resolved to ribbon geometry. */
  readonly topics = computed<TopicBlock[]>(() => {
    const spans = this.timeline()?.topics ?? [];
    const total = this.span();
    if (spans.length === 0 || total <= 0) {
      return [];
    }
    const hues = this.hueIndex(spans.map((s) => s.label));
    return spans.map((s, i) => {
      const start = clamp(s.startS, 0, total);
      const end = clamp(s.endS, start, total);
      const width = Math.max(1, ((end - start) / total) * 100);
      return {
        label: s.label,
        startS: s.startS,
        endS: s.endS,
        left: (start / total) * 100,
        width,
        hue: hues.get(s.label) ?? 0,
        order: i,
        // < ~8% of the track can't fit readable text → show colour only.
        narrow: width < 8,
      };
    });
  });

  /**
   * The topic spans presented as an ordered chapter list (a label row above
   * the ribbon). Each chapter seeks to its start when clicked — the ribbon and
   * the label row are two views onto the same data + the same `seek`.
   */
  readonly chapters = computed(() =>
    this.topics().map((t) => ({
      label: t.label,
      startS: t.startS,
      endS: t.endS,
      hue: t.hue,
      order: t.order,
    })),
  );

  /** True once we have at least one lane or topic to draw. */
  readonly ready = computed(
    () =>
      !this.loading() && (this.lanes().length > 0 || this.topics().length > 0),
  );

  /** True when not loading and there is genuinely nothing to show (or errored). */
  readonly unavailable = computed(
    () =>
      !this.loading() &&
      (this.error() ||
        (this.lanes().length === 0 && this.topics().length === 0)),
  );

  /** 3–4 evenly-spaced axis ticks: 0:00 … total. */
  readonly ticks = computed<AxisTick[]>(() => {
    const total = this.span();
    if (total <= 0) {
      return [];
    }
    const steps = total >= 60 ? 4 : 3;
    const out: AxisTick[] = [];
    for (let i = 0; i <= steps; i++) {
      const at = (total * i) / steps;
      out.push({ pct: (i / steps) * 100, label: this.fmt(at) });
    }
    return out;
  });

  /** Playhead position as a 0–100 percentage of the shared scale. */
  readonly playheadPct = computed(() => {
    const total = this.span();
    if (total <= 0) {
      return 0;
    }
    return clamp((this.currentTime() / total) * 100, 0, 100);
  });

  /** Show the playhead once playback has started (avoids a stuck 0 marker). */
  readonly showPlayhead = computed(
    () => this.ready() && this.currentTime() > 0,
  );

  protected readonly totalRounded = computed(() => Math.round(this.span()));
  protected readonly currentRounded = computed(() =>
    Math.round(clamp(this.currentTime(), 0, this.span())),
  );

  /** True when playback is inside [startS, endS) — drives the live highlight. */
  isActive(startS: number, endS: number): boolean {
    const t = this.currentTime();
    return t >= startS && t < endS;
  }

  /** Click a block → request a seek to its start (parent decides if audio moves). */
  onBlock(event: MouseEvent, startS: number): void {
    event.stopPropagation();
    this.seek.emit(startS);
  }

  /** Click anywhere on a track/axis → seek to that fraction of the total. */
  seekFromTrack(event: MouseEvent): void {
    const total = this.span();
    if (total <= 0) {
      return;
    }
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    if (rect.width <= 0) {
      return;
    }
    const ratio = clamp((event.clientX - rect.left) / rect.width, 0, 1);
    this.seek.emit(ratio * total);
  }

  /**
   * Track the pointer across a time track/axis → update the hover-scrub preview
   * (a thin line + a m:ss bubble). Pure UI state; never seeks until clicked.
   */
  onScrubMove(event: MouseEvent): void {
    if (this.span() <= 0) {
      return;
    }
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    if (rect.width <= 0) {
      return;
    }
    this.hoverPct.set(
      clamp(((event.clientX - rect.left) / rect.width) * 100, 0, 100),
    );
  }

  /** Pointer left a time track → clear the scrub preview. */
  onScrubLeave(): void {
    this.hoverPct.set(null);
  }

  /**
   * "Pin this moment" → emit the CURRENT playhead position in seconds. The
   * component stays presentational; the parent owns the IPC + clipboard.
   */
  onPin(event: MouseEvent): void {
    event.stopPropagation();
    this.pin.emit(Math.max(0, this.currentTime()));
  }

  // --- Inline speaker rename (manual labelling; stays presentational) ------

  /**
   * Enter inline-edit mode for a legend speaker: seed the draft with the
   * current label and focus the field once it renders (zoneless-safe; no
   * setTimeout). The (renameSpeaker) output fires only on a committed change.
   */
  startRename(event: Event, speaker: string): void {
    event.stopPropagation();
    this.labelDraft.set(speaker);
    this.editingLabel.set(speaker);
    afterNextRender(
      () => {
        const el = this.renameInput()?.nativeElement;
        el?.focus();
        el?.select();
      },
      { injector: this.injector },
    );
  }

  /** Mirror the inline rename field value into the `labelDraft` signal. */
  onRenameInput(event: Event): void {
    this.labelDraft.set((event.target as HTMLInputElement).value);
  }

  /**
   * Commit the inline rename: ignore empty/unchanged names, else emit
   * `renameSpeaker` with the original + new label. Always leaves edit mode (so
   * a blur after Enter/Escape doesn't re-fire — the guard below also no-ops once
   * `editingLabel` is cleared).
   */
  commitRename(oldLabel: string): void {
    if (this.editingLabel() !== oldLabel) {
      return;
    }
    const newLabel = this.labelDraft().trim();
    this.editingLabel.set(null);
    if (!newLabel || newLabel === oldLabel) {
      return;
    }
    this.renameSpeaker.emit({ oldLabel, newLabel });
  }

  /** Escape cancels the inline rename without emitting. */
  cancelRename(event: Event): void {
    event.stopPropagation();
    this.editingLabel.set(null);
  }

  /** True when the playhead sits inside a chapter — highlights its label. */
  isActiveChapter(startS: number, endS: number): boolean {
    return this.isActive(startS, endS);
  }

  /** Keyboard seeking on the focusable axis (← / → by 5s, Home/End). */
  onAxisKey(event: KeyboardEvent): void {
    const total = this.span();
    if (total <= 0) {
      return;
    }
    const cur = clamp(this.currentTime(), 0, total);
    let next: number | null = null;
    switch (event.key) {
      case "ArrowLeft":
        next = Math.max(0, cur - 5);
        break;
      case "ArrowRight":
        next = Math.min(total, cur + 5);
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = total;
        break;
      default:
        return;
    }
    event.preventDefault();
    this.seek.emit(next);
  }

  // --- Colour helpers (small categorical data-viz palette; rgba/hsl only) ---

  protected dotColor(i: number): string {
    return PALETTE[i % PALETTE.length].dot;
  }
  protected blockColor(i: number): string {
    return PALETTE[i % PALETTE.length].fill;
  }
  protected topicColor(i: number): string {
    return PALETTE[i % PALETTE.length].topic;
  }
  protected edgeColor(i: number): string {
    return PALETTE[i % PALETTE.length].edge;
  }

  /** Seconds → m:ss (single source of truth for the axis + legend + tooltips). */
  fmt(s: number): string {
    const total = Math.max(0, Math.floor(s || 0));
    const m = Math.floor(total / 60);
    const sec = total % 60;
    return `${m}:${sec.toString().padStart(2, "0")}`;
  }

  /** "m:ss – m:ss" for tooltips + aria-labels. */
  range(startS: number, endS: number): string {
    return `${this.fmt(startS)}–${this.fmt(endS)}`;
  }
}

/** Clamp helper (kept module-local; no rxjs/util dependency). */
function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

/**
 * Small categorical data-viz palette (HSL-derived). Each entry pairs a soft
 * solid `fill` for speaker blocks, a slightly translucent `topic` for ribbon
 * spans, a saturated `dot` for legend chips, and a crisp `edge` border. These
 * are intentionally NOT global tokens — per the brief, a small categorical
 * palette is allowed for data-viz (like the existing accent-glow rgba).
 */
const PALETTE: { fill: string; topic: string; dot: string; edge: string }[] = [
  {
    fill: "hsl(244 90% 70% / 0.85)",
    topic: "hsl(244 80% 64% / 0.45)",
    dot: "hsl(244 90% 72%)",
    edge: "hsl(244 90% 80% / 0.7)",
  },
  {
    fill: "hsl(190 85% 58% / 0.82)",
    topic: "hsl(190 80% 52% / 0.42)",
    dot: "hsl(190 85% 60%)",
    edge: "hsl(190 85% 70% / 0.7)",
  },
  {
    fill: "hsl(330 85% 66% / 0.82)",
    topic: "hsl(330 78% 60% / 0.42)",
    dot: "hsl(330 85% 68%)",
    edge: "hsl(330 85% 78% / 0.7)",
  },
  {
    fill: "hsl(150 70% 52% / 0.8)",
    topic: "hsl(150 65% 46% / 0.4)",
    dot: "hsl(150 70% 56%)",
    edge: "hsl(150 70% 66% / 0.7)",
  },
  {
    fill: "hsl(38 92% 60% / 0.82)",
    topic: "hsl(38 88% 54% / 0.42)",
    dot: "hsl(38 92% 62%)",
    edge: "hsl(38 92% 72% / 0.7)",
  },
  {
    fill: "hsl(280 80% 70% / 0.82)",
    topic: "hsl(280 74% 64% / 0.42)",
    dot: "hsl(280 80% 72%)",
    edge: "hsl(280 80% 80% / 0.7)",
  },
  {
    fill: "hsl(8 88% 66% / 0.82)",
    topic: "hsl(8 82% 60% / 0.42)",
    dot: "hsl(8 88% 66%)",
    edge: "hsl(8 88% 76% / 0.7)",
  },
  {
    fill: "hsl(95 60% 56% / 0.8)",
    topic: "hsl(95 55% 50% / 0.4)",
    dot: "hsl(95 60% 58%)",
    edge: "hsl(95 60% 66% / 0.7)",
  },
];
