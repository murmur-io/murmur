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
import type {
  MeetingTimeline as MeetingTimelineData,
  SpeakerSuggestion,
} from "../../../core/models";

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
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./meeting-timeline.component.html",
  styleUrl: "./meeting-timeline.component.scss",
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
  /**
   * Speaker voiceprints (opt-in) — one suggested person name per diarized
   * `others-{n}` lane, from cross-meeting re-identification. Presentational only:
   * the chip's accept emits `renameSpeaker` (which the parent runs + which enrolls
   * the cluster). Empty when the opt-in is off, nothing matched, or the meeting is
   * locked. A suggestion is a best-effort guess, NOT a certain identity.
   */
  readonly suggestions = input<SpeakerSuggestion[]>([]);

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

  /**
   * Lookup: diarized cluster label (`others-{n}`) → its suggested person name.
   * Drives the "Looks like [[Anna]]?" chip on the matching lane. Only clusters
   * still awaiting a name appear here (the backend suggester skips already-labeled
   * ones), so the chip never covers a lane the user already named.
   */
  readonly suggestionByLabel = computed(() => {
    const map = new Map<string, string>();
    for (const s of this.suggestions()) {
      const name = s.suggestedLabel?.trim();
      if (name) {
        map.set(s.speaker, name);
      }
    }
    return map;
  });

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

  /**
   * Accept a voiceprint suggestion for a lane: emit the same `renameSpeaker` the
   * manual rename does (`others-{n}` → the suggested name). The parent runs the
   * IPC rename — which ENROLLS the cluster's voiceprint under that name — and
   * folds the fresh timeline back in, so the chip disappears with the relabel.
   */
  acceptSuggestion(oldLabel: string): void {
    const newLabel = this.suggestionByLabel().get(oldLabel)?.trim();
    if (!newLabel || newLabel === oldLabel) {
      return;
    }
    this.renameSpeaker.emit({ oldLabel, newLabel });
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
    let next: number;
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
