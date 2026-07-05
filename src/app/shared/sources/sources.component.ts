import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
  signal,
} from "@angular/core";
import { RouterLink } from "@angular/router";

import type { VaultSource } from "../../core/models";

/**
 * Collapsible list of source meetings (for Ask / Brief). Shows the first `limit` as chips that
 * route to the meeting, with a "+N more" toggle for the rest.
 */
@Component({
  selector: "app-sources",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  templateUrl: "./sources.component.html",
  styleUrl: "./sources.component.scss",
})
export class SourcesComponent {
  readonly sources = input<VaultSource[]>([]);
  readonly limit = input<number>(4);
  readonly expanded = signal(false);

  readonly visible = computed(() =>
    this.expanded() ? this.sources() : this.sources().slice(0, this.limit()),
  );

  toggle(): void {
    this.expanded.update((v) => !v);
  }

  fmt(iso: string): string {
    const d = new Date(iso);
    return isNaN(d.getTime())
      ? ""
      : d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  }
}
