/**
 * Reads a design-token CSS file (the SOURCE text, via a `?raw` import) into
 * the groups the Storybook token pages render.
 *
 * The files in `src/design-tokens/` follow one shape: rule blocks (`:root`,
 * `:root[data-theme="light"]`, `@media (…) { :root… }`) holding custom-property
 * declarations, grouped under section banners (`/* --- Surfaces … --- *\/`),
 * with a note either on the lines above a declaration or trailing it. The
 * parser keeps all three — the context a token is declared in, its section and
 * its note — because the notes are most of the documentation this repo has.
 *
 * It is a small scanner rather than a regex because values span lines
 * (`--aurora-field`, multi-layer shadows) and contain `;`/`:` inside `url(…)`.
 */

export interface CssToken {
  /** `--surface-base` */
  readonly name: string;
  /** The declared value, whitespace-collapsed (may reference other tokens). */
  readonly value: string;
  /** The comment that explains it, if any (leading block or trailing note). */
  readonly note: string;
}

export interface CssTokenSection {
  /** Banner title (`Surfaces (deep layered field; glass sits on top)`), or "" before the first banner. */
  readonly title: string;
  readonly tokens: readonly CssToken[];
}

export interface CssTokenGroup {
  /** Where the tokens are declared: `:root`, or `@media (…) › :root[data-theme="system"]`. */
  readonly context: string;
  readonly sections: readonly CssTokenSection[];
}

/** A section banner: `/* --- Title ----- *\/`, `/* ── Blue ── *\/`, or a `/* ═══` block. */
const BANNER = /^\s*(?:-{3}|─{2}|═{3})/;
const DECORATION = /^[\s\-=─═]+|[\s\-=─═]+$/g;

function collapse(text: string): string {
  return text.replace(/\s+/g, " ").trim();
}

function bannerTitle(comment: string): string {
  const firstLine =
    comment
      .split("\n")
      .map((line) => line.replace(DECORATION, "").trim())
      .find((line) => line.length > 0) ?? "";
  return firstLine;
}

/** Index of the `;` ending a declaration value, skipping any nested in parentheses or quotes. */
function valueEnd(src: string, from: number): number {
  let depth = 0;
  let quote: string | null = null;
  for (let i = from; i < src.length; i++) {
    const ch = src[i];
    if (quote) {
      if (ch === quote) quote = null;
    } else if (ch === '"' || ch === "'") {
      quote = ch;
    } else if (ch === "(") {
      depth++;
    } else if (ch === ")") {
      depth = Math.max(0, depth - 1);
    } else if ((ch === ";" || ch === "}") && depth === 0) {
      return i;
    }
  }
  return src.length;
}

export function parseCssTokens(src: string): CssTokenGroup[] {
  const groups = new Map<string, Map<string, CssToken[]>>();
  const stack: string[] = [];
  let section = "";
  let pendingNote = "";
  let last: { list: CssToken[]; index: number; line: number } | null = null;
  let i = 0;

  const lineOf = (pos: number): number => {
    let n = 0;
    for (let k = 0; k < pos; k++) if (src[k] === "\n") n++;
    return n;
  };

  while (i < src.length) {
    const ch = src[i];
    if (/\s/.test(ch)) {
      i++;
      continue;
    }

    if (src.startsWith("/*", i)) {
      const end = src.indexOf("*/", i + 2);
      const stop = end === -1 ? src.length : end;
      const body = src.slice(i + 2, stop);
      const startLine = lineOf(i);
      i = stop + 2;
      if (BANNER.test(body)) {
        section = bannerTitle(body);
        pendingNote = "";
      } else if (last && last.line === startLine) {
        // A note trailing a declaration on its own line belongs to THAT token.
        const token = last.list[last.index];
        last.list[last.index] = { ...token, note: collapse(`${token.note} ${body}`) };
      } else {
        pendingNote = collapse(body);
      }
      continue;
    }

    if (ch === "}") {
      stack.pop();
      section = "";
      pendingNote = "";
      last = null;
      i++;
      continue;
    }

    if (src.startsWith("--", i)) {
      const colon = src.indexOf(":", i);
      const end = valueEnd(src, colon + 1);
      const name = src.slice(i, colon).trim();
      const value = collapse(src.slice(colon + 1, end));
      const context = stack.length > 0 ? stack.join(" › ") : ":root";
      const sections = groups.get(context) ?? new Map<string, CssToken[]>();
      groups.set(context, sections);
      const list = sections.get(section) ?? [];
      sections.set(section, list);
      list.push({ name, value, note: pendingNote });
      last = { list, index: list.length - 1, line: lineOf(end) };
      pendingNote = "";
      i = src[end] === ";" ? end + 1 : end;
      continue;
    }

    // A selector / at-rule prelude up to `{` (a block) or `;` (e.g. @import).
    let j = i;
    while (j < src.length && src[j] !== "{" && src[j] !== ";" && !src.startsWith("/*", j)) j++;
    if (src[j] === "{") {
      stack.push(collapse(src.slice(i, j)));
      section = "";
      pendingNote = "";
      last = null;
      i = j + 1;
    } else {
      i = src[j] === ";" ? j + 1 : j;
    }
  }

  return [...groups.entries()].map(([context, sections]) => ({
    context,
    sections: [...sections.entries()].map(([title, tokens]) => ({ title, tokens })),
  }));
}

export type TokenPreview = "color" | "fill" | "radius" | "length" | "shadow" | "font" | "none";

const LENGTH = /^-?\d*\.?\d+(px|rem|em)$/;
const LENGTH_NAME = /space|gap|inset|pad|slot|indent|width|height|-w$|-h$|max|min|size/;

/** Picks how a token is previewed, from its name and its CURRENT computed value. */
export function previewKind(name: string, resolved: string): TokenPreview {
  const value = resolved.trim();
  if (!value) return "none";
  if (name.includes("shadow") || /^inset\s/.test(value)) return "shadow";
  if (name.includes("radius")) return "radius";
  if (name.startsWith("--font-") && !/size|weight/.test(name)) return "font";
  if (/gradient\(/.test(value)) return "fill";
  if (typeof CSS !== "undefined" && CSS.supports("color", value)) return "color";
  if (LENGTH.test(value) && LENGTH_NAME.test(name)) return "length";
  return "none";
}
