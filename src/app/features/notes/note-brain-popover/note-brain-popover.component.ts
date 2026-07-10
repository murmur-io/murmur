import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
  viewChild,
} from "@angular/core";
import { Router } from "@angular/router";
import { IpcService } from "../../../core/ipc.service";
import type {
  NoteAssistAction,
  NoteAssistResult,
  NoteCitation,
} from "../../../core/models";
import { RepositionOnScrollDirective } from "./reposition-on-scroll.directive";

/** The live text selection the popover acts on, plus its viewport anchor rect. */
export interface PopoverSelection {
  /** The selected body text. */
  text: string;
  /** The selection's start offset in the body (for a precise, unambiguous apply). */
  start: number;
  /** The selection's end offset in the body. */
  end: number;
  /** Up to ~500 chars of context before the selection (for the model). */
  before: string;
  /** Up to ~500 chars of context after the selection (for the model). */
  after: string;
  /** The selection's bounding rect in viewport coordinates (for positioning). */
  rect: { top: number; left: number; right: number; bottom: number };
}

/** One accepted assistant edit, applied by the editor into the textarea. */
export interface AcceptedEdit {
  action: NoteAssistAction;
  /** refine/shorten: replacement for the selection. enhance: additive passage. */
  suggestion: string;
}

/** One step in the animated progress tracker (mirrors the Ask trace-chip language). */
interface FlowStep {
  /** Stable id for `@for` tracking (never key on $index). */
  id: number;
  label: string;
  state: "pending" | "running" | "done";
}

/** Which assistant actions are enabled (from settings — all default ON). */
export interface AssistToggles {
  refine: boolean;
  shorten: boolean;
  enhance: boolean;
}

/** The label shown BEFORE the first result lands, keyed off the pre-fetched posture. */
type PhaseView = "actions" | "running" | "result" | "error";

const POPOVER_WIDTH = 340;
const POPOVER_GAP = 10;

/**
 * The selection Brain-assistant popover (FP3). Floats ABOVE a non-empty body
 * selection and offers Refine / Shorten / Enhance context. Each action expands
 * into an animated step tracker, then lands a reviewable DIFF (original vs
 * suggestion) with Accept / Discard / Retry.
 *
 * The single `noteAssistantAction` await drives the flow: the leading steps
 * animate optimistically while awaiting, then the popover lands on the REAL
 * result (the `Found N related` count is filled from `result.citations.length`,
 * never fabricated). A per-action `activeRequestId` guard drops a late reply for
 * a superseded selection/action (trap #4).
 *
 * OPAQUE overlay (T3): `--surface-overlay`, `--border-strong`, `--shadow-lg`,
 * `backdrop-filter:none`. Positioned via `afterNextRender({injector})` — no
 * `setTimeout`/`rAF`. Dismiss is owned by the host (outside-click / Esc /
 * selection-collapse); the popover only emits `dismiss` on its own Close/Discard.
 */
@Component({
  selector: "app-note-brain-popover",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RepositionOnScrollDirective],
  templateUrl: "./note-brain-popover.component.html",
  styleUrl: "./note-brain-popover.component.scss",
})
export class NoteBrainPopoverComponent {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);
  private readonly destroyRef = inject(DestroyRef);
  private readonly router = inject(Router);

  /** The note being edited (for the assistant request). */
  readonly noteId = input.required<string>();
  /** The live selection + anchor rect. A new object re-positions + resets the flow. */
  readonly selection = input.required<PopoverSelection>();
  /** Which actions are enabled (settings). Disabled actions are hidden. */
  readonly toggles = input<AssistToggles>({
    refine: true,
    shorten: true,
    enhance: true,
  });

  /** Accept: the editor applies the edit into the textarea + triggers autosave. */
  readonly accepted = output<AcceptedEdit>();
  /** The popover asked to close (Close / after Accept / Discard). */
  readonly dismiss = output<void>();

  /** Which phase the popover UI is in. */
  readonly phase = signal<PhaseView>("actions");
  /** The action currently running / shown (null in the picker phase). */
  readonly activeAction = signal<NoteAssistAction | null>(null);
  /** The animated step tracker. */
  readonly steps = signal<FlowStep[]>([]);
  /** The landed result (null until it arrives). */
  readonly result = signal<NoteAssistResult | null>(null);
  /** Inline error message (with a Retry affordance). */
  readonly errorMsg = signal<string | null>(null);

  /** The popover element, positioned after render. */
  private readonly popoverEl =
    viewChild<ElementRef<HTMLDivElement>>("popover");

  /**
   * Monotonic request token — a late `noteAssistantAction` reply for a
   * superseded selection/action is dropped by comparing against this at
   * resolution time (stale-result guard, trap #4).
   */
  private requestSeq = 0;
  /** The step-animation timers to clear on teardown / supersede (no leak). */
  private stepTimers: ReturnType<typeof setTimeout>[] = [];
  /** Monotonic id source for stable step `@for` keys. */
  private nextStepId = 1;

  /** The original selected text — the diff's "before" side. */
  readonly original = computed(() => this.selection().text);

  /** Whether at least one action is enabled (else the picker is empty → dismiss). */
  readonly anyActionEnabled = computed(() => {
    const t = this.toggles();
    return t.refine || t.shorten || t.enhance;
  });

  /** The mode chip label — the resolved model once a result lands, else neutral. */
  readonly modeLabel = computed(() => {
    const r = this.result();
    return r ? `via ${r.modelLabel}` : "Brain";
  });

  /** Whether the current result is the additive-enhance shape (insert, not replace). */
  readonly isEnhanceResult = computed(
    () => this.result()?.action === "enhance",
  );

  constructor() {
    // Re-position + reset the flow whenever a NEW selection arrives. Positioning
    // is DOM-after-render work (afterNextRender), the reset is plain signal
    // writes orchestrated off the input — a legitimate signal-writing effect (T1).
    effect(() => {
      this.selection(); // track
      // A fresh selection supersedes any in-flight action and returns to the picker.
      this.requestSeq++;
      this.clearStepTimers();
      this.phase.set("actions");
      this.activeAction.set(null);
      this.steps.set([]);
      this.result.set(null);
      this.errorMsg.set(null);
      this.reposition();
    });
    // Also re-position after the phase changes (the popover grows/shrinks).
    effect(() => {
      this.phase();
      this.reposition();
    });
    this.destroyRef.onDestroy(() => this.clearStepTimers());
  }

  /** Human labels for the action buttons + the diff header. */
  protected actionLabel(action: NoteAssistAction): string {
    switch (action) {
      case "refine":
        return "Refine";
      case "shorten":
        return "Shorten";
      case "enhance":
        return "Enhance context";
    }
  }

  /**
   * Run one assistant action. Animates the leading steps optimistically while the
   * single `noteAssistantAction` await is in flight, then lands the REAL result
   * (filling the enhance `Found N related` count from `citations.length`). A
   * fresh request bumps `requestSeq`; a late reply for a superseded request/action
   * is dropped.
   */
  async run(action: NoteAssistAction): Promise<void> {
    const seq = ++this.requestSeq;
    this.clearStepTimers();
    this.activeAction.set(action);
    this.phase.set("running");
    this.result.set(null);
    this.errorMsg.set(null);

    const labels =
      action === "enhance"
        ? ["Reading selection", "Searching your brain", "Drafting"]
        : ["Reading selection", "Drafting"];
    this.steps.set(
      labels.map((label, i) => ({
        id: this.nextStepId++,
        label,
        state: i === 0 ? "running" : "pending",
      })),
    );
    this.animateSteps(seq, labels.length);

    const sel = this.selection();
    try {
      const res = await this.ipc.noteAssistantAction({
        noteId: this.noteId(),
        action,
        selection: sel.text,
        before: sel.before,
        after: sel.after,
      });
      if (seq !== this.requestSeq) {
        return; // superseded — a newer selection/action took over.
      }
      // Land the real result: mark every step done, then, for enhance, append the
      // real "Found N related" step from the actual citation count.
      this.clearStepTimers();
      this.steps.update((steps) => {
        const done = steps.map((s) => ({ ...s, state: "done" as const }));
        if (action === "enhance") {
          done.push({
            id: this.nextStepId++,
            label: `Found ${res.citations.length} related`,
            state: "done",
          });
        }
        return done;
      });
      this.result.set(res);
      this.phase.set("result");
    } catch (e) {
      if (seq !== this.requestSeq) {
        return;
      }
      this.clearStepTimers();
      this.errorMsg.set(String(e));
      this.phase.set("error");
    }
  }

  /**
   * Optimistically walk the step tracker forward while the IPC is in flight:
   * every ~520ms mark the running step done and start the next, stopping one
   * short of the last so it stays "running" until the real result lands. Tracked
   * timers cleared on supersede/teardown (no bare component setTimeout leak; the
   * handles are owned + cleared here, the sanctioned tracked pattern).
   */
  private animateSteps(seq: number, count: number): void {
    const advance = (index: number): void => {
      if (seq !== this.requestSeq || index >= count - 1) {
        return;
      }
      const handle = setTimeout(() => {
        if (seq !== this.requestSeq) {
          return;
        }
        this.steps.update((steps) =>
          steps.map((s, i) => {
            if (i < index + 1) {
              return { ...s, state: "done" };
            }
            if (i === index + 1) {
              return { ...s, state: "running" };
            }
            return s;
          }),
        );
        advance(index + 1);
      }, 520);
      this.stepTimers.push(handle);
    };
    advance(0);
  }

  /** Accept the suggestion — the editor applies it + autosaves; then dismiss. */
  accept(): void {
    const res = this.result();
    if (!res) {
      return;
    }
    this.accepted.emit({ action: res.action, suggestion: res.suggestion });
    this.dismiss.emit();
  }

  /** Discard the result and dismiss the popover (selection is untouched). */
  discard(): void {
    this.dismiss.emit();
  }

  /** Retry the current action (fresh request id). */
  retry(): void {
    const action = this.activeAction();
    if (action) {
      void this.run(action);
    }
  }

  /** Open a citation's source note/meeting; dismiss the popover first. */
  openCitation(cite: NoteCitation): void {
    this.dismiss.emit();
    const path = cite.kind === "note" ? "/notes" : "/meeting";
    void this.router.navigate([path, cite.id]);
  }

  /** Reposition the popover ABOVE the selection rect (flips below if no room). */
  reposition(): void {
    afterNextRender(
      () => {
        const el = this.popoverEl()?.nativeElement;
        if (!el) {
          return;
        }
        const rect = this.selection().rect;
        const width = el.offsetWidth || POPOVER_WIDTH;
        const height = el.offsetHeight;
        const vw = window.innerWidth;
        const vh = window.innerHeight;

        // Horizontally center on the selection, clamped to the viewport.
        const anchorCenter = (rect.left + rect.right) / 2;
        let left = anchorCenter - width / 2;
        left = Math.max(POPOVER_GAP, Math.min(left, vw - width - POPOVER_GAP));

        // Prefer ABOVE; flip below when there isn't room above the selection.
        let top = rect.top - height - POPOVER_GAP;
        if (top < POPOVER_GAP) {
          top = Math.min(rect.bottom + POPOVER_GAP, vh - height - POPOVER_GAP);
        }

        el.style.left = `${Math.round(left)}px`;
        el.style.top = `${Math.round(Math.max(POPOVER_GAP, top))}px`;
      },
      { injector: this.injector },
    );
  }

  /** Clear + forget every pending step-animation timer. */
  private clearStepTimers(): void {
    for (const handle of this.stepTimers) {
      clearTimeout(handle);
    }
    this.stepTimers = [];
  }
}
