import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  ViewEncapsulation,
  booleanAttribute,
  computed,
  inject,
  input,
  output,
} from "@angular/core";
import { marked, type Tokens } from "marked";
import DOMPurify from "dompurify";
import { IpcService } from "../../core/ipc.service";
import type { NoteAttachmentDto } from "../../core/models";
import { TabsService } from "../../core/tabs.service";
import { DocumentPreviewService } from "../../services/document-preview.service";
import { ToastService } from "../../services/toast.service";
import { taskStates, toggleTask } from "./task-list";

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
 * - GFM task lists (`- [ ]` / `- [x]`) render as `<span class="md-task-box" role="checkbox">`,
 *   NOT marked's default `<input type="checkbox">`: Angular's `[innerHTML]` sanitizer drops
 *   `<input>` outright (it is not in its `VALID_ELEMENTS`), which is why checklists used to
 *   render as plain bullets. `role` / `aria-checked` / `tabindex` survive both sanitizers.
 *   With `interactiveTasks` set, clicking (or Space/Enter on) a box emits the whole markdown
 *   with that one task flipped via `tasksChange` — the host owns persistence. The flip is
 *   verified against marked's lexer (`task-list.ts`); an unprovable one is a no-op.
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
    "(keydown.space)": "onSpace($event)",
  },
})
export class MarkdownComponent {
  private readonly ipc = inject(IpcService);
  private readonly tabsService = inject(TabsService);
  private readonly docPreview = inject(DocumentPreviewService);
  private readonly toast = inject(ToastService);
  private readonly host = inject<ElementRef<HTMLElement>>(ElementRef);

  readonly markdown = input<string>("");
  readonly compact = input(false, { transform: booleanAttribute });
  /** Gated attachment DTOs for the current content owner. */
  readonly attachments = input<readonly NoteAttachmentDto[]>([]);
  /** Task checkboxes are clickable and report changes through {@link tasksChange}. */
  readonly interactiveTasks = input(false, { transform: booleanAttribute });
  /** The full markdown with one task toggled (only when {@link interactiveTasks}). */
  readonly tasksChange = output<string>();

  readonly html = computed(() =>
    this.render(this.markdown() ?? "", this.attachments(), this.interactiveTasks()),
  );

  /** Click anywhere in the rendered markdown — act on a task box or a `.md-wikilink` chip. */
  onClick(ev: Event): void {
    if (this.maybeToggleTask(ev)) {
      return;
    }
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
    if (this.maybeToggleTask(ev)) {
      return;
    }
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

  /** Space on a focused task box toggles it (native checkbox parity). */
  onSpace(ev: Event): void {
    this.maybeToggleTask(ev);
  }

  /** Toggle the task whose box was the event target. Returns true when the event was a box. */
  private maybeToggleTask(ev: Event): boolean {
    const box = (ev.target as HTMLElement | null)?.closest?.(".md-task-box") as
      | HTMLElement
      | null
      | undefined;
    if (!box) {
      return false;
    }
    ev.preventDefault();
    if (!this.interactiveTasks()) {
      return true;
    }
    const boxes = Array.from(this.host.nativeElement.querySelectorAll(".md-task-box"));
    const source = this.markdown() ?? "";
    // The DOM index is only meaningful if the source lexes to the same task count.
    if (boxes.length !== taskStates(source).length) {
      return true;
    }
    const next = toggleTask(source, boxes.indexOf(box));
    if (next !== null) {
      this.tasksChange.emit(next);
    }
    return true;
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
        label: "Create",
        run: () => void this.createAndOpen(title),
      });
    } catch {
      this.toast.danger(`Couldn't open “${title}”`);
    }
  }

  private async createAndOpen(title: string): Promise<void> {
    try {
      const id = await this.ipc.createNote(null, title);
      await this.tabsService.openNote(id, title);
    } catch {
      this.toast.danger(`Couldn't create “${title}”`);
    }
  }

  private render(
    src: string,
    attachments: readonly NoteAttachmentDto[],
    interactiveTasks: boolean,
  ): string {
    let text = this.stripFrontMatter(src);
    // Raw HTML image/picture/source tags never reach the DOM. The only renderable
    // image route is a gated opaque attachment id below.
    text = text.replace(
      /<\s*\/?\s*(?:img|picture|source)\b[^>]*>/gi,
      '<span class="md-image-blocked">External image blocked for privacy</span>',
    );
    // [[Wikilink]] / [[Wikilink|alias]] → clickable accent chip (marked passes raw HTML through).
    // The chip's TEXT is the title — no `data-*` attribute (Angular's `[innerHTML]` sanitizer
    // strips those; see the class doc's ROOT-CAUSE FIX note), so `chipTitle()` reads `textContent`.
    // Capture the preceding character so `![[embed]]` is not corrupted on older WKWebView.
    text = text.replace(
      /(^|[^!])\[\[([^\]|]+)(?:\|[^\]]+)?\]\]/g,
      (_m, prefix: string, title: string) => {
        const safe = this.escapeHtml(title.trim());
        return `${prefix}<span class="md-wikilink" role="link" tabindex="0">${safe}</span>`;
      },
    );

    const byId = new Map(
      attachments.map((attachment) => [attachment.id.toLowerCase(), attachment]),
    );
    const renderer = new marked.Renderer();
    renderer.image = (token: Tokens.Image): string =>
      this.renderImage(token, byId);
    renderer.html = ({ text: html }: Tokens.HTML): string =>
      /<\s*\/?\s*(?:img|picture|source)\b/i.test(html)
        ? '<span class="md-image-blocked">External image blocked for privacy</span>'
        : html;
    renderer.checkbox = ({ checked }: Tokens.Checkbox): string => {
      const state = checked ? "true" : "false";
      const label = checked ? "Mark as not done" : "Mark as done";
      return interactiveTasks
        ? `<span class="md-task-box" role="checkbox" aria-checked="${state}" tabindex="0" aria-label="${label}"></span>`
        : `<span class="md-task-box" role="checkbox" aria-checked="${state}" aria-disabled="true"></span>`;
    };
    const baseListitem = renderer.listitem.bind(renderer);
    renderer.listitem = (item: Tokens.ListItem): string => {
      if (!item.task) {
        return baseListitem(item);
      }
      const cls = item.checked ? "task-list-item is-done" : "task-list-item";
      // Wrap the item's own text (everything before a nested list) so a done item can be
      // struck through without also striking its sub-items — `text-decoration` on the
      // `<li>` would propagate into them and cannot be undone by a descendant.
      const split = item.tokens.findIndex((t) => t.type === "list");
      const head = split === -1 ? item.tokens : item.tokens.slice(0, split);
      const tail = split === -1 ? [] : item.tokens.slice(split);
      const parser = renderer.parser;
      return `<li class="${cls}"><span class="md-task-text">${parser.parse(head)}</span>${parser.parse(tail)}</li>\n`;
    };

    const out = marked.parse(text, {
      async: false,
      gfm: true,
      breaks: true,
      renderer,
    });
    const raw = typeof out === "string" ? out : src;
    // Sanitize the parsed HTML before it is bound to [innerHTML]. The source is untrusted
    // (LLM output / transcript), so DOMPurify is the authoritative XSS gate — no
    // bypassSecurityTrustHtml is ever applied. `role`/`tabindex` are explicitly allow-listed
    // (both survive Angular's OWN sanitizer pass too, unlike a custom `data-*` attribute).
    return DOMPurify.sanitize(raw, {
      USE_PROFILES: { html: true },
      ADD_ATTR: ["role", "tabindex", "aria-label", "loading", "decoding"],
    });
  }

  /** Render only a canonical attachment URI whose DTO/data URL passes every check. */
  private renderImage(
    token: Tokens.Image,
    byId: ReadonlyMap<string, NoteAttachmentDto>,
  ): string {
    const match = /^murmur-attachment:\/\/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/i.exec(
      token.href,
    );
    const alt = token.text.trim() || "Image";
    if (!match) {
      return `<span class="md-image-blocked">${this.escapeHtml(alt)} — external image blocked for privacy</span>`;
    }
    const attachment = byId.get(match[1].toLowerCase());
    if (!attachment || !this.isSafeDataUrl(attachment)) {
      return `<span class="md-image-unavailable" role="status">${this.escapeHtml(alt)} — image unavailable</span>`;
    }
    const safeAlt = this.escapeHtml(alt);
    const caption = (token.title?.trim() || alt).trim();
    const safeCaption = this.escapeHtml(caption);
    return `<span class="md-attachment" role="figure" aria-label="${safeCaption}"><img src="${attachment.dataUrl}" alt="${safeAlt}" loading="lazy" decoding="async"><span class="md-attachment-caption">${safeCaption}</span></span>`;
  }

  /** Data URLs are accepted only for a verified raster MIME matching the DTO. */
  private isSafeDataUrl(attachment: NoteAttachmentDto): boolean {
    const mimeType = attachment.mimeType.toLowerCase();
    if (!["image/png", "image/jpeg", "image/webp"].includes(mimeType)) {
      return false;
    }
    const prefix = `data:${mimeType};base64,`;
    if (!attachment.dataUrl.startsWith(prefix)) {
      return false;
    }
    const payload = attachment.dataUrl.slice(prefix.length);
    return payload.length > 0 && /^[a-z0-9+/]+={0,2}$/i.test(payload);
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
