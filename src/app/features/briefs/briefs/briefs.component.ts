import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  signal,
} from "@angular/core";
import type { BriefRun, BriefSchedule } from "../../../core/models";
import { ToastService } from "../../../services/toast.service";
import { BriefsStore } from "../briefs.store";

/** Weekday labels indexed by the backend convention: 0 = Monday … 6 = Sunday. */
const WEEKDAYS = [
  "Monday",
  "Tuesday",
  "Wednesday",
  "Thursday",
  "Friday",
  "Saturday",
  "Sunday",
] as const;

/**
 * Brain v2 L5 — SCHEDULED BRIEFS: a collapsible Brain-page section (mirrors the
 * memory section) with
 *  1. the PENDING proposed-brief cards (propose-accept: the 60s backend runner
 *     stages a synthesized brief; the user Accepts → vault export, or
 *     Dismisses → the staged row is deleted), and
 *  2. the schedule CRUD list (label / daily-or-weekday / local time / lookback
 *     window / optional focus hint / enable toggle / delete) plus a small
 *     create form.
 *
 * Signals-first + OnPush; all state lives in {@link BriefsStore} (which owns
 * the one `EVENT_BRIEF_PROPOSED` subscription). The brief markdown shown in a
 * card was synthesized backend-side from VISIBLE-ONLY content (the runner
 * reads with the empty unlock set), so no lock gating applies here.
 */
@Component({
  selector: "app-briefs",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./briefs.component.html",
  styleUrl: "./briefs.component.scss",
})
export class BriefsComponent {
  protected readonly store = inject(BriefsStore);
  private readonly toast = inject(ToastService);

  /** The user's manual collapse/expand toggle (click on `.br-toggle`). */
  readonly open = signal(false);

  /**
   * Collapsed by default UNLESS a proposed brief is waiting: the pending-run
   * card peeks through even while `open()` is false (see template), so the
   * header chrome (aria-expanded + chevron rotation) must reflect that same
   * condition — otherwise the header looks collapsed while content renders
   * underneath it.
   */
  readonly isExpanded = computed(() => this.open() || this.pendingCount() > 0);

  // ── create form (signals-backed native inputs — no FormsModule) ─────────
  readonly formLabel = signal("");
  /** -1 = daily; 0..6 = the ISO weekday (Monday-first). */
  readonly formDay = signal(-1);
  /** "HH:MM" from the native time input. */
  readonly formTime = signal("08:30");
  readonly formScopeDays = signal(7);
  readonly formHint = signal("");
  readonly creating = signal(false);

  /** The run/schedule id with an action in flight (disables just that row). */
  readonly busyId = signal<string | null>(null);

  readonly canCreate = computed(() => {
    const time = this.formTime();
    return (
      !this.creating() &&
      this.formLabel().trim().length > 0 &&
      /^\d{2}:\d{2}$/.test(time)
    );
  });

  readonly pendingCount = computed(() => this.store.pending().length);

  constructor() {
    this.store.init();
  }

  // ── template helpers ─────────────────────────────────────────────────────

  protected readonly weekdays = WEEKDAYS;

  /** "Daily at 08:30 · last 7 days" / "Mondays at 09:00 · last 14 days". */
  scheduleLine(s: BriefSchedule): string {
    const hh = String(s.hourLocal).padStart(2, "0");
    const mm = String(s.minuteLocal).padStart(2, "0");
    const when =
      s.dayOfWeek === null
        ? `Daily at ${hh}:${mm}`
        : `${WEEKDAYS[s.dayOfWeek] ?? "?"}s at ${hh}:${mm}`;
    return `${when} · last ${s.scopeDays} days`;
  }

  /** The proposing schedule's label for a run card (falls back to "Brief"). */
  runLabel(run: BriefRun): string {
    return (
      this.store.schedules().find((s) => s.id === run.scheduleId)?.label ??
      "Brief"
    );
  }

  /** The date part of a run's proposal timestamp. */
  runDate(run: BriefRun): string {
    return run.proposedAt.split("T")[0] ?? run.proposedAt;
  }

  /** A short plain-text preview of the proposed markdown for the card body. */
  runPreview(run: BriefRun): string {
    const text = run.noteMd.replace(/[#*>`[\]]/g, "").trim();
    return text.length > 400 ? `${text.slice(0, 400)}…` : text;
  }

  onLabelInput(event: Event): void {
    this.formLabel.set((event.target as HTMLInputElement).value);
  }
  onDayChange(event: Event): void {
    this.formDay.set(Number((event.target as HTMLSelectElement).value));
  }
  onTimeInput(event: Event): void {
    this.formTime.set((event.target as HTMLInputElement).value);
  }
  onScopeInput(event: Event): void {
    const n = Number((event.target as HTMLInputElement).value);
    this.formScopeDays.set(Number.isFinite(n) && n >= 1 ? Math.min(n, 90) : 7);
  }
  onHintInput(event: Event): void {
    this.formHint.set((event.target as HTMLInputElement).value);
  }

  // ── actions ──────────────────────────────────────────────────────────────

  async create(): Promise<void> {
    if (!this.canCreate()) return;
    const [hh, mm] = this.formTime().split(":").map(Number);
    this.creating.set(true);
    try {
      await this.store.create({
        label: this.formLabel().trim(),
        dayOfWeek: this.formDay() < 0 ? null : this.formDay(),
        hourLocal: hh ?? 8,
        minuteLocal: mm ?? 0,
        scopeDays: this.formScopeDays(),
        promptHint: this.formHint().trim() || undefined,
      });
      this.formLabel.set("");
      this.formHint.set("");
      this.toast.info("Brief scheduled.");
    } catch (e) {
      this.toast.danger(String(e));
    } finally {
      this.creating.set(false);
    }
  }

  async toggleEnabled(s: BriefSchedule): Promise<void> {
    this.busyId.set(s.id);
    try {
      await this.store.update({ ...s, enabled: !s.enabled });
    } catch (e) {
      this.toast.danger(String(e));
    } finally {
      this.busyId.set(null);
    }
  }

  async remove(s: BriefSchedule): Promise<void> {
    this.busyId.set(s.id);
    try {
      await this.store.remove(s.id);
    } catch (e) {
      this.toast.danger(String(e));
    } finally {
      this.busyId.set(null);
    }
  }

  async accept(run: BriefRun): Promise<void> {
    this.busyId.set(run.id);
    try {
      await this.store.accept(run.id);
      this.toast.info("Brief saved to your vault (Briefs/).");
    } catch (e) {
      this.toast.danger(String(e));
    } finally {
      this.busyId.set(null);
    }
  }

  async dismiss(run: BriefRun): Promise<void> {
    this.busyId.set(run.id);
    try {
      await this.store.dismiss(run.id);
    } catch (e) {
      this.toast.danger(String(e));
    } finally {
      this.busyId.set(null);
    }
  }
}
