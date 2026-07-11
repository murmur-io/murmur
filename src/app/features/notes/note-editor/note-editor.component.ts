import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  effect,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { ActivatedRoute, Router } from "@angular/router";
import { map } from "rxjs";
import { IpcService } from "../../../core/ipc.service";
import { NavHistoryService } from "../../../core/nav-history.service";
import type { AppConfigDto, NoteDoc, NoteFolder } from "../../../core/models";
import { DebounceService } from "../../../services/debounce.service";
import { FoldersService } from "../../../services/folders.service";
import { NotesService } from "../../../services/notes.service";
import { ToastService } from "../../../services/toast.service";
import { MarkdownComponent } from "../../../shared/markdown/markdown.component";
import {
  NoteBrainPopoverComponent,
  type AcceptedEdit,
  type PopoverSelection,
} from "../note-brain-popover/note-brain-popover.component";
import { NoteSharePanelComponent } from "../note-share-panel/note-share-panel.component";
import { NoteSelectionToolbarComponent } from "../note-selection-toolbar/note-selection-toolbar.component";
import { parseDoc, serializeDoc } from "./front-matter";

/** The autosave indicator state. */
type SaveState = "idle" | "saving" | "saved" | "error";

/** The formatting-toolbar operations that wrap/toggle markdown around a selection. */
export type FormatOp =
  | "h1"
  | "h2"
  | "h3"
  | "bold"
  | "italic"
  | "strike"
  | "ul"
  | "ol"
  | "check"
  | "quote"
  | "code"
  | "codeblock"
  | "link"
  | "wikilink"
  | "divider";

/** One slash-menu block insertion. */
interface SlashItem {
  id: string;
  label: string;
  /** The markdown snippet inserted at the caret (with `$` marking the caret). */
  snippet: string;
}

const SLASH_ITEMS: readonly SlashItem[] = [
  { id: "h1", label: "Heading 1", snippet: "# $" },
  { id: "h2", label: "Heading 2", snippet: "## $" },
  { id: "h3", label: "Heading 3", snippet: "### $" },
  { id: "ul", label: "Bullet list", snippet: "- $" },
  { id: "ol", label: "Numbered list", snippet: "1. $" },
  { id: "check", label: "Checklist", snippet: "- [ ] $" },
  { id: "quote", label: "Quote", snippet: "> $" },
  { id: "code", label: "Code block", snippet: "```\n$\n```" },
  {
    id: "table",
    label: "Table",
    snippet: "| Column | Column |\n| --- | --- |\n| $ |  |",
  },
  { id: "divider", label: "Divider", snippet: "---\n$" },
  { id: "callout", label: "Callout", snippet: "> [!note]\n> $" },
];

/** Up to this many chars of before/after context ship with an assistant request. */
const CONTEXT_CHARS = 500;
const AUTOSAVE_MS = 600;

/**
 * The full note editor (FP2): a centered document with a borderless title, a
 * collapsible Obsidian-style properties bar (tags + key/value), a source-of-truth
 * `<textarea>` body with a formatting toolbar + markdown keyboard behaviors + a
 * slash `/` block menu, an Edit/Preview toggle (preview is a cached `computed`),
 * debounced autosave, and a sticky header (folder breadcrumb + Move, Preview
 * toggle, Share, ⋯ menu). A sealed-not-unlocked note shows the lock gate.
 *
 * Selecting text in the body floats a compact formatting bubble
 * ({@link NoteSelectionToolbarComponent}); its "Ask Brain" button opens the
 * {@link NoteBrainPopoverComponent} (FP3) over that same selection.
 *
 * State is signals; IPC lands in signals (never a subscribe-into-a-field);
 * DOM-after-render work is `afterNextRender({injector})` (no setTimeout in the
 * component — autosave debounce is the sanctioned {@link DebounceService}).
 */
@Component({
  selector: "app-note-editor",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    MarkdownComponent,
    NoteBrainPopoverComponent,
    NoteSelectionToolbarComponent,
    NoteSharePanelComponent,
  ],
  templateUrl: "./note-editor.component.html",
  styleUrl: "./note-editor.component.scss",
})
export class NoteEditorComponent {
  private readonly ipc = inject(IpcService);
  private readonly notes = inject(NotesService);
  private readonly folders = inject(FoldersService);
  private readonly debounce = inject(DebounceService);
  private readonly toast = inject(ToastService);
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);
  private readonly injector = inject(Injector);
  private readonly destroyRef = inject(DestroyRef);

  /** Drill-down back navigation ("← Notes"). */
  readonly nav = inject(NavHistoryService);

  /** The slash-menu block catalog. */
  protected readonly slashItems = SLASH_ITEMS;

  /**
   * The route `:id`, tracked so a same-route navigation re-fetches even though
   * the RouteReuseStrategy keeps this instance. `null` on `/notes/new`.
   */
  private readonly routeId = toSignal(
    this.route.paramMap.pipe(map((p) => p.get("id"))),
    { initialValue: this.route.snapshot.paramMap.get("id") },
  );

  /** The loaded note doc (identity + lock/shared flags + exported path). */
  readonly note = signal<NoteDoc | null>(null);
  readonly loading = signal(true);
  readonly error = signal<string | null>(null);

  // --- Editable surfaces (source state) -----------------------------------
  /** The title (borderless input). */
  readonly title = signal("");
  /** The BODY markdown (front-matter stripped — the textarea's value). */
  readonly body = signal("");
  /** Front-matter tags. */
  readonly tags = signal<string[]>([]);
  /** Front-matter properties (key → value), excluding tags. */
  readonly properties = signal<Record<string, string>>({});

  // --- View state ----------------------------------------------------------
  /** Edit vs Preview. */
  readonly preview = signal(false);
  /** Properties bar expanded. */
  readonly propsOpen = signal(false);
  /** The autosave indicator. */
  readonly saveState = signal<SaveState>("idle");
  /** The tag-input draft. */
  readonly tagDraft = signal("");
  /** Which floating menu (if any) is open in the header. */
  readonly menu = signal<"none" | "move" | "more">("none");
  /** Add-property form open. */
  readonly addingProp = signal(false);
  readonly propKeyDraft = signal("");
  readonly propValDraft = signal("");
  /** Two-step delete confirm. */
  readonly confirmingDelete = signal(false);
  /** True while the Share modal is open over the document. */
  readonly shareOpen = signal(false);

  /** The note-kind folders (for the Move menu + breadcrumb). */
  readonly noteFolders = signal<NoteFolder[]>([]);
  /** True while a folder unlock is in flight (lock gate). */
  readonly unlocking = signal(false);

  // --- Slash menu ----------------------------------------------------------
  readonly slashOpen = signal(false);
  readonly slashIndex = signal(0);

  // --- Selection toolbar + Brain popover -----------------------------------
  /**
   * The live body selection. Drives the floating formatting toolbar (bubble); the
   * Brain popover mounts over the SAME selection only once {@link brainOpen} flips
   * (the AI button), so selecting text no longer auto-pops the modal.
   */
  readonly sel = signal<PopoverSelection | null>(null);
  /** True once the AI button opened the Brain popover for the current selection. */
  readonly brainOpen = signal(false);

  private readonly titleInput =
    viewChild<ElementRef<HTMLInputElement>>("titleInput");
  private readonly bodyArea =
    viewChild<ElementRef<HTMLTextAreaElement>>("bodyArea");
  private readonly tagInput =
    viewChild<ElementRef<HTMLInputElement>>("tagInput");

  /**
   * Monotonic request token — a late `getNote` reply for a superseded id is
   * dropped (stale-result guard, trap #4).
   */
  private requestSeq = 0;
  /** True while applying the loaded doc into the edit signals (suppress autosave). */
  private hydrating = false;
  /**
   * True when there are edits not yet persisted through the FULL path (re-index +
   * vault export). Cheap autosaves keep the DB text current but leave this set; the
   * full save runs once on a natural boundary (Preview / editor close / retry) so the
   * e5 re-embed never fires per keystroke-pause. Cleared after a successful full save.
   */
  private dirtyFull = false;

  // --- Single-writer save queue --------------------------------------------
  /**
   * The persistence chain — EVERY backend write for this note (cheap
   * `saveNoteText` and full `updateNoteDoc`) is appended here, so writes reach
   * the backend strictly in order. Without it the debounced cheap save and a
   * boundary full save could be concurrently in flight with different payloads,
   * and a stale older write could land after a newer one.
   */
  private saveChain: Promise<void> = Promise.resolve();
  /**
   * The single coalesced pending save (latest-wins): while a save is in flight,
   * new requests only escalate/refresh this slot — they never stack. `"full"`
   * supersedes `"text"` (the full save persists a superset). The payload is
   * snapshotted from the signals when the queued save RUNS, so the newest text
   * always wins.
   */
  private pendingSave: "text" | "full" | null = null;

  /** The rendered preview HTML source — a cached `computed` off title + doc markdown. */
  readonly previewMarkdown = computed(() =>
    serializeDoc(this.tags(), this.properties(), this.body()),
  );

  /** The folder this note lives in (for the breadcrumb). */
  readonly currentFolder = computed<NoteFolder | null>(() => {
    const id = this.note()?.folderId;
    if (!id) {
      return null;
    }
    return this.noteFolders().find((f) => f.id === id) ?? null;
  });

  /** Human breadcrumb: the folder's display path (Notes/… ) or "Notes". */
  readonly breadcrumb = computed(() => {
    const folder = this.currentFolder();
    return folder ? folder.name : "Notes";
  });

  /** The tags already used across notes — autocomplete suggestions for the tag input. */
  readonly tagSuggestions = computed(() => {
    const used = new Set<string>();
    for (const n of this.notes.notes()) {
      for (const t of n.tags) {
        used.add(t);
      }
    }
    // Drop tags already on this note + non-matching against the draft.
    const own = new Set(this.tags());
    const draft = this.tagDraft().trim().toLowerCase();
    return [...used]
      .filter((t) => !own.has(t) && (draft === "" || t.includes(draft)))
      .slice(0, 6);
  });

  /** Property rows for the properties bar (stable order). */
  readonly propertyRows = computed(() =>
    Object.entries(this.properties()).map(([key, value]) => ({ key, value })),
  );

  /**
   * The app config (loaded best-effort in the constructor), the source for the
   * Settings note-assistant toggles. Null until the first load / on failure.
   */
  private readonly config = signal<AppConfigDto | null>(null);

  /**
   * Which selection-assistant actions the user has ENABLED in Settings
   * (`noteAssistRefine`/`-Shorten`/`-Enhance`). All default TRUE — an ABSENT
   * value (undefined) is treated as TRUE (the same contract the settings block +
   * backend use), and a null config (not yet loaded / load failed) also defaults
   * every action ON so the popover works before the config lands. The popover
   * hides a disabled action's button; the backend is still the real gate
   * (a disabled action refuses `Unavailable`).
   */
  readonly assistToggles = computed(() => {
    const cfg = this.config();
    return {
      refine: cfg?.noteAssistRefine ?? true,
      shorten: cfg?.noteAssistShorten ?? true,
      enhance: cfg?.noteAssistEnhance ?? true,
    };
  });

  /**
   * Whether ANY note-assistant action is enabled — gates the bubble's "Ask Brain"
   * button (formatting is always available; only the AI entry point is hidden when
   * the user disabled every action in Settings).
   */
  readonly anyAssistEnabled = computed(() => {
    const t = this.assistToggles();
    return t.refine || t.shorten || t.enhance;
  });

  /**
   * Resolve the route id on every change: `/notes/new` creates + replaces the
   * URL; otherwise fetch. Legitimate signal-writing effect (async IPC + stale
   * guard, T1).
   */
  private readonly _load = effect(() => {
    const id = this.routeId();
    const seq = ++this.requestSeq;
    if (!id) {
      void this.createAndOpen(seq);
      return;
    }
    void this.fetchNote(id, seq);
  });

  constructor() {
    // Warm the note-folder list (Move menu + breadcrumb) + the note list (tag
    // autocomplete) + the config (note-assistant toggles). Best-effort; a
    // failure just means no suggestions / every assistant action defaults ON.
    void this.loadFolders();
    void this.notes.loadNotes(null);
    void this.loadConfig();
    // On teardown (route-leave) run the FULL save ONCE if there are unindexed edits —
    // fire-and-forget so navigation is instant; the backend re-indexes + re-exports in
    // the background. Nothing is lost either way (cheap autosaves already persisted the
    // text). No pending edits ⇒ no needless re-embed on open-then-close.
    this.destroyRef.onDestroy(() => {
      if (this.dirtyFull) {
        this.flushFull();
      }
    });
  }

  /**
   * Load the app config into a signal so `assistToggles` can gate the selection
   * popover's actions by the Settings note-assistant flags. Best-effort — mirrors
   * how `detail.component.ts` reads config (a failure leaves `config` null, and
   * every action defaults ON, matching the undefined-⇒-TRUE contract).
   */
  private async loadConfig(): Promise<void> {
    try {
      this.config.set(await this.ipc.getConfig());
    } catch {
      this.config.set(null);
    }
  }

  private async loadFolders(): Promise<void> {
    try {
      this.noteFolders.set(await this.ipc.listNoteFolders());
    } catch {
      this.noteFolders.set([]);
    }
  }

  /** Create an empty note (`/notes/new`) and replace the URL with its id. */
  private async createAndOpen(seq: number): Promise<void> {
    this.loading.set(true);
    this.error.set(null);
    try {
      const id = await this.notes.create(null, "Untitled");
      if (seq !== this.requestSeq) {
        return;
      }
      await this.router.navigate(["/notes", id], { replaceUrl: true });
    } catch (e) {
      if (seq === this.requestSeq) {
        this.error.set(String(e));
        this.loading.set(false);
      }
    }
  }

  /** Fetch one note, hydrate the edit signals, dropping a stale reply. */
  private async fetchNote(id: string, seq: number): Promise<void> {
    this.loading.set(true);
    this.error.set(null);
    try {
      const doc = await this.ipc.getNote(id);
      if (seq !== this.requestSeq) {
        return;
      }
      this.hydrate(doc);
    } catch (e) {
      if (seq === this.requestSeq) {
        this.error.set(String(e));
        this.note.set(null);
      }
    } finally {
      if (seq === this.requestSeq) {
        this.loading.set(false);
      }
    }
  }

  /** Apply a loaded/reconciled doc into the edit signals (no autosave feedback). */
  private hydrate(doc: NoteDoc): void {
    this.hydrating = true;
    this.note.set(doc);
    this.title.set(doc.title === "🔒 Locked" ? "" : doc.title);
    // The DTO carries parsed tags/properties, but the source-of-truth body is
    // derived by stripping the front-matter from the FULL markdown so a save
    // round-trips exactly. Prefer the DTO's structured fields when present.
    const parsed = parseDoc(doc.markdown);
    this.body.set(parsed.body);
    this.tags.set(doc.tags.length ? [...doc.tags] : parsed.tags);
    this.properties.set(
      Object.keys(doc.properties).length ? { ...doc.properties } : parsed.properties,
    );
    this.propsOpen.set(this.tags().length > 0 || this.propertyRows().length > 0);
    this.saveState.set("idle");
    this.dirtyFull = false;
    this.hydrating = false;
  }

  // ── Title ────────────────────────────────────────────────────────────────

  onTitleInput(event: Event): void {
    this.title.set((event.target as HTMLInputElement).value);
    this.scheduleSave();
  }

  // ── Body ───────────────────────────────────────────────────────────────

  onBodyInput(event: Event): void {
    const el = event.target as HTMLTextAreaElement;
    this.body.set(el.value);
    this.autoGrow();
    this.maybeOpenSlash(el);
    this.scheduleSave();
  }

  /**
   * The textarea auto-grows via CSS (the `.body-grow` grid mirror sizes the row to
   * the content) — so there is NO per-keystroke JS layout reflow (the old
   * `height:auto` → read `scrollHeight` → set `height` thrash was the typing-lag
   * culprit). Kept as a no-op so the historical call sites need no change.
   */
  private autoGrow(): void {
    /* CSS `.body-grow` mirror handles sizing — intentionally no JS reflow. */
  }

  // ── Autosave ─────────────────────────────────────────────────────────────

  /**
   * Debounced autosave — the CHEAP path (`saveNoteText`: persist title + markdown
   * only, NO re-index / no vault export) so typing stays smooth even with the embed
   * model loaded. Marks the note `dirtyFull` so the deferred full save (re-index +
   * export) runs once on the next boundary. No-op while hydrating or locked.
   */
  private scheduleSave(): void {
    if (this.hydrating || this.note()?.locked) {
      return;
    }
    this.dirtyFull = true;
    this.saveState.set("saving");
    this.debounce.schedule(
      "note-editor-save",
      () => void this.queueSave("text"),
      AUTOSAVE_MS,
    );
  }

  /**
   * Enqueue a save on the single-writer chain. If a save is already QUEUED (not
   * yet started), the request only escalates/refreshes the pending slot —
   * latest-payload-wins, since the payload is read from the signals when the
   * save runs. If a save is IN FLIGHT, the queued one starts only after it
   * settles, so writes reach the backend in order. Returns a promise that
   * resolves once this request's save has landed (the chain never rejects —
   * both save paths handle their own errors via `saveState`/toast).
   */
  private queueSave(kind: "text" | "full"): Promise<void> {
    const alreadyQueued = this.pendingSave !== null;
    this.pendingSave =
      this.pendingSave === "full" || kind === "full" ? "full" : "text";
    if (!alreadyQueued) {
      this.saveChain = this.saveChain
        .then(() => this.runPendingSave())
        // Defensive: a rejected link would wedge the chain forever; both save
        // paths already catch, so this only guards the truly unexpected.
        .catch(() => undefined);
    }
    return this.saveChain;
  }

  /** Execute (and clear) the coalesced pending save. Runs on the chain only. */
  private async runPendingSave(): Promise<void> {
    const kind = this.pendingSave;
    this.pendingSave = null;
    if (kind === "full") {
      await this.saveFull();
    } else if (kind === "text") {
      await this.saveText();
    }
  }

  /** The current title + full markdown (front-matter re-emitted). */
  private currentPayload(): { title: string; markdown: string } {
    return {
      title: this.title().trim() || "Untitled",
      markdown: serializeDoc(this.tags(), this.properties(), this.body()),
    };
  }

  /**
   * CHEAP persist — text only (no re-index, no export). Used by the frequent
   * autosave + blur so typing never triggers the e5 re-embed. The DB text is
   * canonical, so nothing is lost; the brain index catches up on the next full save.
   * Runs ONLY via {@link runPendingSave} on the single-writer chain.
   */
  private async saveText(): Promise<void> {
    const doc = this.note();
    if (!doc || doc.locked) {
      return;
    }
    const { title, markdown } = this.currentPayload();
    this.saveState.set("saving");
    try {
      const updatedAt = await this.ipc.saveNoteText(doc.id, title, markdown);
      this.note.update((cur) => (cur ? { ...cur, updatedAt } : cur));
      this.saveState.set("saved");
    } catch (e) {
      this.saveState.set("error");
      if (String(e).includes("Locked")) {
        this.toast.danger("This note is locked — unlock its folder to edit.");
      }
    }
  }

  /**
   * FULL save (fire-and-forget) — persist + RE-INDEX (brain) + vault re-export via
   * `updateNoteDoc`. Requested ONLY on natural boundaries (Preview, editor close,
   * retry) so the e5 re-embed never fires per keystroke-pause. NOT awaited by
   * navigation, so leaving is instant and the backend catches up. Joins the same
   * single-writer chain as the cheap saves, superseding any pending cheap save.
   */
  private flushFull(): void {
    this.debounce.cancel("note-editor-save");
    void this.queueSave("full");
  }

  /**
   * The full-save body — persist + re-index + export, clearing `dirtyFull` on
   * success. Runs ONLY via {@link runPendingSave} on the single-writer chain.
   */
  private async saveFull(): Promise<void> {
    const doc = this.note();
    if (!doc || doc.locked) {
      return;
    }
    const { title, markdown } = this.currentPayload();
    this.saveState.set("saving");
    try {
      const fresh = await this.ipc.updateNoteDoc(doc.id, title, markdown);
      this.dirtyFull = false;
      this.note.update((cur) =>
        cur
          ? {
              ...cur,
              updatedAt: fresh.updatedAt,
              exportedPath: fresh.exportedPath,
              shared: fresh.shared,
              locked: fresh.locked,
            }
          : cur,
      );
      this.saveState.set("saved");
    } catch (e) {
      this.saveState.set("error");
      if (String(e).includes("Locked")) {
        this.toast.danger("This note is locked — unlock its folder to edit.");
      }
    }
  }

  /** Blur handler (title / body) — flush the pending CHEAP save immediately. */
  onBlur(): void {
    if (this.saveState() === "saving") {
      this.debounce.cancel("note-editor-save");
      void this.queueSave("text");
    }
  }

  /** Retry a failed save — the full path (re-index + export). */
  retrySave(): void {
    this.flushFull();
  }

  // ── Tags ─────────────────────────────────────────────────────────────────

  onTagInput(event: Event): void {
    this.tagDraft.set((event.target as HTMLInputElement).value);
  }

  addTag(tag?: string): void {
    const raw = (tag ?? this.tagDraft()).trim().replace(/^#/, "").toLowerCase();
    if (!raw) {
      return;
    }
    if (!this.tags().includes(raw)) {
      this.tags.update((list) => [...list, raw]);
      this.scheduleSave();
    }
    this.tagDraft.set("");
    afterNextRender(() => this.tagInput()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

  removeTag(tag: string): void {
    this.tags.update((list) => list.filter((t) => t !== tag));
    this.scheduleSave();
  }

  toggleProps(): void {
    this.propsOpen.update((v) => !v);
  }

  // ── Properties ───────────────────────────────────────────────────────────

  startAddProp(): void {
    this.addingProp.set(true);
    this.propKeyDraft.set("");
    this.propValDraft.set("");
  }

  onPropKeyInput(event: Event): void {
    this.propKeyDraft.set((event.target as HTMLInputElement).value);
  }
  onPropValInput(event: Event): void {
    this.propValDraft.set((event.target as HTMLInputElement).value);
  }

  /** Pre-fill a common property key (status/date/aliases) into the add form. */
  addPropPreset(key: string): void {
    this.addingProp.set(true);
    this.propKeyDraft.set(key);
    this.propValDraft.set("");
  }

  commitProp(): void {
    const key = this.propKeyDraft().trim();
    const value = this.propValDraft().trim();
    if (!key || key.toLowerCase() === "tags") {
      this.addingProp.set(false);
      return;
    }
    this.properties.update((props) => ({ ...props, [key]: value }));
    this.addingProp.set(false);
    this.scheduleSave();
  }

  cancelAddProp(): void {
    this.addingProp.set(false);
  }

  editProp(key: string, event: Event): void {
    const value = (event.target as HTMLInputElement).value;
    this.properties.update((props) => ({ ...props, [key]: value }));
    this.scheduleSave();
  }

  removeProp(key: string): void {
    this.properties.update((props) => {
      const next = { ...props };
      delete next[key];
      return next;
    });
    this.scheduleSave();
  }

  // ── Edit / Preview ───────────────────────────────────────────────────────

  setPreview(on: boolean): void {
    this.preview.set(on);
    if (on) {
      // No textarea to select in Preview — drop the floating bubble / Brain popover.
      this.clearSelection();
    }
    // Switching to Preview is a natural "I'm reviewing" pause: run the deferred FULL
    // save (re-index + vault export) once if there are unindexed edits, so the note
    // becomes brain-searchable + vault-synced without waiting for editor close.
    if (on && this.dirtyFull) {
      this.flushFull();
    }
  }

  // ── Formatting toolbar ─────────────────────────────────────────────────

  /**
   * Apply a formatting op to the textarea selection: wrap/toggle the markdown
   * markers and restore the caret. Operates on `selectionStart/End`, writes the
   * new value back through the `body` signal, then re-selects.
   */
  format(op: FormatOp): void {
    const el = this.bodyArea()?.nativeElement;
    if (!el) {
      return;
    }
    const value = el.value;
    const start = el.selectionStart;
    const end = el.selectionEnd;
    const selected = value.slice(start, end);

    let replacement = selected;
    let caretStart = start;
    let caretEnd = end;

    switch (op) {
      case "bold":
        replacement = `**${selected || "bold"}**`;
        caretStart = start + 2;
        caretEnd = caretStart + (selected || "bold").length;
        break;
      case "italic":
        replacement = `*${selected || "italic"}*`;
        caretStart = start + 1;
        caretEnd = caretStart + (selected || "italic").length;
        break;
      case "strike":
        replacement = `~~${selected || "text"}~~`;
        caretStart = start + 2;
        caretEnd = caretStart + (selected || "text").length;
        break;
      case "code":
        replacement = `\`${selected || "code"}\``;
        caretStart = start + 1;
        caretEnd = caretStart + (selected || "code").length;
        break;
      case "link":
        replacement = `[${selected || "text"}](url)`;
        caretStart = start + 1;
        caretEnd = caretStart + (selected || "text").length;
        break;
      case "wikilink":
        replacement = `[[${selected || "Note"}]]`;
        caretStart = start + 2;
        caretEnd = caretStart + (selected || "Note").length;
        break;
      case "h1":
      case "h2":
      case "h3":
        return this.applyLinePrefix(el, op === "h1" ? "# " : op === "h2" ? "## " : "### ", true);
      case "ul":
        return this.applyLinePrefix(el, "- ", false);
      case "ol":
        return this.applyLinePrefix(el, "1. ", false);
      case "check":
        return this.applyLinePrefix(el, "- [ ] ", false);
      case "quote":
        return this.applyLinePrefix(el, "> ", false);
      case "codeblock": {
        replacement = `\`\`\`\n${selected}\n\`\`\``;
        caretStart = start + 4;
        caretEnd = caretStart + selected.length;
        break;
      }
      case "divider": {
        const nl = start > 0 && value[start - 1] !== "\n" ? "\n" : "";
        replacement = `${nl}\n---\n`;
        caretStart = caretEnd = start + replacement.length;
        break;
      }
    }

    this.replaceRange(el, start, end, replacement, caretStart, caretEnd);
  }

  /**
   * Toggle a line-prefix (heading / list / quote) on every line the selection
   * touches. For a heading, toggling replaces any existing heading prefix.
   */
  private applyLinePrefix(
    el: HTMLTextAreaElement,
    prefix: string,
    isHeading: boolean,
  ): void {
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
      if (isHeading) {
        const stripped = line.replace(/^#{1,6}\s+/, "");
        return allHave ? stripped : prefix + stripped;
      }
      if (line.startsWith(prefix)) {
        return line.slice(prefix.length);
      }
      // Remove a conflicting list/quote prefix before applying this one.
      const cleaned = line.replace(/^(?:- \[[ xX]\] |[-*] |\d+\. |> )/, "");
      return prefix + cleaned;
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
    this.body.set(next);
    el.value = next;
    el.setSelectionRange(caretStart, caretEnd);
    el.focus();
    this.autoGrow();
    this.scheduleSave();
  }

  // ── Keyboard behaviors on the body textarea ─────────────────────────────

  onBodyKeydown(event: KeyboardEvent): void {
    const el = this.bodyArea()?.nativeElement;
    if (!el) {
      return;
    }

    // Slash menu navigation takes priority when open.
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

    if (event.key === "Enter" && !event.shiftKey) {
      if (this.handleListEnter(el, event)) {
        return;
      }
    }
    if (event.key === "Tab") {
      if (this.handleListTab(el, event)) {
        return;
      }
    }
    if (event.key === "Escape" && this.slashOpen()) {
      event.preventDefault();
      this.slashOpen.set(false);
    }
  }

  /**
   * Enter inside a list/checkbox item: auto-continue the marker on the next line
   * (renumbering ordered items), OR exit the list when the current item is empty.
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
    // Continue: renumber for ordered lists, reset a checkbox to unchecked.
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

  /** Tab / Shift-Tab indent or outdent the current list line(s). */
  private handleListTab(el: HTMLTextAreaElement, event: KeyboardEvent): boolean {
    const value = el.value;
    const pos = el.selectionStart;
    const lineStart = value.lastIndexOf("\n", pos - 1) + 1;
    const line = value.slice(lineStart, value.indexOf("\n", pos) === -1 ? value.length : value.indexOf("\n", pos));
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

  // ── Slash menu ───────────────────────────────────────────────────────────

  /** Open the slash menu when `/` was just typed at the start of a line. */
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

  /** Insert a slash-menu block, replacing the `/` trigger, caret at the `$`. */
  pickSlash(item: SlashItem): void {
    const el = this.bodyArea()?.nativeElement;
    if (!el) {
      return;
    }
    const value = el.value;
    const pos = el.selectionStart;
    const lineStart = value.lastIndexOf("\n", pos - 1) + 1;
    // Replace from the `/` (line start) through the caret with the snippet.
    const caretMarker = item.snippet.indexOf("$");
    const snippet = item.snippet.replace("$", "");
    const caret = lineStart + (caretMarker === -1 ? snippet.length : caretMarker);
    this.slashOpen.set(false);
    this.replaceRange(el, lineStart, pos, snippet, caret, caret);
  }

  // ── Header: Move / ⋯ / Share / Export / Delete ──────────────────────────

  toggleMenu(which: "move" | "more"): void {
    this.menu.update((cur) => (cur === which ? "none" : which));
    this.confirmingDelete.set(false);
  }

  closeMenus(): void {
    this.menu.set("none");
    this.confirmingDelete.set(false);
  }

  /** Move this note into `folderId` via `moveNoteDoc`; reload the doc. */
  async moveTo(folderId: string): Promise<void> {
    const doc = this.note();
    if (!doc || folderId === doc.folderId) {
      this.closeMenus();
      return;
    }
    this.closeMenus();
    try {
      await this.notes.move(doc.id, folderId);
      this.note.update((cur) => (cur ? { ...cur, folderId } : cur));
      const name = this.noteFolders().find((f) => f.id === folderId)?.name ?? "folder";
      this.toast.success(`Moved to ${name}`);
    } catch (e) {
      this.toast.danger(`Couldn’t move — ${String(e)}`);
    }
  }

  /** Export / re-write the vault `.md` and reveal it (best-effort). */
  async exportNote(): Promise<void> {
    const doc = this.note();
    if (!doc) {
      return;
    }
    this.closeMenus();
    // Persist the latest text first (cheap) so the exported file is current —
    // awaited through the single-writer chain so it lands after any in-flight save.
    this.debounce.cancel("note-editor-save");
    await this.queueSave("text");
    try {
      const path = await this.ipc.exportNoteDoc(doc.id);
      this.note.update((cur) => (cur ? { ...cur, exportedPath: path } : cur));
      this.toast.success("Saved to your vault");
    } catch (e) {
      this.toast.danger(`Couldn’t export — ${String(e)}`);
    }
  }

  /**
   * SHARE — open the end-to-end-encrypted link-share modal for THIS note
   * (`shareNoteToLinkDoc` + manage/revoke). A locked note is never shared (the
   * backend refuses `Locked`); the button is disabled while locked, and this
   * flushes any pending edit so the shared body is current.
   */
  share(): void {
    this.closeMenus();
    const doc = this.note();
    if (!doc || doc.locked) {
      return;
    }
    // Persist the latest text (cheap, via the single-writer chain) so the
    // shared body is current.
    this.debounce.cancel("note-editor-save");
    void this.queueSave("text");
    this.shareOpen.set(true);
  }

  /** Close the share modal (backdrop / Close / Esc). */
  closeShare(): void {
    this.shareOpen.set(false);
  }

  /**
   * A share was created/revoked — reconcile this note's `shared` flag from the
   * backend so the ⋯ menu badge stays truthful (best-effort; a failure leaves
   * the stale flag, self-healing on the next load).
   */
  async onShareChanged(): Promise<void> {
    const doc = this.note();
    if (!doc) {
      return;
    }
    try {
      const fresh = await this.ipc.getNote(doc.id);
      this.note.update((cur) => (cur ? { ...cur, shared: fresh.shared } : cur));
    } catch {
      // Ignore — the flag self-heals on the next fetch.
    }
  }

  /** The share gate CTA routes to Settings (the Account/Sharing controls live there). */
  goToSharingSettings(): void {
    this.shareOpen.set(false);
    void this.router.navigate(["/settings"]);
  }

  askDelete(): void {
    this.confirmingDelete.set(true);
  }

  cancelDelete(): void {
    this.confirmingDelete.set(false);
  }

  async doDelete(): Promise<void> {
    const doc = this.note();
    if (!doc) {
      return;
    }
    // Cancel a pending autosave, drop any queued-but-not-started save, and clear
    // the dirty flag so neither the queue nor the teardown full-save resurrects
    // the just-deleted note. (An already in-flight save cannot be recalled —
    // same as before — but nothing new is started.)
    this.debounce.cancel("note-editor-save");
    this.pendingSave = null;
    this.dirtyFull = false;
    try {
      await this.notes.remove(doc.id);
      this.toast.success("Note deleted");
      void this.router.navigate(["/notes"]);
    } catch (e) {
      this.toast.danger(`Couldn’t delete — ${String(e)}`);
      this.confirmingDelete.set(false);
    }
  }

  // ── Lock gate ────────────────────────────────────────────────────────────

  /**
   * Unlock the note's folder (biometric `unlock_folder`), then re-fetch the now
   * unmasked note. Mirrors the meeting-detail unlock: on failure stay gated.
   */
  async unlock(): Promise<void> {
    const doc = this.note();
    if (!doc || this.unlocking()) {
      return;
    }
    this.unlocking.set(true);
    try {
      await this.folders.unlock(doc.folderId);
      const fresh = await this.ipc.getNote(doc.id);
      this.hydrate(fresh);
    } catch (e) {
      this.toast.danger(`Couldn’t unlock — ${String(e)}`);
    } finally {
      this.unlocking.set(false);
    }
  }

  // ── Selection toolbar + Brain popover ───────────────────────────────────

  /**
   * On a non-empty selection inside the body textarea, capture the selected text
   * + bounded surrounding context + the anchor rect and float the formatting
   * bubble. A collapsed selection (or Preview) closes everything. Called on
   * mouseup / keyup / select. Formatting is ALWAYS offered; the bubble's AI button
   * is gated separately by {@link anyAssistEnabled}.
   */
  onBodySelect(): void {
    const el = this.bodyArea()?.nativeElement;
    if (!el || this.preview()) {
      this.clearSelection();
      return;
    }
    const start = el.selectionStart;
    const end = el.selectionEnd;
    const text = el.value.slice(start, end);
    if (text.trim().length === 0) {
      this.clearSelection();
      return;
    }
    // Unchanged selection → leave state alone, so a stray keyup/mouseup can't
    // reset an open Brain popover back to the bubble or re-trigger a reposition.
    const cur = this.sel();
    if (cur && cur.start === start && cur.end === end && cur.text === text) {
      return;
    }
    const rect = this.selectionRect(el, start, end);
    if (!rect) {
      return;
    }
    this.sel.set({
      text,
      start,
      end,
      before: el.value.slice(Math.max(0, start - CONTEXT_CHARS), start),
      after: el.value.slice(end, end + CONTEXT_CHARS),
      rect,
    });
    // A fresh selection always returns to the formatting bubble.
    this.brainOpen.set(false);
  }

  /** Apply a bubble formatting op, then re-anchor the bubble to the new selection. */
  onToolbarFormat(op: FormatOp): void {
    this.format(op);
    // `format` re-selects the transformed span (or collapses for block inserts);
    // re-capture so the bubble repositions to the new rect (or hides if collapsed).
    this.onBodySelect();
  }

  /** The AI button — open the Brain popover over the current selection. */
  openBrain(): void {
    if (this.sel()) {
      this.brainOpen.set(true);
    }
  }

  /** Dismiss the Brain popover + the bubble (Close / Discard / after Accept). */
  closePopover(): void {
    this.clearSelection();
  }

  /** Drop the selection state (hides both the bubble and the Brain popover). */
  private clearSelection(): void {
    this.sel.set(null);
    this.brainOpen.set(false);
  }

  /**
   * Apply an accepted assistant edit into the textarea. Refine/Shorten REPLACE
   * the current selection; Enhance INSERTS the passage AFTER the selection
   * (additive — never destroys the user's text). Then autosave.
   */
  applyEdit(edit: AcceptedEdit): void {
    const el = this.bodyArea()?.nativeElement;
    const sel = this.sel();
    if (!el || !sel) {
      return;
    }
    const value = el.value;
    // Prefer the exact captured offsets (unambiguous even when the same phrase
    // appears elsewhere); fall back to a search only if the body shifted under
    // us. Bail rather than corrupt text if we can no longer locate the span.
    let start = sel.start;
    let end = sel.end;
    if (value.slice(start, end) !== sel.text) {
      start = value.indexOf(sel.text);
      if (start === -1) {
        this.clearSelection();
        return;
      }
      end = start + sel.text.length;
    }
    if (edit.action === "enhance") {
      // Insert the additive passage after the selection on its own paragraph.
      const insert = `\n\n${edit.suggestion.trim()}\n`;
      this.replaceRange(el, end, end, insert, end + insert.length, end + insert.length);
    } else {
      this.replaceRange(el, start, end, edit.suggestion, start, start + edit.suggestion.length);
    }
    this.clearSelection();
  }

  // ── Presentational helpers ────────────────────────────────────────────

  /**
   * The viewport rect of the textarea selection. There is no native
   * `Range.getBoundingClientRect` for a `<textarea>`, so we mirror the text into
   * a hidden measuring div and read the caret rect there — deterministic + no
   * dependency. Best-effort: falls back to the textarea's own rect.
   */
  private selectionRect(
    el: HTMLTextAreaElement,
    start: number,
    end: number,
  ): { top: number; left: number; right: number; bottom: number } | null {
    try {
      const div = document.createElement("div");
      const style = getComputedStyle(el);
      for (const prop of [
        "boxSizing",
        "width",
        "paddingTop",
        "paddingRight",
        "paddingBottom",
        "paddingLeft",
        "borderWidth",
        "fontFamily",
        "fontSize",
        "fontWeight",
        "lineHeight",
        "letterSpacing",
        "whiteSpace",
        "wordWrap",
        "textAlign",
      ] as const) {
        (div.style as unknown as Record<string, string>)[prop] = style[prop];
      }
      div.style.position = "fixed";
      div.style.visibility = "hidden";
      div.style.whiteSpace = "pre-wrap";
      div.style.wordWrap = "break-word";
      div.style.overflow = "hidden";
      const rect = el.getBoundingClientRect();
      div.style.top = `${rect.top - el.scrollTop}px`;
      div.style.left = `${rect.left}px`;
      div.style.height = `${el.clientHeight}px`;

      const before = document.createTextNode(el.value.slice(0, start));
      const selNode = document.createElement("span");
      selNode.textContent = el.value.slice(start, end) || ".";
      div.appendChild(before);
      div.appendChild(selNode);
      document.body.appendChild(div);
      const spanRect = selNode.getBoundingClientRect();
      document.body.removeChild(div);
      if (spanRect.width === 0 && spanRect.height === 0) {
        return { top: rect.top, left: rect.left, right: rect.right, bottom: rect.bottom };
      }
      return {
        top: spanRect.top,
        left: spanRect.left,
        right: spanRect.right,
        bottom: spanRect.bottom,
      };
    } catch {
      const rect = el.getBoundingClientRect();
      return { top: rect.top, left: rect.left, right: rect.right, bottom: rect.bottom };
    }
  }

  /**
   * Return to the Notes home. Navigation is INSTANT — the teardown (`onDestroy`) runs
   * the full save (re-index + export) fire-and-forget in the background, so leaving is
   * never blocked on the e5 re-embed. The text was already cheap-persisted.
   */
  back(): void {
    void this.router.navigate(["/notes"]);
  }
}
