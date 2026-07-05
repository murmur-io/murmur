import {
  ChangeDetectionStrategy,
  Component,
  ViewEncapsulation,
  booleanAttribute,
  computed,
  input,
} from "@angular/core";
import { marked } from "marked";
import DOMPurify from "dompurify";

/**
 * Renders LLM / markdown text (transcript text AND model output) as beautifully formatted,
 * sanitized HTML.
 *
 * Defense in depth — the markdown source is UNTRUSTED (LLM-generated or speech-to-text of
 * whatever was said), so the parsed HTML is treated as hostile:
 *
 * - `marked` parses the markdown into HTML.
 * - `DOMPurify.sanitize(...)` strips scripts, event handlers, `javascript:`/`data:` script URLs,
 *   `<iframe>`/`<object>`/`<embed>`, and any other XSS vector BEFORE the string ever reaches the
 *   DOM. This is the primary sanitizer; we never call `bypassSecurityTrustHtml`, so Angular's
 *   built-in `[innerHTML]` sanitizer also runs as a second, redundant pass.
 * - `[[Wikilinks]]` become accent chips (the label is HTML-escaped first, then DOMPurify keeps
 *   the `<span class="md-wikilink">` wrapper because `span`/`class` are on its default allow-list).
 * - A stray YAML front-matter block (some models leak one) is stripped defensively.
 *
 * Encapsulation is None with a `.md-body` scope so the styles reach the injected HTML.
 */
@Component({
  selector: "app-markdown",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  encapsulation: ViewEncapsulation.None,
  templateUrl: "./markdown.component.html",
  styleUrl: "./markdown.component.scss",
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
    const raw = typeof out === "string" ? out : src;
    // Sanitize the parsed HTML before it is bound to [innerHTML]. The source is untrusted
    // (LLM output / transcript), so DOMPurify is the authoritative XSS gate — no
    // bypassSecurityTrustHtml is ever applied to this string.
    return DOMPurify.sanitize(raw, { USE_PROFILES: { html: true } });
  }

  private stripFrontMatter(src: string): string {
    let s = src.trimStart();
    s = s.replace(/^```ya?ml\s*[\s\S]*?```/i, "").trimStart(); // fenced front-matter
    s = s.replace(/^---\s*\n[\s\S]*?\n---\s*\n?/, "").trimStart(); // --- front-matter ---
    return s;
  }
}
