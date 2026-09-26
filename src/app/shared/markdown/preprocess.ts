/**
 * The text transforms `MarkdownComponent` applies BEFORE `marked` parses the
 * source. Kept pure and separate so the task-list toggle can lex exactly the
 * view the user is looking at (see `task-list.ts`): a transform that shifts or
 * swallows lines (a stripped front-matter block, a multi-line `<img …>`) would
 * otherwise map the Nth rendered checkbox to the wrong source line.
 */
export function preprocessMarkdown(src: string): string {
  let text = stripFrontMatter(src);
  // Raw HTML image/picture/source tags never reach the DOM. The only renderable
  // image route is a gated opaque attachment id (see MarkdownComponent.renderImage).
  text = text.replace(
    /<\s*\/?\s*(?:img|picture|source)\b[^>]*>/gi,
    '<span class="md-image-blocked">External image blocked for privacy</span>',
  );
  // [[Wikilink]] / [[Wikilink|alias]] → clickable accent chip (marked passes raw HTML through).
  // The chip's TEXT is the title — no `data-*` attribute (Angular's `[innerHTML]` sanitizer
  // strips those; see MarkdownComponent's ROOT-CAUSE FIX note), so `chipTitle()` reads `textContent`.
  // Capture the preceding character so `![[embed]]` is not corrupted on older WKWebView.
  text = text.replace(
    /(^|[^!])\[\[([^\]|]+)(?:\|[^\]]+)?\]\]/g,
    (_m, prefix: string, title: string) => {
      const safe = escapeHtml(title.trim());
      return `${prefix}<span class="md-wikilink" role="link" tabindex="0">${safe}</span>`;
    },
  );
  return text;
}

export function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) =>
    c === "&" ? "&amp;" : c === "<" ? "&lt;" : c === ">" ? "&gt;" : "&quot;",
  );
}

function stripFrontMatter(src: string): string {
  let s = src.trimStart();
  s = s.replace(/^```ya?ml\s*[\s\S]*?```/i, "").trimStart(); // fenced front-matter
  s = s.replace(/^---\s*\n[\s\S]*?\n---\s*\n?/, "").trimStart(); // --- front-matter ---
  return s;
}
