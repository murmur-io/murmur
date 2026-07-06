import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
  signal,
} from "@angular/core";

/**
 * One grounding source for the assistant-answer "🔗 Źródła" block. Both the live
 * record-surface card ({@link AssistantCitation}) and the persisted detail Q&A
 * ({@link ParsedCitation}) already parse the backend's flat citation strings into
 * a vault-vs-web shape — this is the common subset they both feed in.
 */
export interface AssistantSource {
  kind: "vault" | "web";
  /** Display label: the bare vault title, or the web result's title. */
  label: string;
  /** Destination URL for a web source (absent/empty for vault). */
  url?: string | null;
}

/** A deduped source enriched for rendering (stable key + extracted domain). */
interface RichSource extends AssistantSource {
  /** Stable `@for` key — the URL for web, the bracketed label for vault. */
  key: string;
  /** The extracted hostname (e.g. "accuweather.com"), web sources only. */
  domain: string | null;
}

/**
 * Compact, rich, deduped "🔗 Źródła" block shared by BOTH assistant surfaces
 * (the live `app-assistant-actions` card and the persisted detail Q&A section).
 * Replaces the old giant flat list of near-duplicate "VIA WEB" chips:
 *
 * - DEDUPE on URL (web) / label (vault) — exact duplicates collapse.
 * - GROUP a small "via web" / "vault" affordance per row instead of a per-chip
 *   "VIA WEB" tag; the header carries the unique count.
 * - CAP to the first {@link AssistantSourcesComponent.PREVIEW} rows; the rest
 *   hide behind a "Pokaż wszystkie ({N})" toggle (a signal + `@if`, no wall).
 * - Each web row shows a link glyph + the extracted **domain** + truncated
 *   title, the whole row a clickable external link. Vault rows are a distinct
 *   no-URL chip.
 *
 * In-flow (not floated), so the frosted-surface tokens are fine — no overlay
 * (trap T3 does not apply here). Tokens only; no new deps.
 */
@Component({
  selector: "app-assistant-sources",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./assistant-sources.component.html",
  styleUrl: "./assistant-sources.component.scss",
})
export class AssistantSourcesComponent {
  /** Number of sources shown before the "Pokaż wszystkie" toggle. */
  private static readonly PREVIEW = 4;

  /** Raw parsed citations from either surface (vault/web shape). */
  readonly citations = input<readonly AssistantSource[]>([]);

  /** Deduped + domain-enriched sources (exact URL/label duplicates collapse). */
  readonly sources = computed<RichSource[]>(() => {
    const seen = new Set<string>();
    const out: RichSource[] = [];
    for (const c of this.citations()) {
      const url = c.url ?? null;
      const isWeb = c.kind === "web" && !!url;
      const domain = isWeb ? this.domainOf(url) : null;
      // Dedup key: the URL for web hits, the bracketed label for vault.
      const key = isWeb ? `w:${url}` : `v:${c.label.trim().toLowerCase()}`;
      if (seen.has(key)) continue;
      seen.add(key);
      out.push({
        kind: isWeb ? "web" : "vault",
        label: c.label.trim(),
        url: isWeb ? url : null,
        domain,
        key,
      });
    }
    return out;
  });

  private readonly _expanded = signal(false);
  readonly expanded = this._expanded.asReadonly();

  /** The visible slice — first PREVIEW rows unless expanded. */
  readonly visible = computed<RichSource[]>(() => {
    const all = this.sources();
    return this._expanded()
      ? all
      : all.slice(0, AssistantSourcesComponent.PREVIEW);
  });

  /** How many sources are hidden behind the toggle (0 → no toggle). */
  readonly hiddenCount = computed(() =>
    Math.max(0, this.sources().length - AssistantSourcesComponent.PREVIEW),
  );

  toggle(): void {
    this._expanded.update((v) => !v);
  }

  /** Extract a clean hostname ("www." stripped) — null if unparseable. */
  private domainOf(url: string): string | null {
    try {
      return new URL(url).hostname.replace(/^www\./i, "");
    } catch {
      return null;
    }
  }
}
