import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  signal,
} from "@angular/core";
import type { AuditFinding, AuditFindingKind } from "../../../core/models";
import { MurSpinnerComponent } from "../../../design-system/spinner/spinner.component";
import { ToastService } from "../../../services/toast.service";
import { MarkdownComponent } from "../../../shared/markdown/markdown.component";
import { AuditStore } from "../audit.store";

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
  private readonly toast = inject(ToastService);

  /** The user's manual collapse/expand toggle (auto-opens on "Audit now"). */
  readonly open = signal(false);

  /** The finding id with a resolve in flight (disables just that row's buttons). */
  readonly busyId = signal<string | null>(null);

  readonly listEmpty = computed(() => this.store.pendingCount() === 0);

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
      this.toast.danger(String(e));
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
      this.toast.danger(String(e));
    } finally {
      this.busyId.set(null);
    }
  }

  async dismiss(f: AuditFinding): Promise<void> {
    this.busyId.set(f.id);
    try {
      await this.store.resolve(f.id, "dismiss");
    } catch (e) {
      this.toast.danger(String(e));
    } finally {
      this.busyId.set(null);
    }
  }
}
