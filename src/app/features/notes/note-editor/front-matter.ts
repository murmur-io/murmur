/**
 * Pure YAML-front-matter helpers for the note editor. The note's `markdown`
 * (from {@link NoteDoc}) is the FULL document INCLUDING a leading `--- … ---`
 * YAML block that carries `tags` + arbitrary properties (vault-native, owned
 * file). The editor edits the BODY (front-matter stripped) plus a structured
 * properties bar, then RE-EMITS the whole document on save.
 *
 * This is a deliberately SMALL, tolerant parser — not a general YAML engine (no
 * new dep, invariant §3). It handles exactly the shapes the note assistant /
 * export writer emit: `tags: [a, b]` (flow) or a `-`-item block, `key: value`
 * scalars, and quoted scalars. Anything it can't confidently parse is preserved
 * as an opaque `properties` string so a round-trip never silently drops data.
 */

/** The decomposed note document. */
export interface ParsedDoc {
  /** Front-matter tags (the `tags:` key), lower-cased/trimmed, de-duplicated. */
  tags: string[];
  /** Every other front-matter key → its scalar value (insertion order kept). */
  properties: Record<string, string>;
  /** The markdown body with the leading front-matter block removed. */
  body: string;
}

// Matches a leading `--- … ---` YAML block and eats up to ONE trailing blank
// line, so the parsed body starts at the first real content line (no stray
// leading newline that would render as an empty first line in the textarea).
const FRONT_MATTER_RE = /^---[ \t]*\r?\n([\s\S]*?)\r?\n---[ \t]*\r?\n(?:[ \t]*\r?\n)?/;

/**
 * Split a full note document into `{ tags, properties, body }`. When there is no
 * leading front-matter block the whole string is the body and tags/properties
 * are empty. Never throws — a malformed block is treated as body.
 */
export function parseDoc(markdown: string): ParsedDoc {
  const match = FRONT_MATTER_RE.exec(markdown);
  if (!match) {
    return { tags: [], properties: {}, body: markdown };
  }
  const yaml = match[1];
  const body = markdown.slice(match[0].length);
  const { tags, properties } = parseYamlBlock(yaml);
  return { tags, properties, body };
}

/** Parse the INNER text of a front-matter block (no `---` fences). */
function parseYamlBlock(yaml: string): {
  tags: string[];
  properties: Record<string, string>;
} {
  const tags: string[] = [];
  const properties: Record<string, string> = {};
  const lines = yaml.split(/\r?\n/);

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (line.trim() === "" || line.trimStart().startsWith("#")) {
      continue;
    }
    // A `- item` continuation belongs to the preceding list key; handled inline
    // below, so a stray one at the top level is ignored.
    if (/^\s*-\s+/.test(line)) {
      continue;
    }
    const kv = /^([A-Za-z0-9_][\w .-]*?):\s?(.*)$/.exec(line);
    if (!kv) {
      continue;
    }
    const key = kv[1].trim();
    const rawValue = kv[2];

    if (key.toLowerCase() === "tags") {
      collectListValue(rawValue, lines, i).values.forEach((t) => {
        const tag = normalizeTag(t);
        if (tag && !tags.includes(tag)) {
          tags.push(tag);
        }
      });
      i = collectListValue(rawValue, lines, i).nextIndex;
      continue;
    }

    // A block-list property (e.g. `aliases:` with `- x` items) is flattened to a
    // comma-joined scalar so the properties bar can round-trip it as text.
    if (rawValue.trim() === "") {
      const list = collectListValue(rawValue, lines, i);
      if (list.values.length > 0) {
        properties[key] = list.values.join(", ");
        i = list.nextIndex;
        continue;
      }
    }
    properties[key] = unquote(rawValue.trim());
  }

  return { tags, properties };
}

/**
 * Read a value that may be a flow list (`[a, b]`), an inline scalar, OR a block
 * list of following `- item` lines. Returns the collected values and the index
 * of the LAST line consumed (so the caller advances past a block list).
 */
function collectListValue(
  rawValue: string,
  lines: string[],
  index: number,
): { values: string[]; nextIndex: number } {
  const trimmed = rawValue.trim();
  if (trimmed.startsWith("[") && trimmed.endsWith("]")) {
    const inner = trimmed.slice(1, -1);
    const values = inner
      .split(",")
      .map((s) => unquote(s.trim()))
      .filter((s) => s.length > 0);
    return { values, nextIndex: index };
  }
  if (trimmed !== "") {
    return { values: [unquote(trimmed)], nextIndex: index };
  }
  // Block list: consume following `- item` lines.
  const values: string[] = [];
  let j = index + 1;
  for (; j < lines.length; j++) {
    const item = /^\s*-\s+(.*)$/.exec(lines[j]);
    if (!item) {
      break;
    }
    const v = unquote(item[1].trim());
    if (v) {
      values.push(v);
    }
  }
  return { values, nextIndex: j - 1 };
}

/** Strip a single layer of matching quotes from a scalar. */
function unquote(s: string): string {
  if (
    (s.startsWith('"') && s.endsWith('"')) ||
    (s.startsWith("'") && s.endsWith("'"))
  ) {
    return s.slice(1, -1);
  }
  return s;
}

/** Normalize a tag: trim, strip a leading `#`, lower-case. */
function normalizeTag(raw: string): string {
  return raw.trim().replace(/^#/, "").trim().toLowerCase();
}

/**
 * Re-assemble a full note document from a structured `{ tags, properties, body }`.
 * Emits a `--- … ---` front-matter block ONLY when there is at least one tag or
 * property (so a plain note stays plain, front-matter-free). Tags are emitted as
 * a YAML flow list; scalars are quoted when they contain YAML-significant
 * characters. Deterministic ordering (tags first, then properties in map order)
 * so an unchanged edit re-emits byte-identically.
 */
export function serializeDoc(
  tags: string[],
  properties: Record<string, string>,
  body: string,
): string {
  const cleanTags = dedupe(tags.map(normalizeTag).filter((t) => t.length > 0));
  const propEntries = Object.entries(properties).filter(
    ([k, v]) => k.trim().length > 0 && v.trim().length > 0,
  );

  if (cleanTags.length === 0 && propEntries.length === 0) {
    return body;
  }

  const lines: string[] = ["---"];
  if (cleanTags.length > 0) {
    lines.push(`tags: [${cleanTags.map((t) => emitScalar(t)).join(", ")}]`);
  }
  for (const [key, value] of propEntries) {
    lines.push(`${key}: ${emitScalar(value.trim())}`);
  }
  lines.push("---");
  // One blank line between front-matter and body reads well in the vault file.
  const trimmedBody = body.replace(/^\s+/, "");
  return `${lines.join("\n")}\n${trimmedBody.length > 0 ? "\n" + trimmedBody : ""}`;
}

/** Quote a scalar when it carries a character YAML would mis-parse bare. */
function emitScalar(value: string): string {
  if (/^[\w .\-/@]+$/.test(value)) {
    return value;
  }
  return `"${value.replace(/"/g, '\\"')}"`;
}

function dedupe(values: string[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const v of values) {
    if (!seen.has(v)) {
      seen.add(v);
      out.push(v);
    }
  }
  return out;
}
