import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  effect,
  inject,
  input,
  signal,
  viewChild,
} from "@angular/core";
import { RecorderStore } from "../../../core/recorder.store";

interface LiveTranscriptRow {
  readonly id: string;
  readonly text: string;
  readonly speaker: "me" | "others";
  readonly speakerLabel: "Me" | "Others";
  readonly timeLabel: string;
  readonly final: boolean;
  readonly possibleQuestion: boolean;
}

function offsetLabel(offsetMs: number | undefined): string {
  if (offsetMs === undefined || !Number.isFinite(offsetMs)) return "Now";
  const seconds = Math.max(0, Math.floor(offsetMs / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

@Component({
  selector: "app-live-transcript-panel",
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { "[class.is-collapsed]": "collapsed()" },
  templateUrl: "./live-transcript-panel.component.html",
  styleUrl: "./live-transcript-panel.component.scss",
})
export class LiveTranscriptPanelComponent {
  readonly store = inject(RecorderStore);
  private readonly injector = inject(Injector);
  private readonly scroller = viewChild<ElementRef<HTMLElement>>("scroller");
  private readonly collapseButton =
    viewChild<ElementRef<HTMLButtonElement>>("collapseButton");
  private readonly railToggle =
    viewChild<ElementRef<HTMLButtonElement>>("railToggle");

  readonly captionsOff = input(false);
  readonly systemCaptureNote = input<string | null>(null);
  readonly collapsed = signal(false);
  readonly pinned = signal(true);
  readonly newCount = signal(0);
  readonly newQuestionCount = signal(0);
  readonly questionAnnouncement = signal("");
  private readonly windowAnchorId = signal<string | null>(null);

  private previousAllIds = new Set<string>();
  private announcedQuestions = new Set<string>();
  private previousEventRevision = this.store.liveTranscriptEventRevision();

  readonly rows = computed<LiveTranscriptRow[]>(() => {
    const all = this.store.liveTranscriptLines();
    const anchor = this.windowAnchorId();
    const anchorIndex = anchor
      ? all.findIndex((line) => line.lineId === anchor)
      : -1;
    const end = anchorIndex >= 0 ? anchorIndex + 1 : all.length;
    const start = Math.max(0, end - 160);
    return all
      .slice(start, end)
      .map((line) => ({
        id: line.lineId!,
        text: line.text,
        speaker: line.speaker!,
        speakerLabel: line.speaker === "me" ? "Me" : "Others",
        timeLabel: offsetLabel(line.offsetMs),
        final: line.final === true,
        possibleQuestion:
          line.final === true &&
          line.speaker === "others" &&
          line.possibleQuestion === true,
      }));
  });

  readonly hasBufferedOlder = computed(
    () => {
      const firstId = this.rows().at(0)?.id;
      return firstId
        ? this.store
            .liveTranscriptLines()
            .findIndex((line) => line.lineId === firstId) > 0
        : false;
    },
  );
  readonly canShowOlder = computed(
    () => this.hasBufferedOlder() || this.store.liveTranscriptHasEarlier(),
  );

  readonly statusLabel = computed(() => {
    if (this.store.liveTranscriptPaused()) return "Live transcript paused";
    if (this.captionsOff()) return "Captions off";
    if (this.systemCaptureNote()) return "Me only";
    if (this.store.liveOthersState() === "starting") return "Starting live captions…";
    if (this.store.liveOthersState() === "unavailable") return "Me only · Others unavailable";
    if (this.store.liveOthersState() === "degraded") return "Me · Others delayed";
    return "Me + Others";
  });

  readonly jumpLabel = computed(() => {
    const questions = this.newQuestionCount();
    if (!this.newCount() && !questions) return "Live transcript";
    return `${this.newCount()} new${questions ? ` · ${questions} question${questions === 1 ? "" : "s"}` : ""}`;
  });

  private readonly followRows = effect(() => {
    const rows = this.rows();
    const allLines = this.store.liveTranscriptLines();
    const eventRevision = this.store.liveTranscriptEventRevision();
    const liveEventArrived = eventRevision !== this.previousEventRevision;
    this.previousEventRevision = eventRevision;
    const allIds = new Set(
      allLines.flatMap((line) => (line.lineId ? [line.lineId] : [])),
    );
    if (rows.length === 0) {
      this.previousAllIds = allIds;
      this.announcedQuestions.clear();
      this.newCount.set(0);
      this.newQuestionCount.set(0);
      this.questionAnnouncement.set("");
      return;
    }

    if (!liveEventArrived) {
      this.previousAllIds = allIds;
      for (const line of allLines) {
        if (line.final && line.possibleQuestion && line.lineId) {
          this.announcedQuestions.add(line.lineId);
        }
      }
      if (this.pinned()) this.scrollToLatest();
      return;
    }

    const added = allLines.filter(
      (line) => !!line.lineId && !this.previousAllIds.has(line.lineId),
    );
    this.previousAllIds = allIds;
    const questions = allLines.filter(
      (line) =>
        !!line.lineId &&
        line.final === true &&
        line.speaker === "others" &&
        line.possibleQuestion === true &&
        !this.announcedQuestions.has(line.lineId),
    );
    for (const line of questions) this.announcedQuestions.add(line.lineId!);
    const lastQuestion = questions.at(-1);
    if (lastQuestion) {
      this.questionAnnouncement.set(
        `Possible question from Others: ${lastQuestion.text}`,
      );
    }

    if (this.pinned()) {
      this.scrollToLatest();
    } else if (added.length > 0 || questions.length > 0) {
      this.newCount.update((count) => count + added.length);
      this.newQuestionCount.update((count) => count + questions.length);
    }
  });

  toggle(): void {
    const willCollapse = !this.collapsed();
    this.collapsed.set(willCollapse);
    afterNextRender(
      () => {
        const target = willCollapse
          ? this.railToggle()?.nativeElement
          : this.collapseButton()?.nativeElement;
        target?.focus();
      },
      { injector: this.injector },
    );
    if (!willCollapse) this.scrollToLatest();
  }

  onScroll(event: Event): void {
    const element = event.currentTarget as HTMLElement;
    const atBottom =
      element.scrollHeight - element.scrollTop - element.clientHeight <= 48;
    if (atBottom && !this.pinned()) {
      // Reaching the bottom of a historical page does not mean reaching live.
      if (this.windowAnchorId() === null) this.jumpToLatest();
    } else if (!atBottom && this.pinned()) {
      this.windowAnchorId.set(this.rows().at(-1)?.id ?? null);
      this.pinned.set(false);
    }
  }

  jumpToLatest(): void {
    this.windowAnchorId.set(null);
    this.pinned.set(true);
    this.newCount.set(0);
    this.newQuestionCount.set(0);
    this.scrollToLatest();
  }

  async showOlder(): Promise<void> {
    if (!this.hasBufferedOlder()) {
      const added = await this.store.loadOlderLiveTranscript();
      if (added === 0) return;
    }
    const all = this.store.liveTranscriptLines();
    const firstId = this.rows().at(0)?.id;
    const currentStart = firstId
      ? all.findIndex((line) => line.lineId === firstId)
      : all.length;
    if (currentStart <= 0) return;
    const nextEnd = currentStart;
    const nextStart = Math.max(0, nextEnd - 160);
    const target = all.slice(nextStart, nextEnd);
    for (const line of target) {
      if (line.final && line.possibleQuestion && line.lineId) {
        this.announcedQuestions.add(line.lineId);
      }
    }
    this.windowAnchorId.set(target.at(-1)?.lineId ?? null);
    this.pinned.set(false);
  }

  private scrollToLatest(): void {
    afterNextRender(
      () => {
        const element = this.scroller()?.nativeElement;
        if (element && this.pinned()) element.scrollTop = element.scrollHeight;
      },
      { injector: this.injector },
    );
  }
}
