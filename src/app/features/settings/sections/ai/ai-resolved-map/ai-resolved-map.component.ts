import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
} from "@angular/core";
import type { AiMapRow } from "../../../../../core/models";
import { SettingsStore } from "../../../settings.store";

/**
 * AI & Models → "What runs where": the resolved-map card. A read-only,
 * always-visible mirror of the backend role resolver (`resolved_ai_map`) —
 * one row per AI job with the engine serving it RIGHT NOW, split into two
 * honest groups: rows whose text LEAVES the Mac (cloud, redacted first) and
 * rows that STAY on the Mac. This grouping is the fix for the "I picked Cloud —
 * why is transcription local?" confusion: the always-on-device jobs (Whisper,
 * search, NER, reactions) live under their own heading instead of reading as a
 * contradiction next to the cloud rows.
 *
 * A routable row's "Change" opens Advanced AND asks the role rows to scroll to +
 * flash that role's override row (`requestHighlightRole`). In-flow card (frosted
 * .card is correct — not a floating overlay).
 */
@Component({
  selector: "app-ai-resolved-map",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./ai-resolved-map.component.html",
  styleUrl: "./ai-resolved-map.component.scss",
})
export class AiResolvedMapComponent {
  private readonly store = inject(SettingsStore);
  readonly rows = this.store.aiMap;

  /** Rows split by egress: cloud (`!onDevice`) vs on-Mac (`onDevice`). */
  readonly groups = computed<{
    cloud: readonly AiMapRow[];
    mac: readonly AiMapRow[];
  }>(() => {
    const all = this.rows();
    return {
      cloud: all.filter((r) => !r.onDevice),
      mac: all.filter((r) => r.onDevice),
    };
  });

  /**
   * A routable row's "Change" → open Advanced AND ask the role rows to scroll
   * to + flash that role's override row. `job` is `notes`/`ask`/`live` on
   * routable rows (the only rows with a Change button — backend `ai_map_rows`).
   */
  change(job: string): void {
    this.store.expandAdvanced();
    this.store.requestHighlightRole(job as "notes" | "ask" | "live");
  }
}
