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

  readonly people = signal<PersonCard[]>([]);
  readonly loading = signal(true);
  readonly error = signal<string | null>(null);

  /** The selected person id — opens the structured person-dossier pane. */
  readonly selectedId = signal<string | null>(null);

  protected readonly total = computed(() => this.people().length);

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

  private async fetch(): Promise<void> {
    this.error.set(null);
    try {
      const rows = await this.ipc.listPeople();
      this.people.set(rows);
      // If the selected person is no longer visible (e.g. their folder re-sealed),
      // close the detail panel so we never point at a vanished person.
      const sel = this.selectedId();
      if (sel && !rows.some((p) => p.id === sel)) {
        this.selectedId.set(null);
      }
    } catch (e) {
      this.people.set([]);
      this.error.set(String(e));
    } finally {
      this.loading.set(false);
    }
  }

  /** Open (or toggle closed) the detail panel for a person. */
  onSelect(id: string): void {
    this.selectedId.update((cur) => (cur === id ? null : id));
  }

  clearSelection(): void {
    this.selectedId.set(null);
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
