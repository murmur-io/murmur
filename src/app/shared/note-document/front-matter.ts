export interface SplitNoteDocument {
  readonly frontMatterPrefix: string;
  readonly body: string;
  readonly tags: readonly string[];
}

/**
 * Split the optional leading YAML block without interpreting or normalising it.
 * The prefix is owned-file data: joining it with an unchanged body is byte exact.
 * Unterminated or non-leading fences are deliberately treated as body content.
 */
export function splitNoteDocument(markdown: string): SplitNoteDocument {
  const bomLength = markdown.startsWith("\uFEFF") ? 1 : 0;
  const start = markdown.slice(bomLength);
  const opening = /^(---[ \t]*(?:\r\n|\n))/.exec(start);
  if (!opening) {
    return { frontMatterPrefix: "", body: markdown, tags: [] };
  }

  const lineBreak = opening[1].endsWith("\r\n") ? "\r\n" : "\n";
  const yamlStart = bomLength + opening[1].length;
  const closePattern = new RegExp(`^---[ \\t]*(?:${lineBreak === "\r\n" ? "\\r\\n" : "\\n"}|$)`, "m");
  const close = closePattern.exec(markdown.slice(yamlStart));
  if (!close) {
    return { frontMatterPrefix: "", body: markdown, tags: [] };
  }

  let bodyStart = yamlStart + close.index + close[0].length;
  const blankLine = lineBreak === "\r\n" ? /^(?:[ \t]*\r\n)/ : /^(?:[ \t]*\n)/;
  let separator: RegExpExecArray | null;
  while ((separator = blankLine.exec(markdown.slice(bodyStart)))) {
    bodyStart += separator[0].length;
  }

  const frontMatterPrefix = markdown.slice(0, bodyStart);
  const yaml = markdown.slice(yamlStart, yamlStart + close.index);
  return {
    frontMatterPrefix,
    body: markdown.slice(bodyStart),
    tags: readTags(yaml),
  };
}

export function joinNoteDocument(frontMatterPrefix: string, body: string): string {
  // A valid closing fence can end at EOF. Keep an untouched file byte-exact,
  // but put newly entered prose on its own line instead of corrupting the fence.
  const separator = body && frontMatterPrefix && !frontMatterPrefix.endsWith("\n")
    ? (frontMatterPrefix.includes("\r\n") ? "\r\n" : "\n") : "";
  return `${frontMatterPrefix}${separator}${body}`;
}

function readTags(yaml: string): readonly string[] {
  const lines = yaml.split(/\r?\n/);
  for (let index = 0; index < lines.length; index += 1) {
    const match = /^tags:\s*(.*)$/i.exec(lines[index]);
    if (!match) continue;
    const value = match[1].trim();
    if (value.startsWith("[") && value.endsWith("]")) {
      return normalizeTags(value.slice(1, -1).split(","));
    }
    if (value) return normalizeTags([value]);
    const block: string[] = [];
    while (index + 1 < lines.length) {
      const item = /^\s*-\s+(.*)$/.exec(lines[index + 1]);
      if (!item) break;
      block.push(item[1]);
      index += 1;
    }
    return normalizeTags(block);
  }
  return [];
}

function normalizeTags(values: readonly string[]): readonly string[] {
  return [...new Set(values.map((value) => value.trim().replace(/^['"]|['"]$/g, "").replace(/^#/, "").toLowerCase()).filter(Boolean))];
}
