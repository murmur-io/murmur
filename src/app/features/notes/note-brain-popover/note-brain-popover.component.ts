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
import { TabsService } from "../../../core/tabs.service";
import type {
  NoteAssistAction,
  NoteAssistResult,
  NoteCitation,
} from "../../../core/models";
import { TimerService } from "../../../services/timer.service";
import {
  NOTE_ASSIST_CATALOG,
  NOTE_ASSIST_GROUPS,
  type NoteAssistCatalogEntry,
  type NoteAssistGroup,
  noteAssistEntry,
} from "./note-assist-catalog";
import { RepositionOnScrollDirective } from "./reposition-on-scroll.directive";
import { TeleportToBodyDirective } from "../../../design-system/teleport-to-body.directive";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";

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

/**
 * One accepted assistant outcome, applied by the editor. Discriminated by `kind`
 * so the editor branches without re-deriving intent from the action:
 * - `replace`    — replace the selection with `suggestion`.
 * - `insert`     — append `suggestion` after the selection (additive; also "Insert as note").
 * - `copy`       — copy `text` to the clipboard (draft follow-up / an info answer).
 * - `spinoff`    — create a NEW note from `title` + `body` and open it.
 * - `insertLink` — insert a `[[title]]` wikilink after the selection (Fix 3: the
 *                  "Insert link" action on a `find_related` citation row, reusing
 *                  the SAME wikilink text the link-picker/toolbar op builds).
 */
export type AcceptedEdit =
  | { kind: "replace"; suggestion: string }
  | { kind: "insert"; suggestion: string }
  | { kind: "copy"; text: string }
  | { kind: "spinoff"; title: string; body: string }
  | { kind: "insertLink"; title: string };

/** One step in the animated progress tracker (mirrors the Ask trace-chip language). */
interface FlowStep {
  /** Stable id for `@for` tracking (never key on $index). */
  id: number;
  label: string;
  state: "pending" | "running" | "done";
}

/** The high-level view the popover is showing. */
type PhaseView = "menu" | "submenu" | "running" | "result" | "error";

const POPOVER_WIDTH = 340;
const POPOVER_GAP = 10;

/** Catalog action ids that read the brain → show a "Searching your brain" step. */
const RETRIEVAL_ACTIONS = new Set<NoteAssistAction>([
  "enhance",
  "find_related",
  "link_entities",
  "fact_check",
  "ask",
]);

/**
 * The selection Brain-assistant popover — a ClickUp-style command menu. Floats
 * ABOVE a non-empty body selection. A command input filters the action list live
 * and, on Enter with text, runs a free-text `custom` instruction. The default
 * (compact) view shows the input + 5 quick actions + "More actions"; expanding
 * reveals every action grouped under quiet section labels; the variant-heavy
 * actions (tone / translate) open a second-level submenu with a Back row.
 *
 * Running an action animates an optimistic step tracker while the single
 * `noteAssistantAction` await is in flight (retrieval actions add a "Searching
 * your brain" step), then lands the REAL result. The RESULT phase branches on
 * `result().shape` (NOT the action) into replace / insert / info / artifact
 * renderings. A per-request `requestSeq` guard drops a late reply for a
 * superseded selection/action (trap #4).
 *
 * OPAQUE overlay (T3): `--surface-overlay`, `--border-strong`, `--shadow-lg`,
 * `backdrop-filter:none`. Positioned via `afterNextRender({injector})` — no
 * `setTimeout`/`rAF` (the step tick uses the root TimerService, rule §5).
 * Dismiss is owned by the host (outside-click / Esc / selection-collapse); the
 * popover only emits `dismiss` on its own Close / Discard / after-Accept.
 */
@Component({
  selector: "app-note-brain-popover",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RepositionOnScrollDirective, TeleportToBodyDirective],
  templateUrl: "./note-brain-popover.component.html",
  styleUrl: "./note-brain-popover.component.scss",
})
export class NoteBrainPopoverComponent {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);
  private readonly destroyRef = inject(DestroyRef);
  private readonly router = inject(Router);
  private readonly tabsService = inject(TabsService);
  private readonly errorCopy = inject(ErrorCopyService);
  /** Root-owned timer service — the sanctioned home for the step-animation tick (rule §5). */
  private readonly timers = inject(TimerService);

  /** The note being edited (for the assistant request). */
  readonly noteId = input.required<string>();
  /** The live selection + anchor rect. A new object re-positions + resets the flow. */
  readonly selection = input.required<PopoverSelection>();
  /**
   * Which action ids are ENABLED (from Settings). A row not in this set is hidden
   * from the menu; `custom` (the command-input escape hatch) is ALWAYS available
   * regardless. The backend is still the real gate (a disabled action refuses
   * `Unavailable`). Defaults to every catalog id enabled.
   */
  readonly enabledActions = input<ReadonlySet<string>>(
    new Set(NOTE_ASSIST_CATALOG.map((a) => a.id)),
  );

  /** Accept: the editor applies the outcome (replace/insert/copy/spinoff). */
  readonly accepted = output<AcceptedEdit>();
  /** The popover asked to close (Close / after Accept / Discard). */
  readonly dismiss = output<void>();

  /** Which phase the popover UI is in. */
  readonly phase = signal<PhaseView>("menu");
  /** Compact (5 quick + More) vs expanded (all, grouped). */
  readonly expanded = signal(false);
  /** The live command-input filter text. */
  readonly filter = signal("");
  /** The open submenu's parent action id (tone / translate), else null. */
  readonly submenuId = signal<string | null>(null);
  /** The action currently running / shown, and its variant (for the run + result header). */
  readonly activeAction = signal<NoteAssistAction | null>(null);
  readonly activeVariant = signal<string | null>(null);
  /** The animated step tracker. */
  readonly steps = signal<FlowStep[]>([]);
  /** The landed result (null until it arrives). */
  readonly result = signal<NoteAssistResult | null>(null);
  /** Inline error message (with a Retry affordance). */
  readonly errorMsg = signal<string | null>(null);

  /** The popover element, positioned after render. */
  private readonly popoverEl =
    viewChild<ElementRef<HTMLDivElement>>("popover");
  /** The command input, focused after the menu renders. */
  private readonly cmdInput =
    viewChild<ElementRef<HTMLInputElement>>("cmdInput");

  /** Static catalog references for the template. */
  protected readonly groups = NOTE_ASSIST_GROUPS;

  /**
   * Monotonic request token — a late `noteAssistantAction` reply for a
   * superseded selection/action is dropped by comparing against this at
   * resolution time (stale-result guard, trap #4).
   */
  private requestSeq = 0;
  /** The step-animation timer HANDLE IDS (from TimerService) to clear on teardown / supersede. */
  private stepTimers: number[] = [];
  /** Monotonic id source for stable step `@for` keys. */
  private nextStepId = 1;

  /** The original selected text — the diff's "before" side. */
  readonly original = computed(() => this.selection().text);

  /** Only the enabled catalog entries (a disabled action is hidden from the menu). */
  private readonly enabledCatalog = computed<readonly NoteAssistCatalogEntry[]>(
    () => {
      const on = this.enabledActions();
      return NOTE_ASSIST_CATALOG.filter((a) => on.has(a.id));
    },
  );

  /** The compact-default quick set (enabled ∩ quick), in catalog order. */
  readonly quickActions = computed(() =>
    this.enabledCatalog().filter((a) => a.quick),
  );

  /**
   * The rows shown in the current menu view, keyed by the trimmed filter:
   * - filter present → fuzzy matches (label + desc), across ALL enabled actions.
   * - compact → the quick set only.
   * - expanded → all enabled actions (rendered grouped by the template).
   */
  readonly filterHits = computed<readonly NoteAssistCatalogEntry[]>(() => {
    const f = this.filter().trim().toLowerCase();
    if (!f) {
      return [];
    }
    return this.enabledCatalog().filter((a) =>
      `${a.label} ${a.desc}`.toLowerCase().includes(f),
    );
  });

  /** True when the filter is non-empty (drives the custom row + filtered list). */
  readonly filtering = computed(() => this.filter().trim().length > 0);

  /** The enabled actions for one group (for the expanded grouped view). */
  groupActions(group: NoteAssistGroup): readonly NoteAssistCatalogEntry[] {
    return this.enabledCatalog().filter((a) => a.group === group);
  }

  /** The open submenu's parent entry (tone / translate), or null. */
  readonly submenu = computed<NoteAssistCatalogEntry | null>(() => {
    const id = this.submenuId();
    return id ? (noteAssistEntry(id) ?? null) : null;
  });

  /** The header caption for the result / submenu / running phases. */
  readonly headerCaption = computed<string | null>(() => {
    const phase = this.phase();
    if (phase === "submenu") {
      return this.submenu()?.label ?? null;
    }
    if (phase === "running" || phase === "result" || phase === "error") {
      const action = this.activeAction();
      if (!action) {
        return null;
      }
      const base = action === "custom" ? "Custom" : (noteAssistEntry(action)?.label ?? action);
      const variant = this.activeVariant();
      return variant ? `${base} · ${variant}` : base;
    }
    return null;
  });

  constructor() {
    // Re-position + reset the flow whenever a NEW selection arrives. Positioning
    // is DOM-after-render work (afterNextRender), the reset is plain signal
    // writes orchestrated off the input — a legitimate signal-writing effect (T1).
    effect(() => {
      this.selection(); // track
      // A fresh selection supersedes any in-flight action and returns to the menu.
      this.requestSeq++;
      this.clearStepTimers();
      this.phase.set("menu");
      this.expanded.set(false);
      this.filter.set("");
      this.submenuId.set(null);
      this.activeAction.set(null);
      this.activeVariant.set(null);
      this.steps.set([]);
      this.result.set(null);
      this.errorMsg.set(null);
      this.reposition();
      this.focusInput();
    });
    // Also re-position after the phase / density changes (the popover grows/shrinks).
    effect(() => {
      this.phase();
      this.expanded();
      this.filtering();
      this.reposition();
    });
    this.destroyRef.onDestroy(() => this.clearStepTimers());
  }

  // ── Menu navigation ──────────────────────────────────────────────────────

  /** The command input changed — track the filter (drives the live-filtered list). */
  onFilterInput(event: Event): void {
    this.filter.set((event.target as HTMLInputElement).value);
  }

  /** Enter with text in the command input → run it as a free-text custom instruction. */
  onFilterKeydown(event: KeyboardEvent): void {
    if (event.key === "Enter" && this.filter().trim().length > 0) {
      event.preventDefault();
      void this.runCustom(this.filter().trim());
    }
  }

  /** Expand the compact menu to the full grouped list. */
  showMore(): void {
    this.expanded.set(true);
    this.reposition();
  }

  /** A row was chosen: open its submenu (variant actions) or run it. */
  choose(entry: NoteAssistCatalogEntry): void {
    if (entry.sub) {
      this.submenuId.set(entry.id);
      this.phase.set("submenu");
      return;
    }
    void this.run(entry.id, null);
  }

  /** Back out of a submenu to the menu. */
  back(): void {
    this.submenuId.set(null);
    this.phase.set("menu");
  }

  /** Run a submenu variant (a tone / a target language). */
  runVariant(entry: NoteAssistCatalogEntry, variant: string): void {
    void this.run(entry.id, variant);
  }

  // ── Running an action ────────────────────────────────────────────────────

  /** Run a free-text custom instruction from the command input. */
  private async runCustom(instruction: string): Promise<void> {
    await this.run("custom", null, instruction);
  }

  /**
   * Run one assistant action. Animates the leading steps optimistically while the
   * single `noteAssistantAction` await is in flight, then lands the REAL result
   * (filling a `Found N related` step from `citations.length` for retrieval
   * actions). A fresh request bumps `requestSeq`; a late reply for a superseded
   * request/action is dropped.
   */
  async run(
    action: NoteAssistAction,
    variant: string | null,
    instruction?: string,
  ): Promise<void> {
    const seq = ++this.requestSeq;
    this.clearStepTimers();
    this.activeAction.set(action);
    this.activeVariant.set(variant);
    this.phase.set("running");
    this.result.set(null);
    this.errorMsg.set(null);

    const labels = RETRIEVAL_ACTIONS.has(action)
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
        variant: variant ?? undefined,
        instruction: instruction ?? undefined,
      });
      if (seq !== this.requestSeq) {
        return; // superseded — a newer selection/action took over.
      }
      // Land the real result: mark every step done, then, for retrieval actions
      // that returned citations, append the real "Found N related" step.
      this.clearStepTimers();
      this.steps.update((steps) => {
        const done = steps.map((s) => ({ ...s, state: "done" as const }));
        if (RETRIEVAL_ACTIONS.has(action) && res.citations.length > 0) {
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
      this.errorMsg.set(this.errorCopy.humanize(e));
      this.phase.set("error");
    }
  }

  /**
   * Optimistically walk the step tracker forward while the IPC is in flight:
   * every ~520ms mark the running step done and start the next, stopping one
   * short of the last so it stays "running" until the real result lands. The tick
   * is scheduled through the root {@link TimerService} (rule §5 — no bare component
   * setTimeout); the returned handle ids are tracked + cleared on supersede/teardown.
   */
  private animateSteps(seq: number, count: number): void {
    const advance = (index: number): void => {
      if (seq !== this.requestSeq || index >= count - 1) {
        return;
      }
      const handle = this.timers.after(520, () => {
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
      });
      this.stepTimers.push(handle);
    };
    advance(0);
  }

  // ── Result actions ───────────────────────────────────────────────────────

  /** replace: apply the suggestion over the selection, then dismiss. */
  acceptReplace(): void {
    const res = this.result();
    if (!res) {
      return;
    }
    this.accepted.emit({ kind: "replace", suggestion: res.suggestion });
    this.dismiss.emit();
  }

  /** insert / "Insert as note": append the suggestion after the selection, then dismiss. */
  acceptInsert(): void {
    const res = this.result();
    if (!res) {
      return;
    }
    this.accepted.emit({ kind: "insert", suggestion: res.suggestion });
    this.dismiss.emit();
  }

  /** Copy the answer/draft body to the clipboard (draft follow-up / an info answer). */
  copyResult(): void {
    const res = this.result();
    if (!res) {
      return;
    }
    this.accepted.emit({ kind: "copy", text: res.suggestion });
    this.dismiss.emit();
  }

  /** Spin-off note: create a note from the artifact title + body and open it. */
  createNote(): void {
    const res = this.result();
    if (!res) {
      return;
    }
    this.accepted.emit({
      kind: "spinoff",
      title: (res.title ?? "").trim() || "Untitled",
      body: res.suggestion,
    });
    this.dismiss.emit();
  }

  /** Discard the result and dismiss the popover (selection is untouched). */
  discard(): void {
    this.dismiss.emit();
  }

  /** Retry the current action (fresh request id, same action/variant). */
  retry(): void {
    const action = this.activeAction();
    if (action) {
      void this.run(action, this.activeVariant());
    }
  }

  /**
   * Open a citation's source note/meeting/org-item; dismiss the popover
   * first. A note/meeting/org-item opens as a TRACKED TAB (live-found bug,
   * 2026-07-12: this used to be a plain `router.navigate`, so a citation
   * click — for ANY kind, not just org — never registered with
   * {@link TabsService}, unlike opening the same note/meeting from its own
   * list row). `person`/`entity` still go to `/graph`, which isn't part of
   * the tab system.
   */
  openCitation(cite: NoteCitation): void {
    this.dismiss.emit();
    if (cite.kind === "org") {
      // NOT a local note/meeting id — the read-only org-item viewer tab.
      void this.tabsService.openOrgItem(cite.id, cite.title || "Shared note");
      return;
    }
    if (cite.kind === "meeting") {
      void this.tabsService.openMeeting(cite.id, cite.title || "Meeting");
      return;
    }
    if (cite.kind === "note") {
      void this.tabsService.openNote(cite.id, cite.title || "Note");
      return;
    }
    void this.router.navigate(["/graph", cite.id]);
  }

  /**
   * Insert a `[[Title]]` wikilink to this citation's source, then dismiss the
   * popover — the PRIMARY action of a `find_related` row (the row itself; its
   * accessible name is "Insert link to <title>"), with "Open" as the quiet
   * secondary (minimalist reshape 2026-07-16; reuses the SAME gated citation
   * data, only the interaction changed). The editor applies it exactly like an
   * `insert` outcome (appended after the selection), so this needs no new
   * textarea-splice logic.
   */
  insertLinkFor(cite: NoteCitation): void {
    this.accepted.emit({ kind: "insertLink", title: cite.title });
    this.dismiss.emit();
  }

  // ── Positioning + teardown ───────────────────────────────────────────────

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

  /** Focus the command input after the menu renders (menu phase only). */
  private focusInput(): void {
    afterNextRender(
      () => {
        if (this.phase() === "menu") {
          this.cmdInput()?.nativeElement.focus();
        }
      },
      { injector: this.injector },
    );
  }

  /** Clear + forget every pending step-animation timer (through the TimerService). */
  private clearStepTimers(): void {
    for (const handle of this.stepTimers) {
      this.timers.clear(handle);
    }
    this.stepTimers = [];
  }
}
