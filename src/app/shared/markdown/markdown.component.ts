import {
  ChangeDetectionStrategy,
  Component,
  ViewEncapsulation,
  booleanAttribute,
  computed,
  inject,
  input,
} from "@angular/core";
import { Router } from "@angular/router";
import { marked } from "marked";
import DOMPurify from "dompurify";
import { IpcService } from "../../core/ipc.service";
import { ToastService } from "../../services/toast.service";

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
 * - `[[Wikilinks]]` become accent chips carrying a `data-wikilink` attribute (title HTML-escaped
 *   first, then DOMPurify keeps the `<span class="md-wikilink" data-wikilink=…>` because `span`/
 *   `class` are default-allowed and `data-wikilink`/`role`/`tabindex` are added to the allow-list).
 *   A host click/Enter handler resolves the title to a VISIBLE note/meeting (gated server-side)
 *   and navigates, or offers to create the note — so the chips are clickable like Obsidian links.
 * - A stray YAML front-matter block (some models leak one) is stripped defensively.
 *
 * Encapsulation is None with a `.md-body` scope so the styles reach the injected HTML.
 */
@Component({
  selector: "app-markdown",
  changeDetection: ChangeDetectionStrategy.OnPush,
  encapsulation: ViewEncapsulation.None,
  templateUrl: "./markdown.component.html",
  styleUrl: "./markdown.component.scss",
  host: {
    "(click)": "onClick($event)",
    "(keydown.enter)": "onEnter($event)",
  },
})
export class MarkdownComponent {
  private readonly router = inject(Router);
  private readonly ipc = inject(IpcService);
  private readonly toast = inject(ToastService);

  readonly markdown = input<string>("");
  readonly compact = input(false, { transform: booleanAttribute });

  readonly html = computed(() => this.render(this.markdown() ?? ""));

  /** Click anywhere in the rendered markdown — act only when a `.md-wikilink` chip was hit. */
  onClick(ev: Event): void {
    const chip = (ev.target as HTMLElement | null)?.closest?.(".md-wikilink") as
      | HTMLElement
      | null
      | undefined;
    if (!chip) {
      return;
    }
    ev.preventDefault();
    const title = chip.getAttribute("data-wikilink");
    if (title) {
      void this.openWikilink(title);
    }
  }

  /** Enter on a focused wikilink chip (it carries `tabindex="0"`) opens it — keyboard parity. */
  onEnter(ev: Event): void {
    const chip = ev.target as HTMLElement | null;
    if (!chip?.classList?.contains("md-wikilink")) {
      return;
    }
    ev.preventDefault();
    const title = chip.getAttribute("data-wikilink");
    if (title) {
      void this.openWikilink(title);
    }
  }

  private async openWikilink(title: string): Promise<void> {
    try {
      const target = await this.ipc.resolveWikilink(title);
      if (target) {
        void this.router.navigate([
          target.kind === "meeting" ? "/meeting" : "/notes",
          target.id,
        ]);
        return;
      }
      // No such note/meeting (or it is locked) — offer to create it, Obsidian-style.
      this.toast.push(`Notatka „${title}" jeszcze nie istnieje`, "info", 0, {
        label: "Utwórz",
        run: () => void this.createAndOpen(title),
      });
    } catch {
      this.toast.danger(`Nie udało się otworzyć „${title}"`);
    }
  }

  private async createAndOpen(title: string): Promise<void> {
    try {
      const id = await this.ipc.createNote(null, title);
      void this.router.navigate(["/notes", id]);
    } catch {
      this.toast.danger(`Nie udało się utworzyć „${title}"`);
    }
  }

  private render(src: string): string {
    let text = this.stripFrontMatter(src);
    // [[Wikilink]] / [[Wikilink|alias]] → clickable accent chip (marked passes raw HTML through).
    text = text.replace(/\[\[([^\]|]+)(?:\|[^\]]+)?\]\]/g, (_m, t: string) => {
      const safe = this.escapeHtml(t.trim());
      return `<span class="md-wikilink" data-wikilink="${safe}" role="link" tabindex="0">${safe}</span>`;
    });
    const out = marked.parse(text, { async: false, gfm: true, breaks: true });
    const raw = typeof out === "string" ? out : src;
    // Sanitize the parsed HTML before it is bound to [innerHTML]. The source is untrusted
    // (LLM output / transcript), so DOMPurify is the authoritative XSS gate — no
    // bypassSecurityTrustHtml is ever applied. `data-wikilink`/`role`/`tabindex` are explicitly
    // allow-listed so the clickable chip survives sanitization.
    return DOMPurify.sanitize(raw, {
      USE_PROFILES: { html: true },
      ADD_ATTR: ["data-wikilink", "role", "tabindex"],
    });
  }

  private escapeHtml(s: string): string {
    return s.replace(/[&<>"]/g, (c) =>
      c === "&" ? "&amp;" : c === "<" ? "&lt;" : c === ">" ? "&gt;" : "&quot;",
    );
  }

  private stripFrontMatter(src: string): string {
    let s = src.trimStart();
    s = s.replace(/^```ya?ml\s*[\s\S]*?```/i, "").trimStart(); // fenced front-matter
    s = s.replace(/^---\s*\n[\s\S]*?\n---\s*\n?/, "").trimStart(); // --- front-matter ---
    return s;
  }
}
