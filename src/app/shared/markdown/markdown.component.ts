import {
  ChangeDetectionStrategy,
  Component,
  ViewEncapsulation,
  booleanAttribute,
  computed,
  inject,
  input,
} from "@angular/core";
import { marked } from "marked";
import DOMPurify from "dompurify";
import { IpcService } from "../../core/ipc.service";
import { TabsService } from "../../core/tabs.service";
import { DocumentPreviewService } from "../../services/document-preview.service";
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
 * - `[[Wikilinks]]` become accent chips (`<span class="md-wikilink" role="link" tabindex="0">`).
 *   ROOT-CAUSE FIX (2026-07-15, found while fixing the "clicking a wikilink pill does nothing"
 *   bug): the chip used to carry the title in a `data-wikilink="…"` attribute, but Angular's OWN
 *   `[innerHTML]` sanitizer (the second pass above) SILENTLY STRIPS unrecognized `data-*`
 *   attributes even when DOMPurify's `ADD_ATTR` allow-lists them — DOMPurify only gates what
 *   REACHES `[innerHTML]`, it doesn't control what Angular's sanitizer then does with it. So
 *   `chip.getAttribute("data-wikilink")` always read `null` and the click silently no-opped —
 *   independent of (and deeper than) the separate `router.navigate`-vs-`TabsService` bug fixed
 *   alongside it. The fix: read the title from the chip's own `textContent` instead (the chip's
 *   visible text ALREADY IS the escaped title — no extra attribute needed, so there's nothing
 *   left for a sanitizer to strip).
 *   A host click/Enter handler resolves the title to a VISIBLE note/meeting/org-item (gated
 *   server-side; the org leg added 2026-07-15) and opens it through {@link TabsService} (not a
 *   raw `router.navigate` — same sibling-function
 *   fix as `NoteBrainPopoverComponent.openCitation`, 2026-07-12: a plain navigate never registers
 *   the resulting view with the tab strip, so a wikilink click opened an orphaned, untracked view
 *   and looked like a no-op), or offers to create the note — so the chips are clickable like
 *   Obsidian links.
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
  private readonly ipc = inject(IpcService);
  private readonly tabsService = inject(TabsService);
  private readonly docPreview = inject(DocumentPreviewService);
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
    const title = this.chipTitle(chip);
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
    const title = this.chipTitle(chip);
    if (title) {
      void this.openWikilink(title);
    }
  }

  /**
   * The wikilink title for a `.md-wikilink` chip — its OWN `textContent`, not a
   * `data-*` attribute (see the class doc: Angular's `[innerHTML]` sanitizer
   * silently strips those regardless of DOMPurify's allow-list, which is what
   * made the chip look permanently unclickable).
   */
  private chipTitle(chip: HTMLElement): string | null {
    const title = chip.textContent?.trim();
    return title ? title : null;
  }

  private async openWikilink(title: string): Promise<void> {
    try {
      const target = await this.ipc.resolveWikilink(title);
      if (target) {
        // Route through TabsService (not a raw `router.navigate`) so the opened
        // note/meeting/org-item is a TRACKED TAB, matching every other open path in
        // the app. "org" (2026-07-15) opens the read-only Shared Brain viewer — never
        // offer to CREATE a note when an org item already matched the title.
        // "document" (a brain-ingested `documents` row, e.g. a PDF) has NO route —
        // open the app-wide read-only preview modal (gated `getDocument`), never a tab.
        if (target.kind === "meeting") {
          await this.tabsService.openMeeting(target.id, title);
        } else if (target.kind === "org") {
          await this.tabsService.openOrgItem(target.id, title);
        } else if (target.kind === "document") {
          this.docPreview.open({ id: target.id, name: title, kind: "document" });
        } else {
          await this.tabsService.openNote(target.id, title);
        }
        return;
      }
      // No such note/meeting/org-item (or it is locked) — offer to create it, Obsidian-style.
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
      await this.tabsService.openNote(id, title);
    } catch {
      this.toast.danger(`Nie udało się utworzyć „${title}"`);
    }
  }

  private render(src: string): string {
    let text = this.stripFrontMatter(src);
    // [[Wikilink]] / [[Wikilink|alias]] → clickable accent chip (marked passes raw HTML through).
    // The chip's TEXT is the title — no `data-*` attribute (Angular's `[innerHTML]` sanitizer
    // strips those; see the class doc's ROOT-CAUSE FIX note), so `chipTitle()` reads `textContent`.
    text = text.replace(/\[\[([^\]|]+)(?:\|[^\]]+)?\]\]/g, (_m, t: string) => {
      const safe = this.escapeHtml(t.trim());
      return `<span class="md-wikilink" role="link" tabindex="0">${safe}</span>`;
    });
    const out = marked.parse(text, { async: false, gfm: true, breaks: true });
    const raw = typeof out === "string" ? out : src;
    // Sanitize the parsed HTML before it is bound to [innerHTML]. The source is untrusted
    // (LLM output / transcript), so DOMPurify is the authoritative XSS gate — no
    // bypassSecurityTrustHtml is ever applied. `role`/`tabindex` are explicitly allow-listed
    // (both survive Angular's OWN sanitizer pass too, unlike a custom `data-*` attribute).
    return DOMPurify.sanitize(raw, {
      USE_PROFILES: { html: true },
      ADD_ATTR: ["role", "tabindex"],
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
