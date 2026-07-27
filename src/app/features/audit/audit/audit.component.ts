import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  signal,
} from "@angular/core";
import type {
  AuditExplanation,
  AuditFinding,
  AuditFindingKind,
} from "../../../core/models";
import { IpcService } from "../../../core/ipc.service";
import { MurSpinnerComponent } from "../../../design-system/spinner/spinner.component";
import { ToastService } from "../../../services/toast.service";
import { MarkdownComponent } from "../../../shared/markdown/markdown.component";
import { AuditStore } from "../audit.store";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";

/** Per-kind copy: the section heading, its one-line explanation, and the row chip. */
const KIND_META: Record<
  AuditFindingKind,
  { label: string; chip: string; explain: string }
> = {
  contradiction: {
    label: "Contradictions",
    chip: "Contradiction",
    explain: "Two places in your vault say conflicting things.",
  },
  stale: {
    label: "Stale notes",
    chip: "Stale",
    explain: "Content that newer meetings or notes have likely overtaken.",
  },
  broken_link: {
    label: "Broken links",
    chip: "Broken link",
    explain: "[[Wikilinks]] that point at a note that doesn't exist.",
  },
  unlinked_mention: {
    label: "Unlinked mentions",
    chip: "Unlinked mention",
    explain: "A known title mentioned in text without a [[link]].",
  },
  orphan: {
    label: "Orphans",
    chip: "Orphan",
    explain: "Notes nothing links to — disconnected from the rest of the vault.",
  },
};

/**
 * Epoch → "today" / "yesterday" / "N days ago" / a short date (the
 * people-page idiom). The backend timestamps are epoch numbers; values below
 * 10^12 are seconds, above are millis — normalize so either shape renders.
 */
function relativeDayLabel(epoch: number): string {
  const ms = epoch < 1_000_000_000_000 ? epoch * 1000 : epoch;
  const d = new Date(ms);
  if (isNaN(d.getTime())) {
    return "";
  }
  const now = new Date();
  const startOfDay = (x: Date): number =>
    new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const days = Math.round((startOfDay(now) - startOfDay(d)) / 86_400_000);
  if (days <= 0) {
    return "today";
  }
  if (days === 1) {
    return "yesterday";
  }
  if (days < 7) {
    return `${days} days ago`;
  }
  const opts: Intl.DateTimeFormatOptions =
    d.getFullYear() === now.getFullYear()
      ? { month: "short", day: "numeric" }
      : { month: "short", day: "numeric", year: "numeric" };
  return d.toLocaleDateString(undefined, opts);
}

/**
 * VAULT AUDIT — a collapsible Brain-page section (mirrors the scheduled-briefs
 * section): an "Audit now" trigger plus the propose-accept FINDINGS INBOX,
 * grouped by kind. Every finding is review-then-apply: Accept (only offered
 * when the backend staged an `acceptAction`) applies that action; Dismiss
 * discards. Neither is optimistic — the row leaves the inbox only after the
 * backend confirms ({@link AuditStore.resolve}); failures toast and the row
 * stays pending.
 *
 * Signals-first + OnPush; all state lives in {@link AuditStore} (root-provided,
 * so cached rows survive remounts — loading never hides them, §8). Evidence
 * snippets render through the shared `app-markdown` (sanitized, wikilink chips
 * clickable) — the same renderer chat/recipes/notes use.
 */
@Component({
  selector: "app-audit",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MarkdownComponent, MurSpinnerComponent],
  templateUrl: "./audit.component.html",
  styleUrl: "./audit.component.scss",
})
export class AuditComponent {
  protected readonly store = inject(AuditStore);
  private readonly ipc = inject(IpcService);
  private readonly toast = inject(ToastService);
  private readonly errorCopy = inject(ErrorCopyService);

  /** The user's manual collapse/expand toggle (auto-opens on "Audit now"). */
  readonly open = signal(false);

  /** The finding id with a resolve in flight (disables just that row's buttons). */
  readonly busyId = signal<string | null>(null);

  /** Finding ids with an explain in flight — at most ONE per row. */
  readonly explaining = signal<ReadonlySet<string>>(new Set());

  /** Resolved AI explanations by finding id (rendered inline, collapsible). */
  readonly explanations = signal<ReadonlyMap<string, AuditExplanation>>(
    new Map(),
  );

  /** Finding ids whose loaded explanation the user collapsed. */
  readonly explainCollapsed = signal<ReadonlySet<string>>(new Set());

  readonly listEmpty = computed(() => this.store.pendingCount() === 0);

  /**
   * The passive weekly-schedule chip: "Weekly: on · last run yesterday".
   * Read-only ON PURPOSE — the toggle lives in Settings (no navigation
   * coupling from the inbox). Null until the schedule loads.
   */
  readonly weeklyChip = computed<string | null>(() => {
    const s = this.store.schedule();
    if (!s) {
      return null;
    }
    const state = s.enabled ? "on" : "off";
    const last = s.lastRunAt ? relativeDayLabel(s.lastRunAt) : "";
    return last ? `Weekly: ${state} · last run ${last}` : `Weekly: ${state}`;
  });

  /** "N new findings · M pending" — shown once a manual run completed. */
  readonly summaryLine = computed(() => {
    const s = this.store.lastRun();
    if (!s) {
      return null;
    }
    const noun = s.findingsNew === 1 ? "new finding" : "new findings";
    return `${s.findingsNew} ${noun} · ${s.findingsTotalPending} pending`;
  });

  protected readonly kindMeta = KIND_META;

  constructor() {
    this.store.init();
  }

  async runNow(): Promise<void> {
    if (this.store.running()) {
      return;
    }
    this.open.set(true);
    try {
      await this.store.runNow();
    } catch (e) {
      this.toast.danger(this.errorCopy.humanize(e));
    }
  }

  async accept(f: AuditFinding): Promise<void> {
    if (!f.acceptAction) {
      return;
    }
    this.busyId.set(f.id);
    try {
      await this.store.resolve(f.id, "accept");
    } catch (e) {
      this.toast.danger(this.errorCopy.humanize(e));
    } finally {
      this.busyId.set(null);
    }
  }

  async dismiss(f: AuditFinding): Promise<void> {
    this.busyId.set(f.id);
    try {
      await this.store.resolve(f.id, "dismiss");
    } catch (e) {
      this.toast.danger(this.errorCopy.humanize(e));
    } finally {
      this.busyId.set(null);
    }
  }

  /**
   * "Explain (AI)" — fetch (once) an AI explanation for one finding and render
   * it inline; once loaded the button toggles the collapsible block instead of
   * re-fetching. One in flight per row; a rejection toasts the backend message
   * VERBATIM (consent-missing / Locked included) and leaves the row unchanged.
   * Stale guard: a response landing after the row resolved or vanished
   * (accept/dismiss/seal-purge mid-flight) is dropped.
   */
  async explain(f: AuditFinding): Promise<void> {
    const id = f.id;
    if (this.explaining().has(id)) {
      return;
    }
    if (this.explanations().has(id)) {
      this.explainCollapsed.update((s) => {
        const next = new Set(s);
        if (!next.delete(id)) {
          next.add(id);
        }
        return next;
      });
      return;
    }
    this.explaining.update((s) => new Set(s).add(id));
    try {
      const ex = await this.ipc.explainAuditFinding(id);
      const stillPending = this.store
        .findings()
        .some((x) => x.id === id && x.status === "pending");
      if (stillPending) {
        this.explanations.update((m) => new Map(m).set(id, ex));
      }
    } catch (e) {
      this.toast.danger(this.errorCopy.humanize(e));
    } finally {
      this.explaining.update((s) => {
        const next = new Set(s);
        next.delete(id);
        return next;
      });
    }
  }
}
