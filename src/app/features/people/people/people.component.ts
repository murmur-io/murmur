import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import type { PersonCard } from "../../../core/models";
import { FoldersService } from "../../../services/folders.service";
import { PersonDossierComponent } from "../person-dossier/person-dossier.component";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";

/**
 * The `/people` page — a personal CRM over the people across your meetings.
 *
 * A directory of {@link PersonCard}s (name + when you last talked + how many open
 * commitments and known facts) is the spine; picking a card opens the structured
 * {@link PersonDossierComponent} pane — a glanceable, deterministic, egress-free
 * dossier (mentioning-meeting timeline + who-owes-what commitments + bitemporal
 * facts + co-occurring neighbours) assembled from the gated DB, no cloud call.
 *
 * Lock-awareness (mirrors GraphComponent): `listPeople()` returns ONLY visible
 * people with visible-only counts, so we re-fetch whenever {@link FoldersService}'s
 * tree signal changes (a session unlock/relock or screen-share re-lock shifts
 * visibility) — sealed people drop out, or reappear, live, with no client-side
 * security decision. The IPC call is a one-shot awaited promise written into a
 * signal (never subscribed-into a field).
 */
@Component({
  selector: "app-people",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [PersonDossierComponent],
  templateUrl: "./people.component.html",
  styleUrl: "./people.component.scss",
})
export class PeopleComponent {
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);
  private readonly errorCopy = inject(ErrorCopyService);

  readonly people = signal<PersonCard[]>([]);
  readonly loading = signal(true);
  readonly error = signal<string | null>(null);

  /**
   * The TRUE count of VISIBLE people, from `listPeople()`'s `totalVisiblePeople` — may exceed
   * `people().length` when the BACKEND's 500-row cap (`list_entities_visible`'s
   * `MAX_VISIBLE_ENTITIES`, upstream of `list_people`) trimmed the roster before it ever reached
   * this component. Defaults to 0 (matches `people`'s empty initial state).
   */
  readonly totalVisiblePeople = signal(0);

  /** The selected person id — opens the structured person-dossier pane. */
  readonly selectedId = signal<string | null>(null);

  protected readonly total = computed(() => this.people().length);

  /**
   * Whether the BACKEND capped the roster below the true visible-person count — independent of,
   * and reported ALONGSIDE, the client-side `RENDER_CAP` expander below. 2026-07-13 UX audit: the
   * old "Show all {{ total() }} people" button used `total()` (== `people().length`, the ALREADY
   * backend-capped roster) as if it were the true count, so a 650-person vault silently showed
   * "Show all 500 people" with 150 missing and no indication anything was cut.
   */
  protected readonly backendCapped = computed(
    () => this.totalVisiblePeople() > this.people().length,
  );

  /**
   * 2026-07-13 perf audit (LOW-MODERATE): `list_people` has no backend LIMIT, so a vault with
   * many people rendered every card unbounded — mirrors the pattern already established for the
   * transcript (`audio-panel.component.ts` RENDER_CAP=80) and the brain entity map
   * (`brain.component.ts` MAP_NODE_CAP=60), just not applied here yet.
   */
  private readonly RENDER_CAP = 100;
  protected readonly expanded = signal(false);
  protected readonly renderedPeople = computed<PersonCard[]>(() => {
    const all = this.people();
    if (this.expanded() || all.length <= this.RENDER_CAP) {
      return all;
    }
    return all.slice(0, this.RENDER_CAP);
  });
  protected readonly hiddenCount = computed(
    () => this.people().length - this.renderedPeople().length,
  );

  /**
   * Load the people list, and re-load whenever the folder lock-state changes.
   * Reading the folders `tree` signal registers this effect as its dependent, so
   * its initial value drives the first fetch (no separate `ngOnInit`), and a
   * later session unlock/relock — or a screen-share-triggered relock-all —
   * re-runs the fetch so sealed people drop out, or reappear, live (mirrors
   * GraphComponent). `fetch()` writes loading/error/data synchronously before its
   * first await, so this tracked effect must be allowed to write (NG0600 guard).
   */
  private readonly _refetchOnLock = effect(
    () => {
      this.folders.tree();
      void this.fetch();
    },
  );

  /**
   * Monotonic fetch id, so an out-of-order response cannot overwrite a newer one.
   *
   * `_refetchOnLock` re-runs on every folders-tree change — a session unlock, a relock, a
   * screen-share-triggered relock-all — so two fetches can be in flight at once, and nothing
   * guarantees they resolve in the order they started. Without this guard the LATER-started fetch
   * could land first and be overwritten by the older one, which for a visibility refetch is not a
   * cosmetic race: it can put sealed people back on screen after a relock, from a response that was
   * already stale when it arrived. `entity-detail.component.ts` keys the same discipline on the
   * entity id; there is no such identity here, so the sequence number is the identity.
   */
  private fetchSeq = 0;

  private async fetch(): Promise<void> {
    const seq = ++this.fetchSeq;
    this.error.set(null);
    try {
      const { people: rows, totalVisiblePeople } = await this.ipc.listPeople();
      if (seq !== this.fetchSeq) return;
      this.people.set(rows);
      this.totalVisiblePeople.set(totalVisiblePeople);
      // If the selected person is no longer visible (e.g. their folder re-sealed),
      // close the detail panel so we never point at a vanished person.
      const sel = this.selectedId();
      if (sel && !rows.some((p) => p.id === sel)) {
        this.selectedId.set(null);
      }
    } catch (e) {
      if (seq !== this.fetchSeq) return;
      this.people.set([]);
      this.totalVisiblePeople.set(0);
      this.error.set(this.errorCopy.humanize(e));
    } finally {
      // Only the newest fetch may clear the spinner; an older one finishing later must not
      // announce "done" while the current request is still running.
      if (seq === this.fetchSeq) this.loading.set(false);
    }
  }

  /** Open (or toggle closed) the detail panel for a person. */
  onSelect(id: string): void {
    this.selectedId.update((cur) => (cur === id ? null : id));
  }

  clearSelection(): void {
    this.selectedId.set(null);
  }

  /** Reveal the full people list (drops the RENDER_CAP window). */
  showAll(): void {
    this.expanded.set(true);
  }

  /** The uppercase leading letter for the avatar (fallback "?" for empty names). */
  protected initial(name: string): string {
    const c = name.trim().charAt(0);
    return c ? c.toUpperCase() : "?";
  }

  /** Human "last talked" label: Today / Yesterday / N days ago / a short date. */
  protected lastTalkedLabel(p: PersonCard): string {
    const iso = p.lastTalked;
    if (!iso) {
      return "No recent meetings";
    }
    const d = new Date(iso);
    if (isNaN(d.getTime())) {
      return "";
    }
    const now = new Date();
    const startOfDay = (x: Date): number =>
      new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
    const days = Math.round(
      (startOfDay(now) - startOfDay(d)) / 86_400_000,
    );
    if (days <= 0) {
      return "Talked today";
    }
    if (days === 1) {
      return "Talked yesterday";
    }
    if (days < 7) {
      return `Talked ${days} days ago`;
    }
    const opts: Intl.DateTimeFormatOptions =
      d.getFullYear() === now.getFullYear()
        ? { month: "short", day: "numeric" }
        : { month: "short", day: "numeric", year: "numeric" };
    return `Last talked ${d.toLocaleDateString(undefined, opts)}`;
  }

  protected commitmentTitle(p: PersonCard): string {
    return p.openCommitmentCount === 1
      ? "1 open commitment"
      : `${p.openCommitmentCount} open commitments`;
  }

  protected factTitle(p: PersonCard): string {
    return p.currentFactCount === 1
      ? "1 known fact"
      : `${p.currentFactCount} known facts`;
  }
}
