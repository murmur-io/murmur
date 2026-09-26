import { marked, type Tokens } from "marked";

/**
 * GFM task lists (`- [ ] todo` / `- [x] done`) — the source-side half of the
 * clickable checkboxes rendered by `MarkdownComponent`.
 *
 * The rendered DOM only knows "the Nth checkbox"; the markdown source knows
 * lines. Mapping one to the other by regex alone is fragile (a `- [ ]` inside a
 * fenced code block, an indented code block, or a list item whose first child
 * is not text are NOT tasks to marked), so every toggle is VERIFIED against
 * marked's own lexer: the result is accepted only when re-lexing it yields
 * exactly the old task states with position `index` flipped. Anything else
 * returns `null` and the caller leaves the note untouched — a wrong toggle
 * would silently edit a line the user never clicked.
 */

/** A list-item line whose content starts with `[ ]` / `[x]` — a task CANDIDATE. */
const CANDIDATE =
  /^((?:[ \t]*>[ \t]?)*[ \t]*(?:[-*+]|\d{1,9}[.)])[ \t]+\[)([ xX])(\][ \t]+\S)/;

/** Opening/closing code fence (optionally inside a blockquote). */
const FENCE = /^(?:[ \t]*>[ \t]?)*[ \t]{0,3}(`{3,}|~{3,})/;

/** Checked state of every task item, in document (= rendered DOM) order. */
export function taskStates(markdown: string): boolean[] {
  const states: boolean[] = [];
  const tokens = marked.lexer(markdown, { gfm: true, breaks: true });
  marked.walkTokens(tokens, (token) => {
    if (token.type === "list_item") {
      const item = token as Tokens.ListItem;
      if (item.task) {
        states.push(!!item.checked);
      }
    }
  });
  return states;
}

/**
 * Flip the `index`-th task (0-based, document order) in `markdown`.
 * Returns the new markdown, or `null` when the toggle cannot be proven exact.
 */
export function toggleTask(markdown: string, index: number): string | null {
  const before = taskStates(markdown);
  if (!Number.isInteger(index) || index < 0 || index >= before.length) {
    return null;
  }
  const expected = before.map((checked, i) => (i === index ? !checked : checked));
  const lines = markdown.split("\n");

  const verified = (lineNo: number): string | null => {
    const next = flipLine(lines, lineNo);
    if (next === null) {
      return null;
    }
    const after = taskStates(next);
    return after.length === expected.length && after.every((c, i) => c === expected[i])
      ? next
      : null;
  };

  // Fast path: the Nth candidate outside fenced code — right for ordinary notes.
  const candidates: number[] = [];
  const outsideFences: number[] = [];
  let fence: string | null = null;
  lines.forEach((line, lineNo) => {
    const f = FENCE.exec(line);
    if (f) {
      if (fence === null) {
        fence = f[1][0];
      } else if (f[1][0] === fence) {
        fence = null;
      }
      return;
    }
    if (CANDIDATE.test(line)) {
      candidates.push(lineNo);
      if (fence === null) {
        outsideFences.push(lineNo);
      }
    }
  });
  const guess = outsideFences[index];
  if (guess !== undefined) {
    const next = verified(guess);
    if (next !== null) {
      return next;
    }
  }
  // Slow path (unusual layouts): accept the ONE candidate whose flip is exact.
  const exact = candidates
    .filter((lineNo) => lineNo !== guess)
    .map(verified)
    .filter((next): next is string => next !== null);
  return exact.length === 1 ? exact[0] : null;
}

function flipLine(lines: readonly string[], lineNo: number): string | null {
  const m = CANDIDATE.exec(lines[lineNo]);
  if (!m) {
    return null;
  }
  const mark = m[2] === " " ? "x" : " ";
  const copy = [...lines];
  copy[lineNo] = m[1] + mark + lines[lineNo].slice(m[1].length + 1);
  return copy.join("\n");
}
