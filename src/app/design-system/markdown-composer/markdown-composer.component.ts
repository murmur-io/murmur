import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  computed,
  input,
  output,
  signal,
  viewChild,
} from "@angular/core";
import type { NoteCitation } from "../../core/models";
import { LinkPickerComponent } from "../../features/notes/link-picker/link-picker.component";

/** The formatting ops the ⌘-shortcuts / slash snippets wrap or toggle. */
type FormatOp = "bold" | "italic" | "h1" | "h2" | "h3" | "wikilink";

/**
 * One slash-menu block insertion — mirrors the note-editor's `SlashItem` shape.
 * The `snippet` is inserted at the caret with `$` marking the final caret; the
 * special `linkToNote` entry has NO `snippet` and opens the link picker instead.
 */
interface SlashItem {
  id: string;
  label: string;
  snippet?: string;
}

/**
 * The composer's slash-block catalog — a lean, purpose-built subset of the
 * note-editor's SLASH_ITEMS (no table/callout — those belong to a full document
 * editor, not a recording-time companion jot). `linkToNote` (no `snippet`) opens
 * the SAME link picker the raw `[[` keystroke opens (one shared codepath).
 */
const SLASH_ITEMS: readonly SlashItem[] = [
  { id: "h1", label: "Heading 1", snippet: "# $" },
  { id: "h2", label: "Heading 2", snippet: "## $" },
  { id: "h3", label: "Heading 3", snippet: "### $" },
  { id: "ul", label: "Bullet list", snippet: "- $" },
  { id: "ol", label: "Numbered list", snippet: "1. $" },
  { id: "check", label: "Checklist", snippet: "- [ ] $" },
  { id: "quote", label: "Quote", snippet: "> $" },
  { id: "code", label: "Code block", snippet: "```\n$\n```" },
  { id: "divider", label: "Divider", snippet: "---\n$" },
  // No `snippet` — opens the inline `[[` link picker (Obsidian-style autocomplete).
  { id: "linkToNote", label: "Link to note" },
];

/**
 * Design System — `<mur-markdown-composer>`: a lean, reusable rich-markdown input
 * for the recording surface (and, as a fast-follow, the Notes editor). It mirrors
 * the shipped {@link import('../../features/notes/note-editor/note-editor.component').NoteEditorComponent}'s
 * editing affordances — auto-growing textarea, `/` slash-block menu, `[[` link
 * picker, ⌘B/⌘I/⌘1-3 formatting, list auto-continue, Tab/Shift-Tab indent — but is
 * purpose-built for "type a jot and send it", not a persisted document editor.
 *
 * PRESENTATIONAL + STATELESS-of-persistence: it owns only the in-progress markdown
 * (a private signal) and emits the finished string via `send`; the host owns any
 * persistence. Reuses {@link LinkPickerComponent} (already presentational) for the
 * `[[` autocomplete — the caret STAYS in the textarea (Obsidian parity), this
 * component owns the picker's `query`/`activeIndex` and the textarea splice on pick.
 *
 * Signals-first (private `value` signal, `computed` derivations); DOM work runs
 * synchronously off the live textarea ref (no per-keystroke reflow — CSS grid-mirror
 * auto-grow; no `setTimeout`/`rAF`). The link picker's fetch + positioning + stale
 * guard live inside {@link LinkPickerComponent}.
 */
@Component({
  selector: "mur-markdown-composer",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [LinkPickerComponent],
  templateUrl: "./markdown-composer.component.html",
  styleUrl: "./markdown-composer.component.scss",
})
export class MarkdownComposerComponent {
  /** The empty-state placeholder shown in the textarea. */
  readonly placeholder = input("");
  /** Disable the composer (in flight / no target) — blocks send + input. */
  readonly disabled = input(false);

  /** Emitted on Enter (no Shift, no open menu) — the current markdown, then cleared. */
  readonly send = output<string>();
  /** Emitted on Esc when NO menu is open (host may close the surface / blur). */
  readonly escape = output<void>();

  /** The in-progress markdown (source state) — nothing writable is exposed. */
  private readonly value = signal("");
  /** Read-only view of the current text for the template (auto-grow mirror). */
  readonly text = this.value.asReadonly();

  private readonly bodyArea =
    viewChild<ElementRef<HTMLTextAreaElement>>("bodyArea");

  /** The slash-menu block catalog (template `@for`). */
  protected readonly slashItems = SLASH_ITEMS;

  // --- Slash menu ----------------------------------------------------------
  readonly slashOpen = signal(false);
  readonly slashIndex = signal(0);

  // --- Link picker (Obsidian-style `[[` autocomplete) ----------------------
  /**
   * The open picker's trigger span start (text position right after `[[`, to be
   * replaced with `[[Title]]` on pick) + the caret's viewport anchor rect for
   * positioning. Set by BOTH trigger paths (raw `[[` and the slash "Link to note"
   * entry). `null` when closed.
   */
  readonly linkPickerTrigger = signal<{
    start: number;
    rect: { top: number; left: number; right: number; bottom: number };
  } | null>(null);
  /** Keyboard-highlighted row in the open picker (↑/↓). */
  readonly linkPickerActiveIndex = signal(0);
  /** The picker's live candidates (via its `candidatesChange`) so ↑/↓/Enter act on them. */
  readonly linkPickerCandidates = signal<NoteCitation[]>([]);

  /**
   * The live filter text: the chars typed since the trigger. Tracks `value()`
   * (a signal, re-runs per keystroke) plus the live caret so the picker re-queries.
   */
  readonly linkPickerQuery = computed(() => {
    const trigger = this.linkPickerTrigger();
    const body = this.value();
    const el = this.bodyArea()?.nativeElement;
    if (!trigger || !el) {
      return "";
    }
    const caret = Math.max(trigger.start, el.selectionStart);
    return body.slice(trigger.start, caret);
  });

  /** True when the send button should be enabled (non-empty, not disabled). */
  readonly canSend = computed(
    () => !this.disabled() && this.value().trim().length > 0,
  );

  // ── Input ────────────────────────────────────────────────────────────────

  onInput(event: Event): void {
    const el = event.target as HTMLTextAreaElement;
    this.value.set(el.value);
    this.maybeOpenSlash(el);
    this.maybeOpenLinkPicker(el);
  }

  // ── Submit / keyboard ─────────────────────────────────────────────────────

  onKeydown(event: KeyboardEvent): void {
    const el = this.bodyArea()?.nativeElement;
    if (!el) {
      return;
    }

    // The link picker takes priority over everything when open — its ↑/↓/Enter/Esc
    // navigate/pick/close rather than sending or moving the caret.
    if (this.linkPickerTrigger()) {
      if (this.handleLinkPickerKey(event)) {
        return;
      }
    }

    // Slash-menu navigation takes priority when open.
    if (this.slashOpen()) {
      if (this.handleSlashKey(event)) {
        return;
      }
    }

    const meta = event.metaKey || event.ctrlKey;
    if (meta) {
      switch (event.key.toLowerCase()) {
        case "b":
          event.preventDefault();
          return this.format("bold");
        case "i":
          event.preventDefault();
          return this.format("italic");
        case "1":
          event.preventDefault();
          return this.format("h1");
        case "2":
          event.preventDefault();
          return this.format("h2");
        case "3":
          event.preventDefault();
          return this.format("h3");
      }
    }

    // Enter = send (no menu open, no Shift). Shift+Enter falls through to the list
    // continuation / native newline. The menu-open cases already returned above.
    if (event.key === "Enter" && !event.shiftKey) {
      if (this.handleListEnter(el, event)) {
        return;
      }
      event.preventDefault();
      this.submit();
      return;
    }

    if (event.key === "Tab") {
      if (this.handleListTab(el, event)) {
        return;
      }
    }

    if (event.key === "Escape") {
      if (this.slashOpen()) {
        event.preventDefault();
        this.slashOpen.set(false);
        return;
      }
      if (this.linkPickerTrigger()) {
        event.preventDefault();
        this.closeLinkPicker();
        return;
      }
      // No menu open — bubble the intent to the host (e.g. close the surface).
      event.preventDefault();
      this.escape.emit();
    }
  }

  /** Emit the current markdown (trimmed of trailing whitespace) + clear. Click or Enter. */
  submit(): void {
    if (!this.canSend()) {
      return;
    }
    const markdown = this.value().replace(/\s+$/, "");
    if (markdown.length === 0) {
      return;
    }
    this.send.emit(markdown);
    this.reset();
  }

  /** Blank the composer + close any open menu, syncing the live textarea. */
  private reset(): void {
    this.value.set("");
    this.closeLinkPicker();
    this.slashOpen.set(false);
    const el = this.bodyArea()?.nativeElement;
    if (el) {
      el.value = "";
    }
  }

  // ── Formatting (⌘B/⌘I/⌘1-3, wikilink) ────────────────────────────────────

  private format(op: FormatOp): void {
    const el = this.bodyArea()?.nativeElement;
    if (!el) {
      return;
    }
    const value = el.value;
    const start = el.selectionStart;
    const end = el.selectionEnd;
    const selected = value.slice(start, end);

    switch (op) {
      case "bold": {
        const inner = selected || "bold";
        this.replaceRange(el, start, end, `**${inner}**`, start + 2, start + 2 + inner.length);
        return;
      }
      case "italic": {
        const inner = selected || "italic";
        this.replaceRange(el, start, end, `*${inner}*`, start + 1, start + 1 + inner.length);
        return;
      }
      case "wikilink": {
        const inner = selected || "Note";
        const link = this.wikilinkText(inner);
        this.replaceRange(el, start, end, link, start + 2, start + 2 + inner.length);
        return;
      }
      case "h1":
        return this.applyLinePrefix(el, "# ");
      case "h2":
        return this.applyLinePrefix(el, "## ");
      case "h3":
        return this.applyLinePrefix(el, "### ");
    }
  }

  /** The `[[Title]]` wikilink markdown — the ONE place the string is built. */
  private wikilinkText(title: string): string {
    return `[[${title}]]`;
  }

  /**
   * Toggle a heading prefix on every line the selection touches — replacing any
   * existing heading prefix (mirrors the note-editor's `applyLinePrefix` heading
   * branch; the composer only wires headings to line-prefix formatting).
   */
  private applyLinePrefix(el: HTMLTextAreaElement, prefix: string): void {
    const value = el.value;
    const start = el.selectionStart;
    const end = el.selectionEnd;
    const lineStart = value.lastIndexOf("\n", start - 1) + 1;
    const lineEnd = value.indexOf("\n", end);
    const blockEnd = lineEnd === -1 ? value.length : lineEnd;
    const block = value.slice(lineStart, blockEnd);

    const lines = block.split("\n");
    const allHave = lines.every((l) => l.startsWith(prefix));
    const newLines = lines.map((line) => {
      const stripped = line.replace(/^#{1,6}\s+/, "");
      return allHave ? stripped : prefix + stripped;
    });
    const replacement = newLines.join("\n");
    this.replaceRange(
      el,
      lineStart,
      blockEnd,
      replacement,
      lineStart,
      lineStart + replacement.length,
    );
  }

  /** Splice `replacement` into the textarea, sync the signal, restore the caret. */
  private replaceRange(
    el: HTMLTextAreaElement,
    from: number,
    to: number,
    replacement: string,
    caretStart: number,
    caretEnd: number,
  ): void {
    const value = el.value;
    const next = value.slice(0, from) + replacement + value.slice(to);
    this.value.set(next);
    el.value = next;
    el.setSelectionRange(caretStart, caretEnd);
    el.focus();
  }

  // ── List behaviors (Enter continuation, Tab/Shift-Tab indent) ─────────────

  /**
   * Enter inside a list/checkbox item: auto-continue the marker on the next line
   * (renumbering ordered items), OR exit the list when the current item is empty.
   * Returns true when consumed (so the caller does NOT send).
   */
  private handleListEnter(el: HTMLTextAreaElement, event: KeyboardEvent): boolean {
    const value = el.value;
    const pos = el.selectionStart;
    if (pos !== el.selectionEnd) {
      return false;
    }
    const lineStart = value.lastIndexOf("\n", pos - 1) + 1;
    const line = value.slice(lineStart, pos);
    const m = /^(\s*)(- \[[ xX]\] |[-*] |(\d+)\. )(.*)$/.exec(line);
    if (!m) {
      return false;
    }
    const [, indent, marker, num, rest] = m;
    // Empty item → exit the list (delete the marker, drop to a blank line).
    if (rest.trim() === "") {
      event.preventDefault();
      this.replaceRange(el, lineStart, pos, indent, lineStart + indent.length, lineStart + indent.length);
      return true;
    }
    // Continue: renumber ordered lists, reset a checkbox to unchecked.
    let nextMarker = marker;
    if (num) {
      nextMarker = `${Number(num) + 1}. `;
    } else if (marker.startsWith("- [")) {
      nextMarker = "- [ ] ";
    }
    event.preventDefault();
    const insert = `\n${indent}${nextMarker}`;
    this.replaceRange(el, pos, pos, insert, pos + insert.length, pos + insert.length);
    return true;
  }

  /** Tab / Shift-Tab indent or outdent the current list line. Returns true when consumed. */
  private handleListTab(el: HTMLTextAreaElement, event: KeyboardEvent): boolean {
    const value = el.value;
    const pos = el.selectionStart;
    const nextNl = value.indexOf("\n", pos);
    const lineStart = value.lastIndexOf("\n", pos - 1) + 1;
    const line = value.slice(lineStart, nextNl === -1 ? value.length : nextNl);
    if (!/^\s*(?:- \[[ xX]\] |[-*] |\d+\. )/.test(line)) {
      return false; // not a list line — let Tab do its default (blur/focus).
    }
    event.preventDefault();
    if (event.shiftKey) {
      if (line.startsWith("  ")) {
        this.replaceRange(el, lineStart, lineStart + 2, "", Math.max(lineStart, pos - 2), Math.max(lineStart, pos - 2));
      }
    } else {
      this.replaceRange(el, lineStart, lineStart, "  ", pos + 2, pos + 2);
    }
    return true;
  }

  // ── Slash menu ─────────────────────────────────────────────────────────────

  /** Open the slash menu when `/` was just typed alone at the start of a line. */
  private maybeOpenSlash(el: HTMLTextAreaElement): void {
    const pos = el.selectionStart;
    const value = el.value;
    const lineStart = value.lastIndexOf("\n", pos - 1) + 1;
    const line = value.slice(lineStart, pos);
    if (line === "/") {
      this.slashIndex.set(0);
      this.slashOpen.set(true);
    } else if (!line.startsWith("/") || line.includes(" ")) {
      this.slashOpen.set(false);
    }
  }

  /** Handle ↑/↓/Enter/Esc in the open slash menu. Returns true when consumed. */
  private handleSlashKey(event: KeyboardEvent): boolean {
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        this.slashIndex.update((i) => (i + 1) % SLASH_ITEMS.length);
        return true;
      case "ArrowUp":
        event.preventDefault();
        this.slashIndex.update((i) => (i - 1 + SLASH_ITEMS.length) % SLASH_ITEMS.length);
        return true;
      case "Enter":
        event.preventDefault();
        this.pickSlash(SLASH_ITEMS[this.slashIndex()]);
        return true;
      case "Escape":
        event.preventDefault();
        this.slashOpen.set(false);
        return true;
    }
    return false;
  }

  /**
   * Insert a slash-menu block (replacing the `/` trigger, caret at the `$`), OR for
   * the `linkToNote` entry (no `snippet`) open the SAME link picker the raw `[[`
   * keystroke opens, anchored where the `/` was.
   */
  pickSlash(item: SlashItem): void {
    const el = this.bodyArea()?.nativeElement;
    if (!el) {
      return;
    }
    const value = el.value;
    const pos = el.selectionStart;
    const lineStart = value.lastIndexOf("\n", pos - 1) + 1;
    this.slashOpen.set(false);
    if (item.snippet === undefined) {
      const openBrackets = "[[";
      this.replaceRange(
        el,
        lineStart,
        pos,
        openBrackets,
        lineStart + openBrackets.length,
        lineStart + openBrackets.length,
      );
      this.openLinkPickerAt(el, lineStart + openBrackets.length);
      return;
    }
    const caretMarker = item.snippet.indexOf("$");
    const snippet = item.snippet.replace("$", "");
    const caret = lineStart + (caretMarker === -1 ? snippet.length : caretMarker);
    this.replaceRange(el, lineStart, pos, snippet, caret, caret);
  }

  // ── Link picker (`[[` autocomplete) ───────────────────────────────────────

  /**
   * Open the picker when the two chars before the caret are `[[` (raw keystroke
   * path), or close it when the trigger context is broken (caret backed up before
   * the trigger, a newline / `]]` was typed).
   */
  private maybeOpenLinkPicker(el: HTMLTextAreaElement): void {
    const trigger = this.linkPickerTrigger();
    const pos = el.selectionStart;
    const value = el.value;
    if (!trigger) {
      if (pos >= 2 && value.slice(pos - 2, pos) === "[[") {
        this.openLinkPickerAt(el, pos);
      }
      return;
    }
    const from = trigger.start;
    if (
      pos < from ||
      value.slice(from - 2, from) !== "[[" ||
      value.slice(from, pos).includes("\n") ||
      value.slice(from, pos).includes("]]")
    ) {
      this.closeLinkPicker();
    }
  }

  /** Open the picker anchored at `start` (the text position right after `[[`). */
  private openLinkPickerAt(el: HTMLTextAreaElement, start: number): void {
    const rect = this.caretRect(el, start);
    if (!rect) {
      return;
    }
    this.linkPickerActiveIndex.set(0);
    this.linkPickerCandidates.set([]);
    this.linkPickerTrigger.set({ start, rect });
  }

  /** ↑/↓/Enter/Tab/Esc while the picker is open. Returns true when consumed. */
  private handleLinkPickerKey(event: KeyboardEvent): boolean {
    const rows = this.linkPickerCandidates();
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        if (rows.length > 0) {
          this.linkPickerActiveIndex.update((i) => (i + 1) % rows.length);
        }
        return true;
      case "ArrowUp":
        event.preventDefault();
        if (rows.length > 0) {
          this.linkPickerActiveIndex.update((i) => (i - 1 + rows.length) % rows.length);
        }
        return true;
      case "Enter":
      case "Tab":
        if (rows.length > 0) {
          event.preventDefault();
          this.pickLinkCandidate(rows[this.linkPickerActiveIndex()]);
        }
        return true;
      case "Escape":
        event.preventDefault();
        this.closeLinkPicker();
        return true;
    }
    return false;
  }

  /** The picker's resolved candidates for the current query (drives keyboard nav). */
  onLinkPickerCandidates(rows: NoteCitation[]): void {
    this.linkPickerCandidates.set(rows);
  }

  /**
   * A candidate was picked (click or Enter): replace the trigger span (`[[` +
   * whatever was typed since) with `[[Title]]`, then collapse the caret after it.
   */
  pickLinkCandidate(candidate: NoteCitation): void {
    const el = this.bodyArea()?.nativeElement;
    const trigger = this.linkPickerTrigger();
    if (!el || !trigger) {
      return;
    }
    const link = this.wikilinkText(candidate.title);
    const from = trigger.start - 2; // include the `[[` itself.
    const to = el.selectionStart;
    this.closeLinkPicker();
    this.replaceRange(el, from, to, link, from + link.length, from + link.length);
  }

  /** Close the picker without inserting anything (Esc / caret moved away / picked). */
  closeLinkPicker(): void {
    this.linkPickerTrigger.set(null);
    this.linkPickerCandidates.set([]);
  }

  /**
   * The viewport rect of the caret at text position `pos` — a lightweight measure
   * via a mirror element mimicking the textarea's box, so the picker anchors at the
   * caret. Returns null if unmeasurable. No `setTimeout`/`rAF` — a synchronous read
   * against the live DOM node, mirroring the note-editor's `selectionRect`.
   */
  private caretRect(
    el: HTMLTextAreaElement,
    pos: number,
  ): { top: number; left: number; right: number; bottom: number } | null {
    const doc = el.ownerDocument;
    const mirror = doc.createElement("div");
    const style = getComputedStyle(el);
    for (const prop of [
      "boxSizing",
      "width",
      "paddingTop",
      "paddingRight",
      "paddingBottom",
      "paddingLeft",
      "borderTopWidth",
      "borderRightWidth",
      "borderBottomWidth",
      "borderLeftWidth",
      "fontFamily",
      "fontSize",
      "fontWeight",
      "lineHeight",
      "letterSpacing",
      "textTransform",
      "whiteSpace",
      "wordBreak",
      "overflowWrap",
    ] as const) {
      mirror.style[prop] = style[prop];
    }
    mirror.style.position = "absolute";
    mirror.style.visibility = "hidden";
    mirror.style.whiteSpace = "pre-wrap";
    mirror.style.overflowWrap = "break-word";
    mirror.style.pointerEvents = "none";

    const rect = el.getBoundingClientRect();
    mirror.style.left = `${rect.left}px`;
    mirror.style.top = `${rect.top}px`;

    const before = el.value.slice(0, pos);
    mirror.textContent = before;
    const marker = doc.createElement("span");
    marker.textContent = "​";
    mirror.appendChild(marker);
    doc.body.appendChild(mirror);

    const markerRect = marker.getBoundingClientRect();
    const anchor = {
      top: markerRect.top - el.scrollTop,
      left: markerRect.left - el.scrollLeft,
      right: markerRect.right - el.scrollLeft,
      bottom: markerRect.bottom - el.scrollTop,
    };
    doc.body.removeChild(mirror);
    return anchor;
  }
}
