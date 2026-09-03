import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
} from "@angular/core";
import { Router } from "@angular/router";
import { IpcService } from "../../../core/ipc.service";
import type {
  ClaimAlignment,
  EntityDetail,
  EntityKnowledgeDiff,
  EntityNeighbor,
  FactStateChange,
} from "../../../core/models";
import { SourcesComponent } from "../../../shared/sources/sources.component";
import { EntityNeighborhoodComponent } from "../entity-neighborhood/entity-neighborhood.component";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";

/** The two selectable comparison windows for the decision ledger's `from` bound. */
type LedgerRange = "90d" | "all";

/**
 * One rendered row of the AS-OF window diff (added / removed / changed between
 * the window's `from` and `to` snapshots) — the diff projected to a display
 * view-model in a `computed()` so the template never calls methods per row.
 */
interface DiffRowVm {
  /** Stable track key: kind + the backend's (subject, predicate) diff key + instant. */
  key: string;
  kind: "added" | "removed" | "changed";
  kindLabel: string;
  change: FactStateChange;
  /** Localized short date of the row's `validFrom`; empty on unparseable. */
  date: string;
}

/**
 * The right-hand detail panel for one selected entity. Loads its
 * {@link EntityDetail} via IPC (re-loading whenever the `entityId` input
 * changes), then renders: a header (name + kind chip), the bounded
 * neighborhood SVG, the backlinked meetings as reusable `app-sources` chips
 * (→ /meeting/:id), and a neighbors list where each row re-selects that entity.
 *
 * Loading/error/empty are all handled honestly. The IPC call is a one-shot
 * awaited promise (not a data stream), so it's loaded imperatively inside an
 * effect that tracks the input signal — no `subscribe`, no markForCheck.
 */
@Component({
  selector: "app-entity-detail",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [SourcesComponent, EntityNeighborhoodComponent],
  templateUrl: "./entity-detail.component.html",
  styleUrl: "./entity-detail.component.scss",
})
export class EntityDetailComponent {
  private readonly ipc = inject(IpcService);
  private readonly router = inject(Router);
  private readonly errorCopy = inject(ErrorCopyService);

  /** The entity to show detail for; changing it re-loads the panel. */
  readonly entityId = input.required<string>();
  /** Emits when the user picks a neighbor — the container re-selects it. */
  readonly selected = output<string>();
  /** Emits when the user dismisses the panel. */
  readonly closed = output<void>();

  readonly detail = signal<EntityDetail | null>(null);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

  /**
   * The comparison window for the decision ledger's `from` bound. The ledger
   * itself is the entity's WHOLE supersession history (window-independent, per the
   * backend), so the range only bounds the diff builder's cost — "all" walks the
   * full history, "90d" the last quarter. `to` is always "now".
   */
  readonly ledgerRange = signal<LedgerRange>("all");
  readonly diff = signal<EntityKnowledgeDiff | null>(null);
  /**
   * The ledger loads asynchronously AFTER the detail resolves, so its own spinner
   * is independent — the rest of the panel never blocks on it.
   */
  readonly ledgerLoading = signal(false);

  protected readonly meetingCountLabel = computed(() => {
    const n = this.detail()?.meetings.length ?? 0;
    return n === 1 ? "1 meeting" : `${n} meetings`;
  });

  /**
   * The chronological supersessions to render (oldest → newest); `[]` when the
   * entity has none — the template hides the whole section on empty, so an entity
   * that was never revised shows no ledger at all (graceful, not an empty card).
   */
  protected readonly ledger = computed<FactStateChange[]>(
    () => this.diff()?.ledger ?? [],
  );

  /**
   * The AS-OF window diff for the ACTIVE range (what the 90d / All-time control
   * actually scopes): every fact added / changed / removed between the window's
   * `from` and `to` snapshots, projected to display rows. Each backend list is
   * already deterministically key-sorted; concatenation order groups the kinds.
   */
  protected readonly windowRows = computed<DiffRowVm[]>(() => {
    const d = this.diff()?.diff;
    if (!d) {
      return [];
    }
    const row = (
      kind: DiffRowVm["kind"],
      kindLabel: string,
      c: FactStateChange,
    ): DiffRowVm => ({
      key: `${kind}|${c.subject}|${c.predicate}|${c.validFrom}`,
      kind,
      kindLabel,
      change: c,
      date: this.ledgerDate(c.validFrom),
    });
    return [
      ...d.added.map((c) => row("added", "Added", c)),
      ...d.changed.map((c) => row("changed", "Changed", c)),
      ...d.removed.map((c) => row("removed", "Removed", c)),
    ];
  });

  /** Human label of the active comparison window, for the diff sub-block title. */
  protected readonly windowLabel = computed(() =>
    this.ledgerRange() === "90d" ? "last 90 days" : "all time",
  );

  /**
   * Re-load the detail whenever `entityId` changes. The IPC call is a one-shot
   * promise, so we await it inside an effect that tracks the input signal — a
   * stale-result guard drops responses that resolve after the id moved on
   * (fast neighbor-to-neighbor pivots), so the panel never shows mismatched data.
   */
  private readonly _load = effect(
    () => {
      const id = this.entityId();
      this.loading.set(true);
      this.error.set(null);
      void this.fetch(id);
    },
    // Sets loading/error synchronously inside the tracked effect, so writes
    // must be permitted here.
  );

  /**
   * Re-load the decision ledger whenever the entity OR the chosen range changes.
   * Separate effect from `_load` so the (potentially slower) fact-diff never gates
   * the header/neighborhood render. Same stale-result discipline: the guard keys on
   * BOTH `entityId` and `ledgerRange`, so a response for a superseded id/range is
   * dropped (a fast pivot or a range toggle mid-flight never shows mismatched rows).
   */
  private readonly _loadLedger = effect(() => {
    const id = this.entityId();
    const range = this.ledgerRange();
    this.ledgerLoading.set(true);
    void this.fetchLedger(id, range);
  });

  private async fetch(id: string): Promise<void> {
    try {
      const result = await this.ipc.getEntityDetail(id);
      // Guard against an out-of-order resolution (the selection moved on).
      if (this.entityId() !== id) {
        return;
      }
      this.detail.set(result);
    } catch (e) {
      if (this.entityId() !== id) {
        return;
      }
      this.detail.set(null);
      this.error.set(this.errorCopy.humanize(e));
    } finally {
      if (this.entityId() === id) {
        this.loading.set(false);
      }
    }
  }

  private async fetchLedger(id: string, range: LedgerRange): Promise<void> {
    // Clear the previous entity's/range's rows immediately so a pivot never shows
    // stale supersessions while the new load is in flight.
    this.diff.set(null);
    const from = this.rangeFrom(range);
    const to = new Date().toISOString();
    try {
      const result = await this.ipc.getEntityKnowledgeDiff(id, from, to);
      // Drop a late response if the entity OR the range moved on mid-flight.
      if (this.entityId() !== id || this.ledgerRange() !== range) {
        return;
      }
      this.diff.set(result);
    } catch {
      // A ledger that can't load is non-fatal for the panel: leave `diff` null so
      // the section hides gracefully rather than surfacing a hard error over the
      // (already-loaded) entity detail.
      if (this.entityId() !== id || this.ledgerRange() !== range) {
        return;
      }
      this.diff.set(null);
    } finally {
      if (this.entityId() === id && this.ledgerRange() === range) {
        this.ledgerLoading.set(false);
      }
    }
  }

  /** The ISO `from` bound for a range: 90 days ago, or the epoch for "all-time". */
  private rangeFrom(range: LedgerRange): string {
    if (range === "all") {
      return new Date(0).toISOString();
    }
    const d = new Date();
    d.setDate(d.getDate() - 90);
    return d.toISOString();
  }

  protected setRange(range: LedgerRange): void {
    this.ledgerRange.set(range);
  }

  /**
   * A STABLE, UNIQUE track key for a ledger row: the composite of
   * `validFrom + subject + predicate`. `validFrom` alone is not unique (two
   * attributes of the same entity can supersede at the same instant), and neither
   * subject nor predicate alone is either — the triple is what the backend keys the
   * ledger on, so it's the safe identity.
   */
  protected ledgerKey(c: FactStateChange): string {
    // Separator written as an ESCAPE, never as a literal control byte: the source used to carry
    // two raw NULs here, invisible in every editor and diff and liable to truncate in tooling
    // that treats them as terminators. The CHARACTER is deliberately unchanged — NUL cannot occur
    // in a subject or predicate, which is what keeps this key collision-free. Substituting a
    // space, as first proposed, would have introduced one: "a b"+"c" and "a"+"b c" would share a
    // key. Same convention and same reasoning as `settings/model-id.ts`.
    return `${c.validFrom}\u0000${c.subject}\u0000${c.predicate}`;
  }

  /** Localized short date for a ledger row's `validFrom`; empty on unparseable. */
  protected ledgerDate(iso: string): string {
    const d = new Date(iso);
    return isNaN(d.getTime())
      ? ""
      : d.toLocaleDateString(undefined, {
          year: "numeric",
          month: "short",
          day: "numeric",
        });
  }

  protected sharedLabel(nb: EntityNeighbor): string {
    return nb.sharedMeetings === 1
      ? "1 shared meeting"
      : `${nb.sharedMeetings} shared meetings`;
  }

  /**
   * The Source chip on a ledger/diff row (Brain v3 audit PR-8, the PR-6 spec's
   * "receipt chip"): opens the fact's source meeting and — when the fact's text
   * aligns to a transcript segment (gated `get_fact_receipt`) — deep-seeks the
   * meeting's audio to that second via `?seekS=&seekSeg=` query params the
   * detail shell applies on load. No receipt (locked meeting, paraphrased fact,
   * or any IPC failure) falls back to plainly opening the meeting.
   */
  protected async openSource(c: FactStateChange): Promise<void> {
    const mid = c.sourceMeetingId;
    if (!mid) {
      return;
    }
    const factText = [c.subject, c.predicate, c.newObject ?? c.oldObject ?? ""]
      .join(" ")
      .trim();
    let receipt: ClaimAlignment | null;
    try {
      receipt = await this.ipc.getFactReceipt(mid, factText);
    } catch {
      receipt = null; // best-effort enhancement — never blocks opening the meeting.
    }
    if (receipt) {
      await this.router.navigate(["/meeting", mid], {
        queryParams: { seekS: receipt.startS, seekSeg: receipt.segmentId },
      });
    } else {
      await this.router.navigate(["/meeting", mid]);
    }
  }
}
