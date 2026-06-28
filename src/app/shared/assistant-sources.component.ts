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
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="src-block" aria-label="Źródła">
      <div class="src-head">
        <span class="src-ico" aria-hidden="true">🔗</span>
        <span class="src-title">Źródła</span>
        <span class="count">{{ sources().length }}</span>
      </div>

      <div class="src-rows">
        @for (s of visible(); track s.key) {
          @if (s.kind === "web" && s.url) {
            <a
              class="src-row is-web"
              [href]="s.url"
              target="_blank"
              rel="noreferrer noopener"
              [title]="s.url"
            >
              <span class="src-glyph" aria-hidden="true">
                <svg viewBox="0 0 16 16" width="13" height="13" fill="none">
                  <path
                    d="M6.5 9.5l3-3M7 4.5l.8-.8a2.5 2.5 0 0 1 3.5 3.5l-.8.8M9 11.5l-.8.8a2.5 2.5 0 0 1-3.5-3.5l.8-.8"
                    stroke="currentColor"
                    stroke-width="1.4"
                    stroke-linecap="round"
                  />
                </svg>
              </span>
              <span class="src-domain">{{ s.domain ?? "web" }}</span>
              @if (s.label && s.label !== s.domain) {
                <span class="src-label">{{ s.label }}</span>
              }
              <span class="src-extern" aria-hidden="true">↗</span>
            </a>
          } @else {
            <span class="src-row is-vault" [title]="s.label">
              <span class="src-glyph" aria-hidden="true">📄</span>
              <span class="src-label">{{ s.label }}</span>
            </span>
          }
        }
      </div>

      @if (hiddenCount() > 0) {
        <button
          type="button"
          class="src-toggle"
          (click)="toggle()"
          [attr.aria-expanded]="expanded()"
        >
          @if (expanded()) {
            Pokaż mniej
          } @else {
            Pokaż wszystkie ({{ sources().length }})
          }
        </button>
      }
    </div>
  `,
  styles: [
    `
      .src-block {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .src-head {
        display: flex;
        align-items: center;
        gap: var(--space-2);
      }
      .src-ico {
        font-size: 0.85rem;
        line-height: 1;
      }
      .src-title {
        color: var(--text-secondary);
        font-size: 0.78rem;
        font-weight: 600;
        text-transform: uppercase;
        letter-spacing: 0.04em;
      }
      .src-rows {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .src-row {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        min-width: 0;
        padding: 4px var(--space-2);
        border-radius: var(--radius-sm);
        border: 1px solid var(--border-subtle);
        background: var(--surface-input);
        font-size: 0.82rem;
        text-decoration: none;
        transition: border-color var(--transition), background var(--transition);
      }
      a.src-row.is-web {
        color: var(--text-secondary);
      }
      a.src-row.is-web:hover {
        border-color: var(--accent-soft);
        background: var(--accent-soft);
        color: var(--text-primary);
      }
      .src-glyph {
        flex: 0 0 auto;
        display: inline-flex;
        align-items: center;
        color: var(--accent);
        line-height: 1;
      }
      .src-domain {
        flex: 0 0 auto;
        color: var(--text-primary);
        font-weight: 600;
      }
      .src-label {
        flex: 1 1 auto;
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        color: var(--text-muted);
      }
      .src-extern {
        flex: 0 0 auto;
        color: var(--text-muted);
        font-size: 0.78rem;
      }
      .src-row.is-vault {
        color: var(--accent-hover);
        background: var(--accent-soft);
        border-color: transparent;
      }
      .src-row.is-vault .src-label {
        color: var(--accent-hover);
        flex: 0 1 auto;
        font-weight: 500;
      }
      .src-toggle {
        align-self: flex-start;
        padding: 2px var(--space-2);
        border: none;
        background: none;
        color: var(--accent-hover);
        font-size: 0.78rem;
        font-weight: 600;
        cursor: pointer;
        border-radius: var(--radius-sm);
      }
      .src-toggle:hover {
        text-decoration: underline;
      }
    `,
  ],
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
