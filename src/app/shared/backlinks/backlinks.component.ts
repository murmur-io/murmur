import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
  signal,
} from "@angular/core";
import { RouterLink } from "@angular/router";

import type { BacklinkSource } from "../../core/models";

/**
 * Note↔note backlinks reader ("Linked mentions"): a collapsible chip row of the
 * VISIBLE inbound sources — meetings AND authored notes — that mention/link the
 * current target. Modeled on {@link SourcesComponent}'s chip language, with the
 * click-through route split by kind: a `"meeting"` chip routes to `/meeting/:id`,
 * a `"note"` chip to `/notes/:id`. Shows the first `limit` chips with a
 * "+N more" / "Show less" toggle. Presentational — the host owns the gated
 * `get_backlinks` fetch (and MUST skip it while the target is locked/masked).
 */
@Component({
  selector: "app-backlinks",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  templateUrl: "./backlinks.component.html",
  styleUrl: "./backlinks.component.scss",
})
export class BacklinksComponent {
  readonly sources = input<BacklinkSource[]>([]);
  readonly limit = input(6);
  readonly expanded = signal(false);

  readonly visible = computed(() =>
    this.expanded() ? this.sources() : this.sources().slice(0, this.limit()),
  );

  toggle(): void {
    this.expanded.update((v) => !v);
  }

  /** The click-through route array for a source, split by its kind. */
  routeFor(s: BacklinkSource): unknown[] {
    return s.kind === "meeting"
      ? ["/meeting", s.id]
      : ["/notes", s.id];
  }

  fmt(iso: string): string {
    const d = new Date(iso);
    return isNaN(d.getTime())
      ? ""
      : d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  }
}
