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
import { RouterLink } from "@angular/router";

import { IpcService } from "../../../core/ipc.service";
import type {
  Commitment,
  DossierData,
  EntityNeighbor,
} from "../../../core/models";

/**
 * The `/people` detail pane — a STRUCTURED, glanceable dossier for one person
 * (Tier 4b). Loads the gated {@link DossierData} via {@link IpcService.getPersonDossier}
 * (re-loading whenever the `entityId` input changes) and renders four native
 * sections over it:
 *   • 🕑 a TIMELINE of the meetings that mention this person (newest first → /meeting),
 *   • ⏳ WHO OWES WHAT — the open commitments tied to them (owner · due · item · source),
 *   • 🔵 FACTS — the current bitemporal state (open facts) plus WHAT CHANGED (closed),
 *   • 🧭 co-occurring NEIGHBOURS — click to pivot the pane to that entity.
 *
 * This replaces the reused graph {@link EntityDetailComponent} in the pane: strictly
 * richer, deterministic, and egress-free (no cloud synthesis). The component contract
 * (`entityId` input + `select`/`close` outputs) matches the panel it supersedes, so
 * the container wiring is unchanged.
 *
 * The IPC call is a one-shot awaited promise (not a data stream), loaded imperatively
 * inside an `effect()` that tracks the input signal — a stale-result guard drops a
 * response that resolves after the id moved on (fast neighbour-to-neighbour pivots),
 * and `allowSignalWrites` is required because loading/error are written synchronously
 * inside that tracked effect (NG0600 guard). Mirrors `entity-detail.component.ts`.
 */
@Component({
  selector: "app-person-dossier",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  templateUrl: "./person-dossier.component.html",
  styleUrl: "./person-dossier.component.scss",
})
export class PersonDossierComponent {
  private readonly ipc = inject(IpcService);

  /** The person/entity to build the dossier for; changing it re-loads the pane. */
  readonly entityId = input.required<string>();
  /** Emits when the user picks a neighbour — the container re-selects it. */
  readonly select = output<string>();
  /** Emits when the user dismisses the pane. */
  readonly close = output<void>();

  readonly dossier = signal<DossierData | null>(null);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

  /** Currently-valid facts (open — `validTo == null`): the present state. */
  protected readonly currentFacts = computed(() =>
    (this.dossier()?.facts ?? []).filter((f) => f.validTo == null),
  );
  /** Superseded facts (closed — `validTo` set): the history / what changed. */
  protected readonly changedFacts = computed(() =>
    (this.dossier()?.facts ?? []).filter((f) => f.validTo != null),
  );

  /** A compact "3 meetings · 2 open · 4 facts" summary for the header. */
  protected readonly summaryLabel = computed(() => {
    const d = this.dossier();
    if (!d) {
      return "";
    }
    const parts: string[] = [
      d.meetings.length === 1 ? "1 meeting" : `${d.meetings.length} meetings`,
    ];
    if (d.commitments.length) {
      parts.push(`${d.commitments.length} open`);
    }
    const facts = this.currentFacts().length;
    if (facts) {
      parts.push(facts === 1 ? "1 fact" : `${facts} facts`);
    }
    return parts.join(" · ");
  });

  /**
   * Re-load the dossier whenever `entityId` changes. The IPC call is a one-shot
   * promise, so we await it inside an effect that tracks the input signal — a
   * stale-result guard drops responses that resolve after the id moved on (fast
   * neighbour-to-neighbour pivots), so the pane never shows mismatched data.
   * `allowSignalWrites` because loading/error are set synchronously here (NG0600).
   */
  private readonly _load = effect(
    () => {
      const id = this.entityId();
      this.loading.set(true);
      this.error.set(null);
      void this.fetch(id);
    },
    { allowSignalWrites: true },
  );

  private async fetch(id: string): Promise<void> {
    try {
      const result = await this.ipc.getPersonDossier(id);
      // Guard against an out-of-order resolution (the selection moved on).
      if (this.entityId() !== id) {
        return;
      }
      this.dossier.set(result);
    } catch (e) {
      if (this.entityId() !== id) {
        return;
      }
      this.dossier.set(null);
      this.error.set(String(e));
    } finally {
      if (this.entityId() === id) {
        this.loading.set(false);
      }
    }
  }

  /** The trimmed owner name, or null when unattributed (so the chip is skipped). */
  protected ownerLabel(c: Commitment): string | null {
    const o = c.owner?.trim();
    return o ? o : null;
  }

  protected sharedLabel(nb: EntityNeighbor): string {
    return nb.sharedMeetings === 1
      ? "1 shared meeting"
      : `${nb.sharedMeetings} shared meetings`;
  }

  /** Short, locale-aware date for timeline rows + fact validity (year only when it differs). */
  protected fmtDate(iso: string | null): string {
    if (!iso) {
      return "";
    }
    const d = new Date(iso);
    if (isNaN(d.getTime())) {
      // Fall back to the raw date portion (facts/meetings always carry ISO strings).
      return iso.split(/[T ]/)[0] ?? "";
    }
    const now = new Date();
    const opts: Intl.DateTimeFormatOptions =
      d.getFullYear() === now.getFullYear()
        ? { month: "short", day: "numeric" }
        : { month: "short", day: "numeric", year: "numeric" };
    return d.toLocaleDateString(undefined, opts);
  }
}
