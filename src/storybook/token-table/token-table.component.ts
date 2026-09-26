import { ChangeDetectionStrategy, Component, computed, input, signal } from "@angular/core";

import { type CssTokenGroup, type TokenPreview, parseCssTokens, previewKind } from "./css-tokens";

interface TokenRow {
  readonly name: string;
  readonly value: string;
  readonly note: string;
  /** What `getComputedStyle(<html>)` resolves the token to under the CURRENT theme/accent/skin. */
  readonly live: string;
  readonly preview: TokenPreview;
  /** The value the swatch paints: `var(--x)` for a base token (so it follows the toolbar), the declared value for an override. */
  readonly paint: string;
}

interface SectionView {
  readonly key: string;
  readonly title: string;
  readonly rows: readonly TokenRow[];
}

interface GroupView {
  readonly context: string;
  readonly isBase: boolean;
  readonly sections: readonly SectionView[];
  readonly count: number;
}

/**
 * Storybook-only: renders one design-token file as a searchable catalogue —
 * name, declared value, the live value under the active toolbar theme, a
 * preview, and the comment that explains the token. Fed the file's SOURCE
 * (`import css from "…/colors.css?raw"`), so the page can never drift from
 * the file.
 */
@Component({
  selector: "app-token-table",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./token-table.component.html",
  styleUrl: "./token-table.component.scss",
})
export class TokenTableComponent {
  /** The raw CSS source of one `src/design-tokens/*.css` file. */
  readonly source = input.required<string>();
  /**
   * Any value that changes with the toolbar globals. The live values are read
   * from `<html>` and nothing about that read is a signal, so this is what
   * tells the table to read again after Theme / Accent / Skin change.
   */
  readonly revision = input<unknown>(null);

  readonly query = signal("");

  private readonly groups = computed<readonly CssTokenGroup[]>(() => parseCssTokens(this.source()));

  readonly total = computed(() =>
    this.groups().reduce(
      (sum, g) => sum + g.sections.reduce((n, s) => n + s.tokens.length, 0),
      0,
    ),
  );

  readonly views = computed<readonly GroupView[]>(() => {
    this.revision();
    const q = this.query().trim().toLowerCase();
    const style = getComputedStyle(document.documentElement);
    return this.groups()
      .map((group) => {
        const isBase = group.context === ":root";
        const sections = group.sections
          .map((section, i) => ({
            key: `${i}:${section.title}`,
            title: section.title,
            rows: section.tokens
              .filter(
                (t) =>
                  !q ||
                  t.name.includes(q) ||
                  t.value.toLowerCase().includes(q) ||
                  t.note.toLowerCase().includes(q),
              )
              .map((t) => {
                const live = style.getPropertyValue(t.name).trim();
                const paint = isBase ? `var(${t.name})` : t.value;
                return {
                  ...t,
                  live,
                  paint,
                  preview: previewKind(t.name, isBase ? live : t.value),
                };
              }),
          }))
          .filter((s) => s.rows.length > 0);
        return {
          context: group.context,
          isBase,
          sections,
          count: sections.reduce((n, s) => n + s.rows.length, 0),
        };
      })
      .filter((g) => g.count > 0);
  });

  onQuery(event: Event): void {
    this.query.set((event.target as HTMLInputElement).value);
  }
}
