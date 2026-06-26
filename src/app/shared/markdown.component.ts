import {
  ChangeDetectionStrategy,
  Component,
  ViewEncapsulation,
  booleanAttribute,
  computed,
  input,
} from "@angular/core";
import { marked } from "marked";

/**
 * Renders LLM / markdown text as beautifully formatted, sanitized HTML.
 *
 * - `marked` parses the markdown; Angular's default `[innerHTML]` sanitizer strips anything
 *   unsafe (scripts, event handlers, javascript: URLs), so no `bypassSecurityTrust` is used.
 * - `[[Wikilinks]]` become accent chips.
 * - A stray YAML front-matter block (some models leak one) is stripped defensively.
 *
 * Encapsulation is None with a `.md-body` scope so the styles reach the injected HTML.
 */
@Component({
  selector: "app-markdown",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  encapsulation: ViewEncapsulation.None,
  template: `<div
    class="md-body"
    [class.md-compact]="compact()"
    [innerHTML]="html()"
  ></div>`,
  styles: [
    `
      .md-body {
        color: var(--text-primary);
        font-size: 15px;
        line-height: 1.66;
        overflow-wrap: anywhere;
      }
      .md-body.md-compact {
        font-size: 14px;
        line-height: 1.6;
      }
      .md-body > :first-child {
        margin-top: 0;
      }
      .md-body > :last-child {
        margin-bottom: 0;
      }
      .md-body h1,
      .md-body h2,
      .md-body h3,
      .md-body h4 {
        margin: 1.35em 0 0.5em;
        line-height: 1.25;
        font-weight: 700;
        letter-spacing: -0.01em;
        color: var(--text-primary);
      }
      .md-body h1 {
        font-size: 1.42em;
      }
      .md-body h2 {
        font-size: 1.16em;
        display: flex;
        align-items: center;
        gap: var(--space-2);
        padding-bottom: 0.35em;
        border-bottom: 1px solid var(--border-subtle);
      }
      .md-body h2::before {
        content: "";
        flex: 0 0 auto;
        width: 4px;
        height: 1.05em;
        border-radius: var(--radius-pill);
        background: var(--accent-gradient);
      }
      .md-body h3 {
        font-size: 1.04em;
      }
      .md-body p {
        margin: 0.6em 0;
      }
      .md-body strong {
        color: var(--text-primary);
        font-weight: 700;
      }
      .md-body em {
        color: var(--text-secondary);
      }
      .md-body a {
        color: var(--accent-hover);
        text-decoration: none;
        border-bottom: 1px solid var(--accent-soft);
      }
      .md-body a:hover {
        border-bottom-color: var(--accent-hover);
      }
      .md-body ul,
      .md-body ol {
        margin: 0.55em 0;
        padding-left: 1.35em;
      }
      .md-body li {
        margin: 0.28em 0;
      }
      .md-body ul li::marker {
        color: var(--accent);
      }
      .md-body ol li::marker {
        color: var(--text-muted);
        font-variant-numeric: tabular-nums;
      }
      .md-body li.task-list-item {
        list-style: none;
        margin-left: -1.2em;
      }
      .md-body input[type="checkbox"] {
        accent-color: var(--accent);
        margin-right: 0.45em;
        transform: translateY(1px);
      }
      .md-body blockquote {
        margin: 0.8em 0;
        padding: 0.3em 0 0.3em 1em;
        border-left: 3px solid var(--accent-soft);
        color: var(--text-secondary);
        font-style: italic;
      }
      .md-body code {
        font-family: var(--font-mono);
        font-size: 0.86em;
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
        padding: 0.08em 0.4em;
        border-radius: var(--radius-sm);
      }
      .md-body pre {
        margin: 0.8em 0;
        padding: var(--space-4);
        background: var(--surface-solid);
        border: 1px solid var(--border);
        border-radius: var(--radius-md);
        overflow-x: auto;
      }
      .md-body pre code {
        background: none;
        border: none;
        padding: 0;
        font-size: 0.84em;
      }
      .md-body hr {
        border: none;
        border-top: 1px solid var(--border-subtle);
        margin: 1.2em 0;
      }
      .md-body table {
        border-collapse: collapse;
        margin: 0.8em 0;
        width: 100%;
        font-size: 0.92em;
      }
      .md-body th,
      .md-body td {
        border: 1px solid var(--border-subtle);
        padding: 0.45em 0.7em;
        text-align: left;
      }
      .md-body th {
        background: var(--surface-input);
        font-weight: 600;
      }
      .md-wikilink {
        display: inline-flex;
        align-items: center;
        gap: 4px;
        padding: 0.04em 0.5em;
        margin: 0 1px;
        background: var(--accent-soft);
        color: var(--accent-hover);
        border-radius: var(--radius-pill);
        font-size: 0.9em;
        font-weight: 500;
        white-space: nowrap;
      }
      .md-wikilink::before {
        content: "🔗";
        font-size: 0.82em;
      }
    `,
  ],
})
export class MarkdownComponent {
  readonly markdown = input<string>("");
  readonly compact = input(false, { transform: booleanAttribute });

  readonly html = computed(() => this.render(this.markdown() ?? ""));

  private render(src: string): string {
    let text = this.stripFrontMatter(src);
    // [[Wikilink]] / [[Wikilink|alias]] → safe accent chip (marked passes raw HTML through).
    text = text.replace(/\[\[([^\]|]+)(?:\|[^\]]+)?\]\]/g, (_m, t: string) => {
      const label = t.trim().replace(/[<>]/g, "");
      return `<span class="md-wikilink">${label}</span>`;
    });
    const out = marked.parse(text, { async: false, gfm: true, breaks: true });
    return typeof out === "string" ? out : src;
  }

  private stripFrontMatter(src: string): string {
    let s = src.trimStart();
    s = s.replace(/^```ya?ml\s*[\s\S]*?```/i, "").trimStart(); // fenced front-matter
    s = s.replace(/^---\s*\n[\s\S]*?\n---\s*\n?/, "").trimStart(); // --- front-matter ---
    return s;
  }
}
