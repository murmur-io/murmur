import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  EnvironmentInjector,
  Injector,
  afterNextRender,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
  untracked,
  viewChild,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { FormsModule } from "@angular/forms";
import { ActivatedRoute, Router } from "@angular/router";
import { map } from "rxjs";
import { IpcService } from "../../../core/ipc.service";
import { NavHistoryService } from "../../../core/nav-history.service";
import { RecordingFlushService } from "../../../core/recording-flush.service";
import { tabKeyFor } from "../../../core/tab-keys";
import { TabRouteReuseStrategy } from "../../../core/tab-route-reuse.strategy";
import { TabsService } from "../../../core/tabs.service";
import type {
  AppConfigDto,
  FolderNode,
  NoteAttachmentDto,
  NoteCitation,
  NoteDoc,
  NoteFolder,
} from "../../../core/models";
import { DebounceService } from "../../../services/debounce.service";
import { FoldersService } from "../../../services/folders.service";
import { NotesService } from "../../../services/notes.service";
import { ToastService } from "../../../services/toast.service";
import {
  NoteAttachmentService,
  MAX_NOTE_ATTACHMENTS,
  insertMarkdownBlock,
  referencedNoteAttachments,
  replacePendingAttachmentUri,
  type AttachmentPastePlan,
  type MarkdownEdit,
} from "../../../services/note-attachment.service";
import { ConnectionsComponent } from "../../../shared/connections/connections.component";
import { MarkdownComponent } from "../../../shared/markdown/markdown.component";
import { LinkPickerComponent } from "../link-picker/link-picker.component";
import { NOTE_ASSIST_CATALOG } from "../note-brain-popover/note-assist-catalog";
import {
  NoteBrainPopoverComponent,
  type AcceptedEdit,
  type PopoverSelection,
} from "../note-brain-popover/note-brain-popover.component";
import { NoteSharePanelComponent } from "../note-share-panel/note-share-panel.component";
import { NoteSelectionToolbarComponent } from "../note-selection-toolbar/note-selection-toolbar.component";
import { NoteChatComponent } from "../note-chat/note-chat.component";
import { MurCopyIdComponent } from "../../../design-system/copy-id/copy-id.component";
import { MurIconComponent } from "../../../design-system/icon/icon.component";
import { MurToggleComponent } from "../../../design-system/toggle/toggle.component";
import { parseDoc, serializeDoc } from "./front-matter";
import {
  coerceForKind,
  formatForYaml,
  type PropertyKind,
  type PropertySchemaField,
} from "./property-field-types";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";
import { SmartReminderCardComponent } from "../../reminders/smart-reminder-card/smart-reminder-card.component";

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

/** One slash-menu block insertion, OR the special "Link to note" entry (no `snippet`). */
interface SlashItem {
  id: string;
  label: string;
  /**
   * The markdown snippet inserted at the caret (with `$` marking the caret).
   * Absent for `linkToNote` — that entry opens the link picker instead of
   * inserting a static snippet (`pickSlash` branches on `id === "linkToNote"`).
   */
  snippet?: string;
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
  { id: "image", label: "Image" },
  // No `snippet` — opens the inline link-picker popover (Obsidian-style `[[` autocomplete)
  // instead of inserting static markdown. Reuses the SAME picker as the raw `[[` keystroke
  // trigger (one shared component/service, per the parity requirement).
  { id: "linkToNote", label: "Link to note" },
];

/** Up to this many chars of before/after context ship with an assistant request. */
const CONTEXT_CHARS = 500;
const AUTOSAVE_MS = 600;

/** The property kinds offered when defining a NEW schema field (label + kind). */
const PROPERTY_KIND_OPTIONS: readonly { kind: PropertyKind; label: string }[] = [
  { kind: "text", label: "Text" },
  { kind: "select", label: "Select" },
  { kind: "date", label: "Date" },
  { kind: "checkbox", label: "Checkbox" },
  { kind: "number", label: "Number" },
];

/**
 * localStorage key for the "Full width" display preference: "1" = full width.
 * A GLOBAL preference (not per-note) — Notion persists this per-page, but that
 * needs a backend column + IPC round-trip for a purely cosmetic toggle; a
 * global `localStorage` flag mirrors the existing chrome prefs (`AppShellComponent`'s
 * `SIDEBAR_KEY`/`INSIGHTS_KEY`) and is zero-risk to wire correctly. Revisit if
 * per-note persistence is explicitly requested.
 */
const FULL_WIDTH_KEY = "murmur-note-full-width";

/**
 * localStorage key for the "Ask Brain" chat drawer open/closed state: "1" = open.
 * Mirrors {@link FULL_WIDTH_KEY} exactly — a GLOBAL chrome preference (not
 * per-note), zero-risk to wire, matching the existing chrome prefs. Defaults
 * CLOSED (the drawer starts collapsed so the writing surface owns the width).
 */
const NOTE_CHAT_OPEN_KEY = "murmur-note-chat-open";

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
  host: {
    "(document:pointerdown)": "onDocumentPointerDown($event)",
  },
  imports: [
    ConnectionsComponent,
    LinkPickerComponent,
    MarkdownComponent,
    NoteBrainPopoverComponent,
    NoteSelectionToolbarComponent,
    NoteSharePanelComponent,
    NoteChatComponent,
    MurToggleComponent,
    MurCopyIdComponent,
    MurIconComponent,
    SmartReminderCardComponent,
    FormsModule,
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
  private readonly attachmentService = inject(NoteAttachmentService);
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);
  private readonly tabsService = inject(TabsService);
  private readonly tabRouteReuse = inject(TabRouteReuseStrategy);
  private readonly injector = inject(Injector);
  private readonly errorCopy = inject(ErrorCopyService);
  /** Environment (root) injector — hosts the detach-proof root lock effect. */
  private readonly envInjector = inject(EnvironmentInjector);
  private readonly destroyRef = inject(DestroyRef);
  /** The flush-before-finalize seam — registered while EMBEDDED (companion editor). */
  private readonly flushService = inject(RecordingFlushService);

  /** Drill-down back navigation ("← Notes"). */
  readonly nav = inject(NavHistoryService);

  /**
   * The slash-menu catalog. On the routed editor this is exactly {@link SLASH_ITEMS}
   * (byte-for-byte unchanged). When {@link embedded} (the recording surface) it puts
   * an "Ask Brain" entry FIRST (Notion-style AI-at-the-top) so `/` immediately surfaces
   * the Brain instead of burying it below 12 block items in the scrollable menu — the
   * Calm-Notepad "summon, don't station" model. The default keyboard highlight still
   * lands on the first BLOCK (see {@link maybeOpenSlash}) so `/`+Enter keeps inserting a
   * heading; Ask is the prominent top item you click or arrow up to.
   */
  protected readonly slashItems = computed<readonly SlashItem[]>(() =>
    this.embedded()
      ? [{ id: "askBrain", label: "✦ Ask Brain" }, ...SLASH_ITEMS]
      : SLASH_ITEMS,
  );

  /**
   * The body textarea's placeholder — a warm ghost prompt. In the recording
   * surface ({@link embedded}) it names both note-taking AND the two ways to reach
   * the Brain, so an empty note reads as an invitation, not a void. The routed
   * editor keeps its original block-oriented prompt.
   */
  protected readonly bodyPlaceholder = computed(() =>
    this.embedded()
      ? "Type to take notes — / for blocks, or Ask Brain"
      : "Start writing… Type / for blocks.",
  );

  /**
   * EMBEDDED mode (additive, 2026-07-17) — when true this editor is HOSTED
   * inside another surface (the recording panel's "Note" tab) rather than the
   * `/notes/:id` route: it reads its note id from {@link noteIdInput} instead
   * of the route, hides the page chrome (header / title / properties bar /
   * backlinks), forces Edit mode (no Preview toggle), and fills its host
   * container (no full-viewport / sticky-header assumptions). The BODY editor,
   * the selection toolbar, the in-note Brain popover, the link picker, autosave,
   * and the locked/empty states all stay. Defaults false ⇒ the routed
   * `/notes/:id` path is byte-for-byte unchanged.
   */
  readonly embedded = input(false);
  /** The note id to load when {@link embedded}. Ignored on the route. */
  readonly noteIdInput = input<string | null>(null);

  /**
   * Emitted when the user picks the "Ask Brain" entry from the `/` slash menu
   * (Calm-Notepad redesign, 2026-07-19). ONLY offered when {@link embedded} (the
   * recording surface), so the routed `/notes/:id` slash menu is byte-for-byte
   * unchanged. The recording surface host summons the Ask-Brain panel on this —
   * the editor itself owns no Ask panel, keeping the note the hero.
   */
  readonly askBrain = output<void>();

  /**
   * The route `:id`, tracked so a same-route navigation re-fetches even though
   * the RouteReuseStrategy keeps this instance. `null` on `/notes/new`.
   */
  private readonly routeId = toSignal(
    this.route.paramMap.pipe(map((p) => p.get("id"))),
    { initialValue: this.route.snapshot.paramMap.get("id") },
  );

  /**
   * The note id the load/save effects act on: {@link noteIdInput} when
   * {@link embedded} (the route is NOT read), else the route `:id`. A single
   * `computed` so the whole load/save machinery is source-agnostic — the routed
   * path (`embedded()===false`) resolves EXACTLY `routeId()` as before, so its
   * behavior is unchanged.
   */
  private readonly activeNoteId = computed<string | null>(() =>
    this.embedded() ? this.noteIdInput() : (this.routeId() ?? null),
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
  /** Gated image DTOs for the active, unlocked note. Cleared synchronously on lock. */
  readonly attachments = signal<NoteAttachmentDto[]>([]);
  /** Only images referenced by the current body are disclosed in share UI. */
  readonly referencedAttachments = computed(() =>
    referencedNoteAttachments(this.body(), this.attachments()),
  );
  /** Number of local canvas/import jobs still replacing stable pending markers. */
  readonly importingImages = signal(0);
  /** Front-matter tags. */
  readonly tags = signal<string[]>([]);
  /** Front-matter properties (key → value), excluding tags. */
  readonly properties = signal<Record<string, string>>({});

  // --- View state ----------------------------------------------------------
  /**
   * Edit vs Preview. Starts false, but {@link hydrate} flips an OPENED note that
   * already has a body into Preview — see {@link opensInPreview}. The initial
   * value stays `false` so an empty/new note (and the embedded host) never begins
   * in a read-only pane.
   */
  readonly preview = signal(false);
  /**
   * `"<note id>:<locked>"` of the last document the open-in-Preview default was
   * applied to.
   *
   * `hydrate` is NOT only "the user opened a note" — it also re-runs on a
   * lock/unlock transition for the note already on screen (see the seal branch in
   * the folder-tree lock effect). Keying the default means a re-hydrate cannot
   * yank someone out of Edit mid-note, while a genuine open — or an unlock, which
   * changes the `locked` half — still gets the default.
   */
  private previewDefaultedFor: string | null = null;
  /** Properties bar expanded. */
  readonly propsOpen = signal(false);
  /** The autosave indicator. */
  readonly saveState = signal<SaveState>("idle");
  /**
   * What went wrong behind the last failed save — surfaced as a tooltip on the "Save failed" pill
   * instead of being swallowed (root-cause fix, 2026-07-15: before it, every rejection other than
   * a lock refusal showed a blank, undiagnosable red banner).
   *
   * P3: this is now the sentence `ErrorCopyService` owns, not the raw `AppError` display. The raw
   * message was chosen deliberately in 2026-07-15 for diagnosability, but the same channel carries
   * developer vocabulary from ~2100 `AppError` sites, and an autosave tooltip is not the place to
   * leak it. A recognised failure still says exactly what happened; an unrecognised one says
   * "Couldn’t save this note. Please try again." rather than nothing.
   */
  readonly saveErrorMessage = signal<string | null>(null);
  /** The tag-input draft. */
  readonly tagDraft = signal("");
  /** Which floating menu (if any) is open in the header. */
  readonly menu = signal<"none" | "move" | "more">("none");
  /** Add-property menu open (the DEFINED-keys picker + "New property" hatch). */
  readonly addingProp = signal(false);
  /**
   * The "New property" sub-form is open (a fresh key + kind that gets PERSISTED
   * to the folder schema on commit). false = showing the defined-keys picker.
   */
  readonly definingProp = signal(false);
  readonly propKeyDraft = signal("");
  /** The kind chosen for a brand-new schema property (defaults to text). */
  readonly propKindDraft = signal<PropertyKind>("text");
  /** Two-step delete confirm. */
  readonly confirmingDelete = signal(false);
  /** True while the Share modal is open over the document. */
  readonly shareOpen = signal(false);
  /**
   * Notion-style "Full width" display toggle (item lives in the ⋯ menu, not a
   * new header icon — the ⋯ menu already hosts view/action items). A GLOBAL
   * preference (see {@link FULL_WIDTH_KEY}); overrides `--editor-max` with
   * `min(1600px, 92%)` on `.note-doc` (note-editor.component.scss).
   */
  readonly fullWidth = signal(this.readStoredFullWidth());
  /**
   * "Ask Brain" chat drawer open/closed (routed mode only — the embedded
   * companion + a locked note never show it). Persisted GLOBALLY under
   * {@link NOTE_CHAT_OPEN_KEY}, mirroring {@link fullWidth}. Default COLLAPSED.
   */
  readonly noteChatOpen = signal(this.readStoredChatOpen());

  /** The note-kind folders (for the Move menu + breadcrumb). */
  readonly noteFolders = signal<NoteFolder[]>([]);
  /** True while a folder unlock is in flight (lock gate). */
  readonly unlocking = signal(false);

  // --- Slash menu ----------------------------------------------------------
  readonly slashOpen = signal(false);
  readonly slashIndex = signal(0);

  // --- Link picker (Obsidian-style `[[` autocomplete, Fix 2) ----------------
  /**
   * The open link-picker's trigger span (`start`..caret in the body text to be
   * REPLACED with `[[Title]]` on pick) + the caret's viewport anchor rect for
   * positioning. `null` when the picker is closed. Set by BOTH trigger paths —
   * the raw `[[` keystroke ({@link maybeOpenLinkPicker}) and the slash-menu
   * "Link to note" entry ({@link pickSlash}) — so there is exactly ONE picker
   * instance/codepath regardless of how it was opened (parity requirement).
   */
  readonly linkPickerTrigger = signal<{
    start: number;
    rect: { top: number; left: number; right: number; bottom: number };
  } | null>(null);
  /** Keyboard-highlighted row in the open picker (↑/↓, mirrors {@link slashIndex}). */
  readonly linkPickerActiveIndex = signal(0);
  /** The picker's live candidate list — updated via its `candidatesChange` output so
   *  this component's ↑/↓/Enter handler (shared with the slash menu) can act on it
   *  without duplicating the fetch. */
  readonly linkPickerCandidates = signal<NoteCitation[]>([]);
  /**
   * The live filter text derived from what's been typed since the trigger.
   * Tracks `body()` (a signal, so it re-runs on every keystroke) rather than
   * reading `el.value` directly — a `computed()` only re-runs when a SIGNAL
   * dependency changes, and raw DOM reads aren't one.
   */
  readonly linkPickerQuery = computed(() => {
    const trigger = this.linkPickerTrigger();
    const body = this.body();
    const el = this.bodyArea()?.nativeElement;
    if (!trigger || !el) {
      return "";
    }
    const caret = Math.max(trigger.start, el.selectionStart);
    return body.slice(trigger.start, caret);
  });

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
  private readonly imageFileInput =
    viewChild<ElementRef<HTMLInputElement>>("imageFileInput");
  /** Live textarea node passed to the teleported link picker for motion filtering. */
  readonly linkPickerAnchorElement = computed(
    () => this.bodyArea()?.nativeElement ?? null,
  );
  /** Coalesce captured scroll bursts into one post-render caret measurement. */
  private linkPickerRepositionQueued = false;
  private readonly tagInput =
    viewChild<ElementRef<HTMLInputElement>>("tagInput");
  private readonly selectionToolbar = viewChild(NoteSelectionToolbarComponent);
  private readonly brainPopover = viewChild(NoteBrainPopoverComponent);
  private readonly linkPicker = viewChild(LinkPickerComponent);

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
  /** Selection preserved before the hidden file picker takes focus. */
  private imageInsertion: { start: number; end: number } | null = null;
  /** In-flight imports joined by every save/finalize boundary. */
  private readonly attachmentTasks = new Set<Promise<void>>();

  // --- Single-writer save queue --------------------------------------------
  /**
   * The persistence chain — EVERY backend write for this note (cheap
   * `saveNoteText` and full `updateNoteDoc`) is appended here, so writes reach
   * the backend strictly in order. Without it the debounced cheap save and a
   * boundary full save could be concurrently in flight with different payloads,
   * and a stale older write could land after a newer one.
   */
  private saveChain: Promise<boolean> = Promise.resolve(false);
  /**
   * The single coalesced pending save (latest-wins): while a save is in flight,
   * new requests only escalate/refresh this slot — they never stack. `"full"`
   * supersedes `"text"` (the full save persists a superset). The payload is
   * snapshotted from the signals when the queued save RUNS, so the newest text
   * always wins.
   */
  private pendingSave: "text" | "full" | null = null;
  /** Monotonic local edit generation used to join an already-current save at Stop. */
  private editRevision = 0;
  /** Revision snapshotted by the save currently executing on {@link saveChain}. */
  private savingRevision: number | null = null;
  /** Most recent revision whose backend persistence attempt fully settled. */
  private settledRevision = -1;
  /** Durability result paired with {@link settledRevision}. */
  private settledRevisionSaved = false;

  /**
   * Whether the PREVIEW pane is showing. EMBEDDED mode force-hides the
   * Edit/Preview toggle and always shows the editable body, so preview can never
   * be active there (the flag can't be toggled with the control gone — this is
   * belt-and-braces so a stale `preview()` from before an embed can't leak the
   * read-only pane). On the route (`embedded()===false`) this is just `preview()`.
   */
  readonly previewActive = computed(() => this.preview() && !this.embedded());

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

  /**
   * True when this note lives in the reserved always-open root — it's "unfiled"
   * and therefore NOT sealable (unfiled notes are deliberately open plaintext;
   * sealing requires filing into a lockable folder). Drives the breadcrumb label
   * + the "not sealed" privacy hint (2026-07-14).
   */
  readonly isUnfiled = computed(() => this.currentFolder()?.isRoot ?? false);

  /** Human breadcrumb: "Unfiled" for a root note, else the folder's name (or "Notes"). */
  readonly breadcrumb = computed(() => {
    const folder = this.currentFolder();
    if (folder?.isRoot) {
      return "Unfiled";
    }
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

  /**
   * The property KIND options offered when defining a new schema field (template
   * dropdown). Static — exposed as a field so the template can `@for` over it.
   */
  protected readonly propertyKindOptions = PROPERTY_KIND_OPTIONS;

  /**
   * The active note-folder's typed-property SCHEMA (Feature C) — read from the
   * root {@link NotesService}, loaded reactively by {@link _loadSchema} when the
   * note's folder changes. `[]` for a folder with no schema (or a locked folder,
   * which the backend gates). Drives the SCHEMA-AWARE widget per property row and
   * the Add-property menu's "defined keys first" list.
   */
  readonly folderSchema = computed<PropertySchemaField[]>(() =>
    this.notes.folderSchema(),
  );

  /** Fast key → schema-field lookup for resolving a row's widget kind. */
  private readonly schemaByKey = computed<Map<string, PropertySchemaField>>(
    () => new Map(this.folderSchema().map((f) => [f.key, f])),
  );

  /**
   * Property rows for the properties bar (stable order), each ENRICHED with its
   * schema-resolved widget `kind` + select `options`. A key with no schema entry
   * defaults to `text` (unchanged behavior). The underlying value stays the raw
   * front-matter STRING — the widget coerces on read + writes back a string via
   * `formatForYaml`, so `serializeDoc`'s round-trip is untouched.
   */
  readonly propertyRows = computed(() => {
    const byKey = this.schemaByKey();
    return Object.entries(this.properties()).map(([key, value]) => {
      const field = byKey.get(key);
      return {
        key,
        value,
        kind: (field?.kind ?? "text") as PropertyKind,
        options: field?.options ?? [],
      };
    });
  });

  /**
   * The schema keys NOT yet present on this note — the Add-property menu offers
   * these DEFINED keys first (with their kind badge) before the "New property"
   * escape hatch.
   */
  readonly unusedSchemaFields = computed<PropertySchemaField[]>(() => {
    const present = new Set(Object.keys(this.properties()));
    return this.folderSchema().filter((f) => !present.has(f.key));
  });

  /**
   * The app config (loaded best-effort in the constructor), the source for the
   * Settings note-assistant toggles. Null until the first load / on failure.
   */
  private readonly config = signal<AppConfigDto | null>(null);

  /**
   * The set of selection-assistant action ids the user has ENABLED in Settings,
   * fed to the popover (it hides a disabled row; the backend is still the real
   * gate — a disabled action refuses `Unavailable`). The legacy trio follows its
   * own AppConfig bools (`noteAssistRefine`/`-Shorten`/`-Enhance`); every other
   * catalog action is enabled unless its id is in `noteAssistActionsOff`. All
   * default ON: an ABSENT bool reads TRUE and an absent off-list means nothing is
   * off (the same contract the settings block + backend use), and a null config
   * (not yet loaded / load failed) also defaults every action ON so the popover
   * works before the config lands. `custom` is always available (the popover adds
   * it regardless), so it is not in this set.
   */
  readonly enabledActions = computed<Set<string>>(() => {
    const cfg = this.config();
    const off = new Set(cfg?.noteAssistActionsOff ?? []);
    const on = new Set<string>();
    for (const a of NOTE_ASSIST_CATALOG) {
      let enabled: boolean;
      if (a.id === "refine") {
        enabled = cfg?.noteAssistRefine ?? true;
      } else if (a.id === "shorten") {
        enabled = cfg?.noteAssistShorten ?? true;
      } else if (a.id === "enhance") {
        enabled = cfg?.noteAssistEnhance ?? true;
      } else {
        enabled = !off.has(a.id);
      }
      if (enabled) {
        on.add(a.id);
      }
    }
    return on;
  });

  /**
   * Whether the bubble's "Ask Brain" AI entry point is offered. `custom` (the
   * command-input free-text instruction) is ALWAYS available in the popover, so
   * the AI button stays even when the user turned every catalog action off — the
   * user can still type an instruction. (The backend still gates each action.)
   */
  readonly anyAssistEnabled = computed(() => true);

  /**
   * Resolve the ACTIVE note id (route `:id`, or {@link noteIdInput} when
   * embedded) on every change and load it. On the ROUTE with no id (`/notes/new`)
   * the null branch creates a note + replaces the URL; when EMBEDDED a null id
   * means the host hasn't handed us a companion note yet — do NOT auto-create
   * (the host owns eager creation via `get_or_create_companion_note`), just wait
   * (loading stays true until an id arrives). Legitimate signal-writing effect
   * (async IPC + stale guard, T1).
   */
  private readonly _load = effect(() => {
    const embedded = this.embedded();
    const id = this.activeNoteId();
    const seq = ++this.requestSeq;
    if (!id) {
      if (!embedded) {
        void this.createAndOpen(seq);
      }
      // Embedded + no id yet: keep the loading state until the host supplies one.
      return;
    }
    void this.fetchNote(id, seq);
  });

  /**
   * The titles of neighbours the note BODY already links inline via `[[Title]]`
   * (2026-07-19, IA consolidation item 4). Fed to the merged `app-connections`
   * "Related" panel so a body `[[Title]]` — which materializes a `wikilink` edge
   * that would ALSO render as a Related chip — is not triplicated (inline chip +
   * Related chip). A pure `computed` off `body()` (a signal), reusing the SAME
   * `[[...]]` extraction the link engine uses (first-`|`-split alias-safe, though
   * Murmur wikilinks carry no alias today). Empty while there is no body.
   */
  readonly inlineWikilinkTitles = computed<string[]>(() => {
    const body = this.body();
    if (!body.includes("[[")) {
      return [];
    }
    const titles = new Set<string>();
    const re = /\[\[([^\]]+)\]\]/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(body)) !== null) {
      const title = m[1].split("|")[0].trim();
      if (title) {
        titles.add(title);
      }
    }
    return [...titles];
  });

  /**
   * The folder id whose schema the editor should load — null while the note is
   * locked/masked (the backend gates it to `[]` anyway). A `computed` so the
   * schema effect below only re-runs on a REAL folder/lock change, not on every
   * autosave (which replaces the `note()` object reference with an unchanged
   * `folderId`).
   */
  private readonly _schemaFolderId = computed<string | null>(() => {
    const doc = this.note();
    return doc && !doc.locked ? doc.folderId : null;
  });

  /**
   * Load the active note-folder's typed-property SCHEMA when its id changes
   * (Feature C). Delegated to the root {@link NotesService} so the schema is
   * shared with the folder Table/Board views; best-effort (the service captures
   * errors). Legitimate signal-writing effect via the service (T1) — async IPC
   * keyed on a single input.
   */
  private readonly _loadSchema = effect(() => {
    void this.notes.loadSchema(this._schemaFolderId());
  });

  /** Persist the "Full width" preference whenever it changes (mirrors AppShellComponent). */
  private readonly _persistFullWidth = effect(() => {
    const value = this.fullWidth();
    try {
      localStorage.setItem(FULL_WIDTH_KEY, value ? "1" : "0");
    } catch {
      // Private-mode / storage-disabled — the preference is not persisted.
    }
  });

  /** Persist the "Ask Brain" drawer open state whenever it changes (mirrors {@link _persistFullWidth}). */
  private readonly _persistChatOpen = effect(() => {
    const value = this.noteChatOpen();
    try {
      localStorage.setItem(NOTE_CHAT_OPEN_KEY, value ? "1" : "0");
    } catch {
      // Private-mode / storage-disabled — the preference is not persisted.
    }
  });

  constructor() {
    // Warm the note-folder list (Move menu + breadcrumb) + the note list (tag
    // autocomplete) + the config (note-assistant toggles). Best-effort; a
    // failure just means no suggestions / every assistant action defaults ON.
    void this.loadFolders();
    void this.notes.loadNotes(null);
    void this.loadConfig();
    // On teardown (route-leave / app-quit — hard close) run the boundary work ONCE —
    // fire-and-forget so navigation is instant; the backend re-indexes + re-exports (+
    // auto-titles) in the background. Nothing is lost either way (cheap autosaves already
    // persisted the text).
    this.destroyRef.onDestroy(() => void this.runNoteBoundaryWork());

    // Root-cause fix (2026-07-15): the callback above ONLY ever fired on a hard close (✕)
    // or app quit — `notes/:id` tabs are DETACHED, not destroyed, on a plain tab switch
    // (`TabRouteReuseStrategy.shouldDetach` returns true for this route), so
    // `DestroyRef.onDestroy` never runs for the overwhelmingly common "type a note, click
    // a different tab" flow, and a note's title never auto-generated unless the user
    // literally closed the tab. `TabRouteReuseStrategy.onDetach` is the one place that
    // genuinely knows a tab is being backgrounded RIGHT NOW (its `store()`, called by the
    // router mid-navigation) — subscribe here and run the SAME boundary work at that
    // earlier moment too. Filtered to THIS note's own tab key (a detach notification with
    // no filter would run when ANY other note/meeting/org-item tab in the app detaches).
    // Additive, not a replacement — the `onDestroy` trigger above still covers hard-close
    // and app-quit, which never route through the router's detach path at all. Running
    // this work twice for the same note (detach, then later a real destroy) is at worst a
    // wasted no-op IPC round-trip (see {@link runNoteBoundaryWork}'s doc).
    const unsubDetach = this.tabRouteReuse.onDetach((key) => {
      const doc = this.note();
      if (!doc || key !== tabKeyFor("note", doc.id)) {
        return;
      }
      this.onTabBackgrounded();
      void this.runNoteBoundaryWork();
    });
    this.destroyRef.onDestroy(unsubDetach);

    // LOCK-REACTIVE re-mask — a ROOT effect via the EnvironmentInjector, NOT a
    // view effect: a view effect is FROZEN while `TabRouteReuseStrategy` keeps
    // this editor detached in a backgrounded tab (view effects only run inside
    // `refreshView()`'s CD traversal of ATTACHED views; verified against this
    // repo's @angular/core 22.0.5 — see the twin effect in
    // `detail.component.ts` for the full source-trace note). A root effect is
    // flushed by `ApplicationRef.synchronizeOnce()` BEFORE any view refresh,
    // so a "Lock all"/screen-share auto-relock re-masks this note even while
    // backgrounded, with no stale-plaintext frame on reattach. Env-injector
    // effects are NOT auto-destroyed with the component — hence the explicit
    // EffectRef destroy below (skipping it would leak a live effect per
    // closed tab). `untracked` keeps `folders.tree()` the ONLY dependency.
    const lockEffectRef = effect(
      () => {
        const tree = this.folders.tree();
        untracked(() => this.onLockTreeChanged(tree));
      },
      { injector: this.envInjector },
    );
    this.destroyRef.onDestroy(() => lockEffectRef.destroy());

    // FLUSH-BEFORE-FINALIZE registration (root-cause fix, 2026-07-17): while this
    // editor is EMBEDDED (the recording panel's companion note), register its
    // durable flush with the root {@link RecordingFlushService} so `RecorderStore.stop()`
    // can await the pending (debounced) save landing in the DB BEFORE `stop_recording`
    // runs its delete-if-empty — otherwise a Stop fired inside the autosave debounce
    // window loses the user's just-typed prose. `embedded()` is set once at mount and
    // never changes, so the register runs exactly once; the returned unregister is
    // wired to teardown so a destroyed editor is never flushed. The ROUTED
    // (`embedded()===false`) path never registers — its behavior is unchanged.
    let unregisterFlush: (() => void) | null = null;
    const registerEffectRef = effect(() => {
      if (this.embedded() && !unregisterFlush) {
        unregisterFlush = this.flushService.register(() =>
          this.flushPendingSave(),
        );
      }
    });
    this.destroyRef.onDestroy(() => {
      registerEffectRef.destroy();
      unregisterFlush?.();
    });
  }

  /**
   * FLUSH-BEFORE-FINALIZE (root-cause fix, 2026-07-17): force this editor's PENDING
   * (debounced) save to the backend NOW and resolve once the DB write has landed.
   * The recorder's Stop path awaits this via {@link RecordingFlushService} BEFORE
   * `stop_recording` so the companion note's just-typed body is durable before the
   * backend's delete-if-empty predicate is evaluated (else the user's prose is lost).
   *
   * Reuses the EXISTING save machinery — no second writer: it cancels the autosave
   * debounce and joins the SAME single-writer chain with a CHEAP text-only write. A
   * not-yet-started full save is deliberately downgraded for this deadline-sensitive
   * boundary; `dirtyFull` remains set, so indexing + vault export still run at the next
   * natural note boundary instead of delaying recording Stop. Latest signal state wins
   * (the payload is snapshotted when the queued save runs), so the character typed a
   * millisecond before Stop is included. A no-op (resolves at once)
   * while hydrating, when there is no loaded doc, or when the note is locked — the
   * chain never rejects (both save paths handle their own errors). The boolean
   * resolves `true` only when this request reached a confirmed backend write;
   * `false` keeps normal fire-and-forget autosave error handling intact while
   * allowing the recording flush witness to fail closed.
   */
  async flushPendingSave(): Promise<boolean> {
    const doc = this.note();
    if (this.hydrating || !doc || doc.locked) {
      return false;
    }
    await this.waitForAttachmentTasks();
    this.debounce.cancel(this.saveDebounceKey(doc.id));
    // Clicking Stop first blurs the textarea. `onBlur()` may therefore have already started the
    // exact save this durability boundary needs. Joining that same generation avoids a duplicate
    // write/retry pair (and avoids spending the two-second Stop budget twice). If a newer edit
    // arrived while an older save was in flight, the revisions differ and we enqueue the latest
    // payload behind it as usual.
    if (this.pendingSave === null && this.savingRevision === this.editRevision) {
      return this.saveChain;
    }
    // The same revision may have settled between typing and Stop (including an autosave failure).
    // Reuse that exact result: success is already durable, while retrying an unchanged failed
    // payload only doubles storage pressure and cannot strengthen the fail-closed witness.
    if (this.pendingSave === null && this.settledRevision === this.editRevision) {
      return this.settledRevisionSaved;
    }
    return this.queueSave("text", true);
  }

  /**
   * The shared "the user is done with this note for now" boundary work, run from BOTH
   * triggers in the constructor (hard close/quit, and a plain tab-switch detach): flush
   * the deferred full save (re-index + export) if there are unindexed edits, then attempt
   * auto-title. Sequenced (not parallel) so auto-title's `suggestNoteTitle` read — which
   * reads the last-SAVED body off the backend — sees the full-save's write if one just
   * happened, not a race between the two.
   */
  private async runNoteBoundaryWork(): Promise<void> {
    await this.waitForAttachmentTasks();
    if (this.dirtyFull) {
      await this.flushFull();
    }
    this.maybeAutoTitle();
  }

  /**
   * Feature B — ask the backend to auto-title a still-"Untitled" note from its body (the
   * on-device model when present, else a first-line heuristic; LOCAL-only). Called on
   * every natural "the user is done with this note for now" boundary — a hard close/quit
   * (the `destroyRef.onDestroy` callback) AND a plain tab switch (the `tabRouteReuse.onDetach`
   * callback, since `notes/:id` tabs are detached-not-destroyed on switch — see the
   * constructor comment). Fire-and-forget either way: we only reflect the new title back
   * onto the (persisting) tab strip when it resolves. Skips a locked/masked note and an
   * empty body up front (the backend re-checks both, plus re-checks the CURRENT title
   * immediately before writing, so calling this twice for the same note is at worst a
   * wasted on-device inference call, never a clobber).
   */
  private maybeAutoTitle(): void {
    const doc = this.note();
    if (!doc || doc.locked) {
      return;
    }
    const t = this.title().trim();
    if (t !== "" && t.toLowerCase() !== "untitled") {
      return; // the note already has a real title — never overwrite it
    }
    if (this.body().trim() === "") {
      return; // nothing to title
    }
    const id = doc.id;
    void this.ipc
      .suggestNoteTitle(id)
      .then((title) => {
        if (
          this.saveResponseStillApplies(id) &&
          title &&
          title.toLowerCase() !== "untitled"
        ) {
          this.tabsService.setTitle(tabKeyFor("note", id), title);
        }
      })
      .catch(() => {
        /* best-effort: an auto-title failure never surfaces */
      });
  }

  /**
   * React to a folder-tree lock-state change for THIS note. Only a genuine
   * seal/unseal TRANSITION acts (comparing the tree's exposure against the
   * loaded doc's `locked` flag) — an unrelated folder op (create/rename/move)
   * refreshes the tree too, and re-hydrating on those would clobber in-flight
   * keystrokes. On a seal: cancel pending saves, SYNCHRONOUSLY blank every
   * plaintext signal (hydrate a locally-masked doc — this also re-titles the
   * tab strip to "🔒 Locked" via `hydrate`'s `setTitle`, F3), then re-fetch to
   * converge on the backend's masked DTO. On an unseal: just re-fetch (the
   * unmasked doc re-hydrates). Note folders live in the same `folders` table
   * as meeting folders (`db.list_folders` has no kind filter), so the tree
   * carries this note's folder + its session-unlock state.
   */
  private onLockTreeChanged(tree: FolderNode[]): void {
    const doc = this.note();
    if (!doc) {
      return;
    }
    const node = this.findFolderNode(tree, doc.folderId);
    if (!node) {
      // Folder unresolvable from this tree (not yet loaded / unknown id) —
      // INDETERMINATE → the SAFE path: re-fetch (the gated backend returns
      // the masked doc if the folder is sealed). NEVER the skip path — a
      // possible unlocked→locked transition must not be suppressed (lock
      // review constraint). No local mask (that's only done on a POSITIVELY
      // derived seal, so a transient miss can't blank an open editor); the
      // re-hydrate may cost the caret, but this only occurs in the
      // pathological folder-missing-from-a-loaded-tree case.
      const missSeq = ++this.requestSeq;
      void this.fetchNote(doc.id, missSeq);
      return;
    }
    const sealed = node.locked && !node.unlocked;
    if (sealed === doc.locked) {
      // Consistent — no lock transition for THIS note. The skip keys ONLY on
      // the derived lock state (perf-audit fix 1a: skipping here is what
      // prevents both the per-tree-change IPC stampede AND a re-hydrate
      // clobbering in-flight keystrokes on unrelated folder ops).
      return;
    }
    if (sealed) {
      // Mask FIRST, synchronously — no pending save may resurrect plaintext,
      // and even a detached tab's signals hold nothing from this moment on.
      this.debounce.cancel(this.saveDebounceKey(doc.id));
      this.pendingSave = null;
      this.dirtyFull = false;
      this.attachments.set([]);
      // The merged "Related" panel (app-connections) owns its own gated fetch and
      // skips/clears itself while locked, and re-asks on the folders.tree() change
      // this same seal drives — so there are no stale relationship chips to blank
      // here (the old host-owned backlinks list is gone).
      this.hydrate({
        ...doc,
        locked: true,
        title: "🔒 Locked",
        markdown: "",
        tags: [],
        properties: {},
        exportedPath: null,
      });
      this.clearSelection();
      this.closeLinkPicker();
    }
    const seq = ++this.requestSeq;
    void this.fetchNote(doc.id, seq);
  }

  /** Depth-first lookup of a folder node by id across the forest. */
  private findFolderNode(nodes: FolderNode[], id: string): FolderNode | null {
    for (const n of nodes) {
      if (n.id === id) {
        return n;
      }
      const hit = this.findFolderNode(n.children ?? [], id);
      if (hit) {
        return hit;
      }
    }
    return null;
  }

  /**
   * Load the app config into a signal so `enabledActions` can gate the selection
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

  /**
   * Create an empty note (`/notes/new`) and replace the URL with its id.
   * Routes the transition through `TabsService.openNote` (not a plain
   * `router.navigate`) so the note gets its own tracked tab the MOMENT its
   * real id exists — `/notes/new` itself is deliberately never tab-tracked
   * (see `TabRouteReuseStrategy`'s header comment, risk #2 of the tabs plan).
   */
  private async createAndOpen(seq: number): Promise<void> {
    this.loading.set(true);
    this.error.set(null);
    try {
      const id = await this.notes.create(null, "Untitled");
      if (seq !== this.requestSeq) {
        return;
      }
      await this.tabsService.openNote(id, "Untitled", { replaceUrl: true });
    } catch (e) {
      if (seq === this.requestSeq) {
        this.error.set(this.errorCopy.humanize(e));
        this.loading.set(false);
      }
    }
  }

  /**
   * Re-load the currently-open note's body from the backend (EMBEDDED companion
   * editor, 2026-07-17). The recording panel keeps a SINGLE embedded editor mounted
   * for the whole recording (the Note tab is HIDDEN, not destroyed, when the user is
   * on Ask Brain), so returning to the Note tab no longer re-mounts the editor to
   * pick up an Ask-Brain "Add to note" append / an external edit — it calls this
   * instead. A stale-guarded re-fetch (a new request token drops a late reply). No-op
   * when there is no loaded doc yet. Deliberately re-hydrates the whole doc (the
   * append lands server-side, so the fresh body IS canonical) — the same path a
   * re-mount used to run, minus the destroy/recreate churn.
   */
  reload(): void {
    const doc = this.note();
    if (!doc) {
      return;
    }
    const seq = ++this.requestSeq;
    void this.fetchNote(doc.id, seq);
  }

  /** Fetch one note, hydrate the edit signals, dropping a stale reply. */
  private async fetchNote(id: string, seq: number): Promise<void> {
    this.loading.set(true);
    this.error.set(null);
    // A previous tab/note's image bytes must never survive across an owner load.
    this.attachments.set([]);
    try {
      const doc = await this.ipc.getNote(id);
      if (seq !== this.requestSeq) {
        return;
      }
      let attachments: NoteAttachmentDto[] = [];
      if (!doc.locked) {
        try {
          const rows = await this.ipc.listNoteAttachments("note", id);
          if (seq !== this.requestSeq) {
            return;
          }
          attachments = Array.isArray(rows) ? rows : [];
        } catch {
          // The note remains editable when only its optional image load fails.
          this.toast.danger("Couldn’t load this note’s images.");
        }
      }
      // The attachment read may have raced a root lock effect.
      if (seq !== this.requestSeq) {
        return;
      }
      this.attachments.set(attachments);
      this.hydrate(doc);
    } catch (e) {
      if (seq === this.requestSeq) {
        this.error.set(this.friendlyLoadError(e));
        this.note.set(null);
        this.attachments.set([]);
      }
    } finally {
      if (seq === this.requestSeq) {
        this.loading.set(false);
      }
    }
  }

  /**
   * A clean, non-technical message for the "couldn't open this note" state.
   *
   * `get_note` rejects with `[note-missing]` (`errcode::NOTE_MISSING`) for an unknown id — normally
   * the tab-strip's `content-deleted` fan-out (`TabsService`) closes a stale tab before this is
   * ever reached, but a note opened a different way (a stale bookmark, a link from a surface that
   * does not go through `TabsService`) can still land here after a delete.
   *
   * Folded into `ErrorCopyService` under the "note-load" context — the "no note " prose test is
   * gone, and the non-matching arm no longer falls through to the raw backend string.
   */
  private friendlyLoadError(e: unknown): string {
    return this.errorCopy.humanize(e, "note-load");
  }

  /** Apply a loaded/reconciled doc into the edit signals (no autosave feedback). */
  private hydrate(doc: NoteDoc): void {
    this.hydrating = true;
    // Never let a settled write for a previously loaded owner serve as this document's witness.
    this.editRevision += 1;
    this.settledRevision = -1;
    this.settledRevisionSaved = false;
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
    this.applyPreviewDefault(doc, parsed.body);
    this.saveState.set("idle");
    this.dirtyFull = false;
    this.hydrating = false;
    // Adopt the loaded title into the tab strip (a no-op if this note isn't
    // tab-tracked, e.g. a direct routerLink open elsewhere in the app).
    this.tabsService.setTitle(tabKeyFor("note", doc.id), doc.title || "Untitled");
  }

  /**
   * A note with something to read OPENS in Preview; anything else opens in Edit.
   *
   * Reading is the common case — an existing note was landing in a raw-markdown
   * textarea, so every open began by showing source rather than the note. The
   * three exclusions are not stylistic:
   *
   *  - **empty body** — a brand-new note (`/notes/new` creates one, then this
   *    editor hydrates it) has nothing to render, and a read-only empty pane is a
   *    dead end where the user meant to start typing.
   *  - **locked** — a sealed note's body is masked server-side, so there is
   *    nothing to preview; the template shows the lock gate instead, and
   *    `setPreview` itself refuses while locked. Defaulting past that guard would
   *    be the one place preview mode is reachable for a locked document.
   *  - **embedded** — the recording panel's Note tab is a capture surface. It
   *    already force-disables preview via `previewActive`; keeping the signal
   *    false too honors that component's belt-and-braces contract.
   */
  private opensInPreview(doc: NoteDoc, body: string): boolean {
    return !doc.locked && !this.embedded() && body.trim().length > 0;
  }

  /** Apply {@link opensInPreview} once per opened document — see {@link previewDefaultedFor}. */
  private applyPreviewDefault(doc: NoteDoc, body: string): void {
    const key = `${doc.id}:${doc.locked}`;
    if (key === this.previewDefaultedFor) {
      return;
    }
    this.previewDefaultedFor = key;
    this.preview.set(this.opensInPreview(doc, body));
  }

  // ── Title ────────────────────────────────────────────────────────────────

  onTitleInput(event: Event): void {
    const value = (event.target as HTMLInputElement).value;
    this.title.set(value);
    // Live tab-title sync (root-cause fix, 2026-07-15): update the tab-strip label
    // OPTIMISTICALLY from the typed value, independent of whether the debounced
    // autosave has landed yet. Previously `tabsService.setTitle` was only called
    // from inside `saveText`/`saveFull` on a SUCCESSFUL save, so a save that failed
    // (see the busy-DB fix above) — or simply hadn't fired yet — left the tab
    // showing its stale/creation-time label even though the user had typed a real
    // title. The user's typed text IS the intent; the tab should reflect it right
    // away, and keep it even if the save later fails (a no-op call if this note
    // isn't tab-tracked, or if the title is empty/unchanged — see `TabsService.setTitle`).
    const doc = this.note();
    if (doc) {
      this.tabsService.setTitle(tabKeyFor("note", doc.id), value || "Untitled");
    }
    this.scheduleSave();
  }

  // ── Body ───────────────────────────────────────────────────────────────

  onBodyInput(event: Event): void {
    const el = event.target as HTMLTextAreaElement;
    this.body.set(el.value);
    this.autoGrow();
    this.maybeOpenSlash(el);
    this.maybeOpenLinkPicker(el);
    this.scheduleSave();
  }

  /** Preserve the exact caret before the explicit image button opens a picker. */
  rememberImageInsertion(): void {
    const el = this.bodyArea()?.nativeElement;
    if (!el) {
      return;
    }
    this.imageInsertion = {
      start: el.selectionStart,
      end: el.selectionEnd,
    };
  }

  /** Open the hidden local-only raster picker without webview filesystem paths. */
  openImagePicker(): void {
    const doc = this.note();
    if (!doc || doc.locked || this.importingImages() > 0) {
      return;
    }
    if (!this.imageInsertion) {
      this.rememberImageInsertion();
    }
    this.imageFileInput()?.nativeElement.click();
  }

  /** Import explicit picker files at the caret captured before focus moved. */
  onImageFilesSelected(event: Event): void {
    const input = event.target as HTMLInputElement;
    const plan = this.attachmentService.planFromFiles(
      input.files ?? [],
      this.availableImageSlots(),
    );
    input.value = "";
    this.notifyAttachmentWarnings(plan);
    if (!plan.segments.some((segment) => segment.kind === "image")) {
      this.imageInsertion = null;
      return;
    }
    const el = this.bodyArea()?.nativeElement;
    const selection = this.imageInsertion ?? {
      start: el?.selectionStart ?? this.body().length,
      end: el?.selectionEnd ?? this.body().length,
    };
    this.imageInsertion = null;
    this.startAttachmentImport(plan, selection.start, selection.end);
  }

  /** Cmd-V: intercept only when the clipboard contains an actual safe image blob. */
  onBodyPaste(event: ClipboardEvent): void {
    const data = event.clipboardData;
    const el = event.target as HTMLTextAreaElement;
    if (!data) {
      return;
    }
    const plan = this.attachmentService.planFromTransfer(
      data,
      this.availableImageSlots(),
    );
    this.notifyAttachmentWarnings(plan);
    if (!plan.segments.some((segment) => segment.kind === "image")) {
      // Plain text keeps the browser's native paste behavior and undo semantics.
      return;
    }
    event.preventDefault();
    this.startAttachmentImport(plan, el.selectionStart, el.selectionEnd);
  }

  /** Allow a local image file drop; remote URL-only drops remain inert. */
  onBodyDragOver(event: DragEvent): void {
    if (event.dataTransfer?.types.includes("Files")) {
      event.preventDefault();
    }
  }

  onBodyDrop(event: DragEvent): void {
    const data = event.dataTransfer;
    const el = event.target as HTMLTextAreaElement;
    if (!data) {
      return;
    }
    const plan = this.attachmentService.planFromTransfer(
      data,
      this.availableImageSlots(),
    );
    this.notifyAttachmentWarnings(plan);
    if (!plan.segments.some((segment) => segment.kind === "image")) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    this.startAttachmentImport(plan, el.selectionStart, el.selectionEnd);
  }

  /** Insert stable markers first, then replace each by identity as imports finish. */
  private startAttachmentImport(
    plan: AttachmentPastePlan,
    selectionStart: number,
    selectionEnd: number,
  ): void {
    const doc = this.note();
    if (!doc || doc.locked) {
      return;
    }
    const pending = this.attachmentService.pendingPlan(plan);
    if (!pending.images.length || !pending.markdown) {
      return;
    }
    const edit = insertMarkdownBlock(
      this.body(),
      selectionStart,
      selectionEnd,
      pending.markdown,
    );
    this.applyBodyEdit(edit, true);
    this.dirtyFull = true;
    this.saveState.set("saving");
    this.importingImages.update((count) => count + pending.images.length);

    const task = this.performAttachmentImport(doc.id, pending.images);
    this.attachmentTasks.add(task);
    void task.finally(() => this.attachmentTasks.delete(task));
  }

  private async performAttachmentImport(
    noteId: string,
    pendingImages: ReturnType<NoteAttachmentService["pendingPlan"]>["images"],
  ): Promise<void> {
    try {
      // Sequential decode/encode bounds peak RGBA + canvas memory.
      for (const { id, image } of pendingImages) {
        try {
          const attachment = await this.attachmentService.importImage(
            "note",
            noteId,
            image,
          );
          const current = this.note();
          if (!current || current.id !== noteId || current.locked) {
            void this.attachmentService
              .deleteAttachment("note", noteId, attachment.id)
              .catch(() => undefined);
            continue;
          }
          const replaced = this.replacePendingAttachment(
            id,
            this.attachmentService.attachmentMarkdown(attachment, image.alt),
          );
          if (replaced) {
            this.attachments.update((rows) =>
              rows.some((row) => row.id === attachment.id)
                ? rows
                : [...rows, attachment],
            );
          } else {
            void this.attachmentService
              .deleteAttachment("note", noteId, attachment.id)
              .catch(() => undefined);
          }
        } catch (error) {
          if (this.note()?.id === noteId && !this.note()?.locked) {
            this.replacePendingAttachment(id, "");
            this.toast.danger(this.errorCopy.humanize(error));
          }
        }
      }
    } finally {
      this.importingImages.update((count) =>
        Math.max(0, count - pendingImages.length),
      );
      if (this.note()?.id === noteId && !this.note()?.locked) {
        this.scheduleSave();
      }
    }
  }

  /** Replace one unique marker while preserving a caret in concurrently typed text. */
  private replacePendingAttachment(pendingId: string, replacement: string): boolean {
    const el = this.bodyArea()?.nativeElement;
    const edit = replacePendingAttachmentUri(
      this.body(),
      pendingId,
      replacement,
      el?.selectionStart ?? this.body().length,
      el?.selectionEnd ?? this.body().length,
    );
    if (!edit) {
      return false;
    }
    this.applyBodyEdit(edit, false);
    return edit.canonicalSlot;
  }

  private availableImageSlots(): number {
    if (this.importingImages() > 0) {
      return 0;
    }
    return Math.max(0, MAX_NOTE_ATTACHMENTS - this.attachments().length);
  }

  private applyBodyEdit(edit: MarkdownEdit, focus: boolean): void {
    this.body.set(edit.value);
    const el = this.bodyArea()?.nativeElement;
    if (el) {
      el.value = edit.value;
      el.setSelectionRange(edit.selectionStart, edit.selectionEnd);
      if (focus) {
        el.focus();
      }
    }
    this.autoGrow();
  }

  private notifyAttachmentWarnings(plan: AttachmentPastePlan): void {
    if (plan.skippedExternalImages) {
      this.toast.push(
        "External images were skipped to protect your privacy.",
        "info",
      );
    }
    if (plan.skippedUnsupportedImages) {
      this.toast.danger("Some images were skipped. Use PNG, JPEG, or WebP files.");
    }
    if (plan.skippedTooManyImages) {
      this.toast.danger(`A note can contain up to ${MAX_NOTE_ATTACHMENTS} images.`);
    }
  }

  private async waitForAttachmentTasks(): Promise<void> {
    while (this.attachmentTasks.size > 0) {
      await Promise.allSettled([...this.attachmentTasks]);
    }
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
    const doc = this.note();
    if (this.hydrating || !doc || doc.locked) {
      return;
    }
    this.editRevision += 1;
    this.dirtyFull = true;
    this.saveState.set("saving");
    // Pending URIs are transient UI state and must never cross IPC.
    if (this.importingImages() > 0 || this.body().includes("murmur-pending://")) {
      return;
    }
    this.debounce.schedule(
      this.saveDebounceKey(doc.id),
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
   * resolves once this request's save attempt has settled. `true` means the
   * backend confirmed persistence; `false` means skipped/failed, while the
   * chain remains fulfilled so debounced fire-and-forget callers never create
   * unhandled rejections and later saves are not wedged.
   */
  private queueSave(
    kind: "text" | "full",
    replacePending = false,
  ): Promise<boolean> {
    // Blur and Stop can ask for the same cheap write while that exact edit generation is already
    // executing. Join it before appending to the chain; a newer generation still queues normally,
    // and a `full` boundary is never deduped because it also owes re-index/export work.
    if (
      kind === "text" &&
      this.pendingSave === null &&
      this.savingRevision === this.editRevision
    ) {
      return this.saveChain;
    }
    const alreadyQueued = this.pendingSave !== null;
    this.pendingSave = replacePending
      ? kind
      : this.pendingSave === "full" || kind === "full"
        ? "full"
        : "text";
    if (!alreadyQueued) {
      this.saveChain = this.saveChain
        .then(() => this.runPendingSave())
        // Defensive: an unexpected rejected link must neither wedge future
        // autosaves nor become a false-positive durability witness.
        .catch(() => false);
    }
    return this.saveChain;
  }

  /** Execute (and clear) the coalesced pending save. Runs on the chain only. */
  private async runPendingSave(): Promise<boolean> {
    await this.waitForAttachmentTasks();
    const kind = this.pendingSave;
    this.pendingSave = null;
    if (this.body().includes("murmur-pending://")) {
      this.saveState.set("error");
      this.saveErrorMessage.set(
        "An image is still being resolved. Remove its pending marker or try again.",
      );
      return false;
    }
    const revision = this.editRevision;
    this.savingRevision = revision;
    try {
      let saved = false;
      if (kind === "full") {
        saved = await this.saveFull();
      } else if (kind === "text") {
        saved = await this.saveText();
      }
      this.settledRevision = revision;
      this.settledRevisionSaved = saved;
      return saved;
    } finally {
      if (this.savingRevision === revision) {
        this.savingRevision = null;
      }
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
   * True for a save rejection that is inherently NOT retryable: a lock refusal
   * (`AppError::Locked`, e.g. a screen-share auto-relock racing the save) or a
   * missing-row refusal (`AppError::InvalidArg("no note {id}")` — the
   * stale-tab-after-delete case). Retrying either would fail identically every
   * time, so a bounded retry is only useful for the REMAINING error domains
   * (chiefly a transient `AppError::Storage` from write contention with a
   * concurrent background writer — the brain re-index / org-feed sync tick /
   * memory consolidation — now itself mitigated at the source by the
   * `busy_timeout` fix in `Db::open_with_key`, but a retry is still a correct,
   * cheap belt-and-suspenders for whatever transient storage hiccup slips
   * through).
   *
   * BUG FIXED HERE (P3). This tested `message.includes("Locked")` — CAPITAL L — while every
   * producer is lowercase: `AppError`'s `Display` is `#[error("locked: {0}")]` and the note
   * write-gates in `commands/notes.rs` all go through it. So the lock arm NEVER fired: a save into
   * a folder that sealed under the user fell into the retry branch, waited out an 800 ms backoff,
   * failed identically, and only then surfaced. The guard is now the `[note-locked]` /
   * `[folder-locked]` / `[note-missing]` CODE, which cannot have a casing bug.
   */
  private isUnretryableSaveError(e: unknown): boolean {
    const code = this.errorCopy.codeOf(e);
    return (
      code === "note-locked" ||
      code === "folder-locked" ||
      code === "note-missing"
    );
  }

  /**
   * The `DebounceService` key for every autosave/retry timer THIS note owns —
   * scoped PER NOTE ID (root-cause fix, 2026-07-15, widened from the retry-only
   * fix of the same date after adversarial review of PR #332 found the other 7
   * call sites still shared one bare literal). `DebounceService` is a
   * `providedIn: 'root'` SINGLETON shared by every open `NoteEditorComponent`
   * instance, and `notes/:id` tabs stay ALIVE-BUT-DETACHED when backgrounded
   * (`TabRouteReuseStrategy` — `shouldDetach`/`shouldAttach`), so a bare
   * literal key let ANY open note's `schedule`/`cancel` call silently
   * `clearTimeout` a DIFFERENT open note's pending timer under the same key —
   * for the retry key that stranded `saveState` on `"saving"` forever; for the
   * PRIMARY autosave key (`scheduleSave`/`onBlur`/`flushFull`/`exportNote`/
   * `share`/`doDelete`/`onLockTreeChanged`) it is worse: a cancelled autosave
   * timer with no replacement scheduled is a SILENT, invisible content-loss —
   * no error, no stuck indicator, just a keystroke that never persists.
   * Scoping every one of these 8 call sites per note id gives each open tab
   * its own independent timer slot in the shared singleton.
   */
  private saveDebounceKey(noteId: string): string {
    return `note-editor-save:${noteId}`;
  }

  /**
   * One bounded retry for a transient save failure: schedule a single
   * re-attempt of `attempt` after a short backoff via the app's ONE sanctioned
   * debounce timer (`DebounceService` — never a raw component `setTimeout`,
   * angular-zoneless §5). Resolves the retry's own result; the caller decides
   * what "still failed after the retry" means. NOT a retry loop — exactly one
   * extra attempt, then the caller surfaces the real error. Keyed per note id
   * (see {@link saveDebounceKey}'s doc) — deliberately a DIFFERENT key than
   * the primary autosave/cancel timer so a retry can never coalesce with (or
   * get cancelled by) an in-flight autosave for the SAME note.
   */
  private retryOnce<T>(noteId: string, attempt: () => Promise<T>): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      this.debounce.schedule(
        `note-editor-save-retry:${noteId}`,
        () => {
          attempt().then(resolve, reject);
        },
        800,
      );
    });
  }

  /**
   * Shared failure path for both save paths: classify the error, retry ONCE for a retryable
   * (non-lock, non-missing-row) failure, and only THEN settle `saveState`/`saveErrorMessage`/toast.
   * `noteId` scopes the bounded retry's debounce key so concurrent open note tabs never cancel
   * each other's retry (see {@link retryOnce}).
   *
   * Classification is by `[code]`, never by prose — see {@link isUnretryableSaveError} for the
   * casing bug that motivated it. The settled message is the sentence `ErrorCopyService` owns, so
   * an unrecognised failure reads "Couldn’t save this note. Please try again." instead of a Rust
   * diagnostic.
   */
  private async handleSaveFailure<T>(
    noteId: string,
    e: unknown,
    retryAttempt: () => Promise<T>,
    onRetrySuccess: (result: T) => void,
  ): Promise<boolean> {
    // A lock transition or navigation may have replaced the editor state while the failed IPC
    // was in flight. Never let its retry/error UI mutate the new or synchronously masked owner.
    if (!this.saveResponseStillApplies(noteId)) {
      return false;
    }
    const message = this.errorCopy.humanize(e, "note-save");
    const code = this.errorCopy.codeOf(e);
    if (code === "note-missing") {
      // Stale-tab-after-delete: this note id no longer exists server-side —
      // retrying a save against it can never succeed.
      this.saveState.set("error");
      this.saveErrorMessage.set("This note no longer exists.");
      this.toast.danger(message);
      return false;
    }
    if (code === "note-locked" || code === "folder-locked") {
      this.saveState.set("error");
      this.saveErrorMessage.set(message);
      this.toast.danger("This note is locked — unlock its folder to edit.");
      return false;
    }
    if (!this.isUnretryableSaveError(e)) {
      try {
        const result = await this.retryOnce(noteId, retryAttempt);
        if (!this.saveResponseStillApplies(noteId)) {
          // The retry did persist, but its response belongs to an authorization/navigation state
          // that has since been revoked. Treat durability as success without touching current UI.
          return true;
        }
        onRetrySuccess(result);
        this.saveState.set("saved");
        this.saveErrorMessage.set(null);
        return true;
      } catch (retryError) {
        if (!this.saveResponseStillApplies(noteId)) {
          return false;
        }
        this.saveState.set("error");
        this.saveErrorMessage.set(
          this.errorCopy.humanize(retryError, "note-save"),
        );
        return false;
      }
    }
    this.saveState.set("error");
    this.saveErrorMessage.set(message);
    return false;
  }

  /**
   * A save response is UI-authoritative only while this exact note is still the visible,
   * unlocked owner. `onLockTreeChanged` masks synchronously, but an already-running Promise can
   * settle later; accepting it would flip `locked` back to false and restore a plaintext tab title.
   */
  private saveResponseStillApplies(noteId: string): boolean {
    const current = this.note();
    return current?.id === noteId && !current.locked;
  }

  /**
   * CHEAP persist — text only (no re-index, no export). Used by the frequent
   * autosave + blur so typing never triggers the e5 re-embed. The DB text is
   * canonical, so nothing is lost; the brain index catches up on the next full save.
   * Runs ONLY via {@link runPendingSave} on the single-writer chain.
   */
  private async saveText(): Promise<boolean> {
    const doc = this.note();
    if (!doc || doc.locked || this.importingImages() > 0) {
      return false;
    }
    const { title, markdown } = this.currentPayload();
    this.saveState.set("saving");
    try {
      const updatedAt = await this.ipc.saveNoteText(doc.id, title, markdown);
      if (!this.saveResponseStillApplies(doc.id)) {
        return true;
      }
      this.note.update((cur) => (cur ? { ...cur, updatedAt } : cur));
      this.saveState.set("saved");
      this.saveErrorMessage.set(null);
      // Live tab-title sync (bug fix, 2026-07-12): `hydrate()` only sets the tab
      // title on INITIAL load/re-mask, so a rename previously kept showing the
      // stale title until the tab was closed + reopened. Every committed save
      // (debounced autosave, not every keystroke) re-syncs it — a no-op if this
      // note isn't tab-tracked (see `TabsService.setTitle`). Also mirrored
      // OPTIMISTICALLY from every keystroke in `onTitleInput` now, so this call
      // is a reconciling no-op in the common case.
      this.tabsService.setTitle(tabKeyFor("note", doc.id), title || "Untitled");
      return true;
    } catch (e) {
      return this.handleSaveFailure(
        doc.id,
        e,
        () => this.ipc.saveNoteText(doc.id, title, markdown),
        (updatedAt) => {
          this.note.update((cur) => (cur ? { ...cur, updatedAt } : cur));
          this.tabsService.setTitle(tabKeyFor("note", doc.id), title || "Untitled");
        },
      );
    }
  }

  /**
   * FULL save (fire-and-forget) — persist + RE-INDEX (brain) + vault re-export via
   * `updateNoteDoc`. Requested ONLY on natural boundaries (Preview, editor close,
   * retry) so the e5 re-embed never fires per keystroke-pause. NOT awaited by
   * navigation, so leaving is instant and the backend catches up. Joins the same
   * single-writer chain as the cheap saves, superseding any pending cheap save.
   */
  private flushFull(): Promise<boolean> {
    const doc = this.note();
    if (doc) {
      this.debounce.cancel(this.saveDebounceKey(doc.id));
    }
    return this.queueSave("full");
  }

  /**
   * The full-save body — persist + re-index + export, clearing `dirtyFull` on
   * success. Runs ONLY via {@link runPendingSave} on the single-writer chain.
   */
  private async saveFull(): Promise<boolean> {
    const doc = this.note();
    if (!doc || doc.locked || this.importingImages() > 0) {
      return false;
    }
    const { title, markdown } = this.currentPayload();
    this.saveState.set("saving");
    try {
      const fresh = await this.ipc.updateNoteDoc(doc.id, title, markdown);
      if (!this.saveResponseStillApplies(doc.id)) {
        return true;
      }
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
      this.saveErrorMessage.set(null);
      // Live tab-title sync (bug fix, 2026-07-12) — see the twin call in {@link saveText}.
      this.tabsService.setTitle(tabKeyFor("note", doc.id), title || "Untitled");
      return true;
    } catch (e) {
      return this.handleSaveFailure(
        doc.id,
        e,
        () => this.ipc.updateNoteDoc(doc.id, title, markdown),
        (fresh) => {
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
          this.tabsService.setTitle(tabKeyFor("note", doc.id), title || "Untitled");
        },
      );
    }
  }

  /** Blur handler (title / body) — flush the pending CHEAP save immediately. */
  onBlur(): void {
    const doc = this.note();
    if (doc && this.saveState() === "saving") {
      this.debounce.cancel(this.saveDebounceKey(doc.id));
      void this.queueSave("text");
    }
  }

  /** Retry a failed save — the full path (re-index + export). */
  retrySave(): void {
    void this.flushFull();
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

  // ── Properties (schema-driven, Feature C) ─────────────────────────────────
  // The `properties` signal stays Record<string,string>: every widget commit
  // COERCES its raw value for the schema kind then stores the canonical STRING
  // via `formatForYaml`, so `serializeDoc`'s byte-exact YAML round-trip is
  // untouched. The Add-property menu offers the folder schema's DEFINED keys
  // first, then a "New property" escape hatch that persists a new field to the
  // folder schema (via `notes.saveSchema`) before adding it to this note.

  /** Open the Add-property menu (defined-keys picker; not yet the new-property form). */
  startAddProp(): void {
    this.addingProp.set(true);
    this.definingProp.set(false);
    this.propKeyDraft.set("");
    this.propKindDraft.set("text");
  }

  /** Close the Add-property menu / new-property form. */
  cancelAddProp(): void {
    this.addingProp.set(false);
    this.definingProp.set(false);
  }

  /**
   * Add an already-DEFINED schema field to this note (menu item). Seeds a coerced
   * default for its kind (a checkbox starts `false`, everything else empty) so the
   * widget renders immediately, then autosaves. No-op for `tags` (its own row).
   */
  addSchemaProp(field: PropertySchemaField): void {
    if (field.key.toLowerCase() === "tags") {
      this.cancelAddProp();
      return;
    }
    const seed = formatForYaml(coerceForKind("", field.kind));
    this.properties.update((props) => ({ ...props, [field.key]: seed }));
    this.cancelAddProp();
    this.scheduleSave();
  }

  /** Open the "New property" sub-form (a fresh key + kind, persisted to the schema). */
  startDefineProp(): void {
    this.definingProp.set(true);
    this.propKeyDraft.set("");
    this.propKindDraft.set("text");
  }

  onPropKeyInput(event: Event): void {
    this.propKeyDraft.set((event.target as HTMLInputElement).value);
  }
  onPropKindInput(event: Event): void {
    this.propKindDraft.set(
      (event.target as HTMLSelectElement).value as PropertyKind,
    );
  }

  /**
   * Commit a BRAND-NEW property: PERSIST its `{key, kind}` to the folder schema
   * (so the widget + the Table/Board views all see the kind), then add it to this
   * note with a coerced default. If the schema save fails the note property is
   * NOT added (the value would render as bare text with no kind), and a toast
   * surfaces the failure. A duplicate key just re-focuses (no-op on the schema).
   */
  async createSchemaProperty(): Promise<void> {
    const key = this.propKeyDraft().trim();
    const kind = this.propKindDraft();
    const doc = this.note();
    if (!key || key.toLowerCase() === "tags" || !doc || doc.locked) {
      this.cancelAddProp();
      return;
    }
    // Already a property on this note? Just close (no duplicate schema entry).
    if (key in this.properties()) {
      this.cancelAddProp();
      return;
    }
    const existing = this.folderSchema().find((f) => f.key === key);
    const nextSchema: PropertySchemaField[] = existing
      ? this.folderSchema()
      : [...this.folderSchema(), { key, kind, options: [] }];
    try {
      if (!existing) {
        await this.notes.saveSchema(doc.folderId, nextSchema);
      }
      const seed = formatForYaml(coerceForKind("", existing?.kind ?? kind));
      this.properties.update((props) => ({ ...props, [key]: seed }));
      this.cancelAddProp();
      this.scheduleSave();
    } catch {
      this.toast.danger("Couldn’t save the property. Please try again.");
    }
  }

  /**
   * Edit a TEXT/DATE/NUMBER/SELECT property from an `<input>`/`<select>` change.
   * Coerces the raw value for the row's schema kind and stores the canonical
   * string, so the YAML round-trip is untouched. `kind` comes from the row's
   * resolved schema (`text` when undefined).
   */
  editProp(key: string, kind: PropertyKind, event: Event): void {
    const raw = (event.target as HTMLInputElement | HTMLSelectElement).value;
    this.setPropRaw(key, kind, raw);
  }

  /** Set a SELECT property from a picked option value (schema-aware coerce). */
  setSelectProp(key: string, kind: PropertyKind, value: string): void {
    this.setPropRaw(key, kind, value);
  }

  /** Toggle a CHECKBOX property (from `mur-toggle`'s ngModelChange-equivalent). */
  setCheckboxProp(key: string, checked: boolean): void {
    const raw = formatForYaml({ kind: "checkbox", value: checked });
    this.properties.update((props) => ({ ...props, [key]: raw }));
    this.scheduleSave();
  }

  /** Read a CHECKBOX property's current boolean (for `mur-toggle`'s [checked]-equivalent). */
  checkboxValue(value: string): boolean {
    const coerced = coerceForKind(value, "checkbox");
    return coerced.kind === "checkbox" ? coerced.value : false;
  }

  /**
   * Whether a SELECT row's current value is one of its schema options. When it is
   * NOT, the value is an out-of-schema passthrough — the template keeps it visible
   * as an extra option so it is never silently dropped.
   */
  selectValueIsOffSchema(value: string, options: string[]): boolean {
    return value.trim().length > 0 && !options.includes(value);
  }

  /** Coerce `raw` for `kind`, store the canonical string, autosave. Shared writer. */
  private setPropRaw(key: string, kind: PropertyKind, raw: string): void {
    const canonical = formatForYaml(coerceForKind(raw, kind));
    this.properties.update((props) => ({ ...props, [key]: canonical }));
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

  async setPreview(on: boolean): Promise<void> {
    if (!on) {
      this.preview.set(false);
      return;
    }
    await this.waitForAttachmentTasks();
    if (this.dirtyFull && !(await this.flushFull())) {
      return;
    }
    if (this.saveState() === "error" || this.note()?.locked) {
      return;
    }
    this.preview.set(true);
    // No textarea to select in Preview — drop the floating bubble / Brain popover
    // and the link picker (its trigger position lives in the textarea).
    this.clearSelection();
    this.closeLinkPicker();
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
        replacement = this.wikilinkText(selected || "Note");
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
   * The `[[Title]]` wikilink markdown for `title` — the ONE place this string is
   * built, shared by the `wikilink` toolbar op (wraps a selection) and the link
   * picker (Fix 2: replaces the `[[` trigger + typed query with a picked title).
   */
  private wikilinkText(title: string): string {
    return `[[${title}]]`;
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

    // The link picker takes priority over everything else when open (mirrors
    // the slash menu's own priority below) — Backspace back past the trigger
    // closes it (handled in onBodyInput once the trigger text is gone).
    if (this.linkPickerTrigger()) {
      if (this.handleLinkPickerKey(event)) {
        return;
      }
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
      // When embedded, "Ask Brain" is item 0 — default the keyboard highlight to the
      // first BLOCK (index 1) so `/`+Enter still inserts a heading; Ask stays the
      // visible top item (click or ArrowUp). Routed editor is unchanged (index 0).
      this.slashIndex.set(this.embedded() ? 1 : 0);
      this.slashOpen.set(true);
    } else if (!line.startsWith("/") || line.includes(" ")) {
      this.slashOpen.set(false);
    }
  }

  /** Handle ↑/↓/Enter/Esc in the open slash menu. Returns true when consumed. */
  private handleSlashKey(event: KeyboardEvent): boolean {
    const items = this.slashItems();
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        this.slashIndex.update((i) => (i + 1) % items.length);
        return true;
      case "ArrowUp":
        event.preventDefault();
        this.slashIndex.update((i) => (i - 1 + items.length) % items.length);
        return true;
      case "Enter":
        event.preventDefault();
        this.pickSlash(items[this.slashIndex()]);
        return true;
      case "Escape":
        event.preventDefault();
        this.slashOpen.set(false);
        return true;
    }
    return false;
  }

  /**
   * Insert a slash-menu block, replacing the `/` trigger, caret at the `$` — OR,
   * for the `linkToNote` entry (no `snippet`), open the SAME link picker the raw
   * `[[` keystroke opens, anchored where the `/` trigger was (parity requirement).
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
    if (item.id === "askBrain") {
      // Summon the Ask-Brain panel — remove the bare `/` trigger first so no
      // stray slash is left in the note, then let the recording host open the
      // panel. Caret returns to the (now-empty) line start.
      this.replaceRange(el, lineStart, pos, "", lineStart, lineStart);
      this.askBrain.emit();
      return;
    }
    if (item.id === "image") {
      // Remove the `/` trigger, preserve that exact block position, then let
      // the hidden picker take focus. Async work targets a stable marker.
      this.replaceRange(el, lineStart, pos, "", lineStart, lineStart);
      this.imageInsertion = { start: lineStart, end: lineStart };
      this.openImagePicker();
      return;
    }
    if (item.id === "linkToNote" || item.snippet === undefined) {
      // Replace the `/` trigger with `[[` (the natural Obsidian trigger text),
      // then open the picker anchored right after it.
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
    // Replace from the `/` (line start) through the caret with the snippet.
    const caretMarker = item.snippet.indexOf("$");
    const snippet = item.snippet.replace("$", "");
    const caret = lineStart + (caretMarker === -1 ? snippet.length : caretMarker);
    this.replaceRange(el, lineStart, pos, snippet, caret, caret);
  }

  // ── Link picker (Obsidian-style `[[` autocomplete, Fix 2) ────────────────

  /**
   * Open the link picker when the two characters immediately before the caret
   * are `[[` and it isn't already open — the raw-keystroke trigger path (the
   * slash-menu path opens it directly via {@link openLinkPickerAt}). Closes it
   * when the trigger text no longer starts with `[[` right before the caret
   * (e.g. Backspace past it, or the caret moved away) or a newline/`]]` was typed.
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
    // Already open — close it if the caret backed up before the trigger start,
    // a newline was typed since, or the query just got a closing `]]`.
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

  /** Open the picker with its trigger anchored at `start` (the text position right after `[[`). */
  private openLinkPickerAt(el: HTMLTextAreaElement, start: number): void {
    const rect = this.selectionRect(el, start, start);
    if (!rect) {
      return;
    }
    this.linkPickerActiveIndex.set(0);
    this.linkPickerCandidates.set([]);
    this.linkPickerTrigger.set({ start, rect });
  }

  /** ↑/↓/Enter/Esc while the picker is open. Returns true when consumed. */
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

  /** Re-measure the live textarea caret after an ancestor/window scroll. */
  repositionLinkPicker(): void {
    if (this.linkPickerRepositionQueued) {
      return;
    }
    this.linkPickerRepositionQueued = true;
    afterNextRender(
      () => {
        this.linkPickerRepositionQueued = false;
        const trigger = this.linkPickerTrigger();
        const el = this.bodyArea()?.nativeElement;
        if (!trigger || !el) {
          return;
        }
        const rect = this.selectionRect(el, trigger.start, trigger.start);
        if (rect) {
          this.linkPickerTrigger.set({ start: trigger.start, rect });
        }
      },
      { injector: this.injector },
    );
  }

  /**
   * A candidate was picked (click or Enter): replace the trigger span (`[[` +
   * whatever was typed since) with the wikilink text via the SAME `wikilink`
   * formatting op `format("wikilink")` builds (Fix 2 reuses it, never duplicates
   * the `[[Title]]` construction), then collapse the caret after it.
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

  /** Close the picker without inserting anything (Esc / outside click / caret moved away). */
  closeLinkPicker(): void {
    this.linkPickerTrigger.set(null);
    this.linkPickerCandidates.set([]);
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

  /** Toggle the "Full width" display preference (⋯ menu item). Does not close the menu. */
  toggleFullWidth(): void {
    this.fullWidth.update((v) => !v);
  }

  /** Read the persisted "Full width" preference; default OFF (the tuned `--editor-max`). */
  private readStoredFullWidth(): boolean {
    try {
      return localStorage.getItem(FULL_WIDTH_KEY) === "1";
    } catch {
      return false;
    }
  }

  /** Toggle the "Ask Brain" chat drawer (header button). */
  toggleNoteChat(): void {
    this.noteChatOpen.update((v) => !v);
  }

  /** Read the persisted drawer open state; default CLOSED (starts collapsed). */
  private readStoredChatOpen(): boolean {
    try {
      return localStorage.getItem(NOTE_CHAT_OPEN_KEY) === "1";
    } catch {
      return false;
    }
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
      if (!this.saveResponseStillApplies(doc.id)) {
        return;
      }
      this.note.update((cur) => (cur ? { ...cur, folderId } : cur));
      const name = this.noteFolders().find((f) => f.id === folderId)?.name ?? "folder";
      this.toast.success(`Moved to ${name}`);
    } catch (e) {
      this.toast.danger(this.errorCopy.because("Couldn’t move", e));
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
    this.debounce.cancel(this.saveDebounceKey(doc.id));
    if (!(await this.queueSave("text")) || this.note()?.locked) {
      return;
    }
    try {
      const path = await this.ipc.exportNoteDoc(doc.id);
      if (!this.saveResponseStillApplies(doc.id)) {
        return;
      }
      this.note.update((cur) => (cur ? { ...cur, exportedPath: path } : cur));
      this.toast.success("Saved to your vault");
    } catch (e) {
      this.toast.danger(this.errorCopy.because("Couldn’t export", e));
    }
  }

  /**
   * SHARE — open the end-to-end-encrypted link-share modal for THIS note
   * (`shareNoteToLinkDoc` + manage/revoke). A locked note is never shared (the
   * backend refuses `Locked`); the button is disabled while locked, and this
   * flushes any pending edit so the shared body is current.
   */
  async share(): Promise<void> {
    this.closeMenus();
    const doc = this.note();
    if (!doc || doc.locked) {
      return;
    }
    // Persist the latest text (cheap, via the single-writer chain) so the
    // shared body is current.
    this.debounce.cancel(this.saveDebounceKey(doc.id));
    if (!(await this.queueSave("text")) || this.note()?.locked) {
      return;
    }
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
    this.debounce.cancel(this.saveDebounceKey(doc.id));
    this.pendingSave = null;
    this.dirtyFull = false;
    try {
      await this.notes.remove(doc.id);
      this.toast.success("Note deleted");
      void this.router.navigate(["/notes"]);
    } catch (e) {
      this.toast.danger(this.errorCopy.because("Couldn’t delete", e));
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
      const seq = ++this.requestSeq;
      const fresh = await this.ipc.getNote(doc.id);
      if (
        seq === this.requestSeq &&
        this.note()?.id === doc.id &&
        !fresh.locked
      ) {
        this.hydrate(fresh);
      }
    } catch (e) {
      // "unlock" context: a Touch ID cancel reads "Touch ID was cancelled — try again." and an
      // unrecognised failure falls back to "Couldn’t unlock. Please try again.".
      this.toast.danger(this.errorCopy.humanize(e, "unlock"));
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

  /**
   * Dismiss editor-owned transient UI when a pointer interaction leaves the
   * body textarea and its teleported overlays. Other note chrome (title,
   * properties, header) is outside the selection's owning surface and must
   * dismiss it too. The overlay boxes live under `<body>`, so they carry an
   * explicit marker rather than relying on component-host containment.
   */
  onDocumentPointerDown(event: PointerEvent): void {
    const target = event.target;
    if (!(target instanceof Element)) {
      return;
    }
    if (
      this.bodyArea()?.nativeElement === target ||
      target.closest("[data-note-editor-overlay]")
    ) {
      return;
    }

    // A pointerdown runs before the target's click. Clear selection-owned UI
    // immediately, but do not tear down the specific inline menu whose click
    // still needs to run (otherwise Share / slash-menu actions disappear before
    // Angular receives their click). Other open menus still close as click-away.
    const insideHeaderMenu = target.closest(".head-crumb, .head-more") !== null;
    const insideSlashMenu = target.closest(".slash-menu") !== null;
    this.clearSelection();
    this.closeLinkPicker();
    if (!insideSlashMenu) {
      this.slashOpen.set(false);
    }
    if (!insideHeaderMenu) {
      this.closeMenus();
    }
  }

  /**
   * A cached note route is detached rather than destroyed on tab/navigation
   * switches. Drop transient editor state explicitly so teleported boxes cannot
   * remain visible over the newly-active route.
   */
  onTabBackgrounded(): void {
    this.dismissTransientUi();
  }

  private dismissTransientUi(): void {
    // A tab route is detached before its signal-driven template can reconcile.
    // Remove any boxes already teleported to <body> synchronously; clearing the
    // owning signals below keeps the cached view correct when it reattaches.
    this.selectionToolbar()?.detachFromDocument();
    this.brainPopover()?.detachFromDocument();
    this.linkPicker()?.detachFromDocument();
    this.clearSelection();
    this.closeLinkPicker();
    this.slashOpen.set(false);
    this.closeMenus();
  }

  /** Drop the selection state (hides both the bubble and the Brain popover). */
  private clearSelection(): void {
    this.sel.set(null);
    this.brainOpen.set(false);
  }

  /**
   * Apply an accepted assistant outcome. Branches on `edit.kind` (never on the
   * action):
   * - `copy`    — write `text` to the clipboard (draft follow-up / an info answer);
   *              no textarea change needed.
   * - `spinoff` — create a NEW note from `title` + `body` and open it.
   * - `replace` — replace the current selection with `suggestion`.
   * - `insert`  — append `suggestion` after the selection (additive — never
   *              destroys the user's text). Then autosave.
   */
  applyEdit(edit: AcceptedEdit): void {
    // Clipboard + note-creation don't touch the selection span — handle first.
    if (edit.kind === "copy") {
      void this.copyToClipboard(edit.text);
      this.clearSelection();
      return;
    }
    if (edit.kind === "spinoff") {
      void this.createSpinoffNote(edit.title, edit.body);
      this.clearSelection();
      return;
    }

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
    if (edit.kind === "insertLink") {
      // Fix 3 — insert a `[[Title]]` wikilink after the selection, on its own
      // paragraph (same additive placement as `insert`), via the ONE shared
      // wikilink-text builder (never re-duplicated).
      const link = `\n\n${this.wikilinkText(edit.title)}\n`;
      this.replaceRange(el, end, end, link, end + link.length, end + link.length);
    } else if (edit.kind === "insert") {
      // Insert the additive passage after the selection on its own paragraph.
      const insert = `\n\n${edit.suggestion.trim()}\n`;
      this.replaceRange(el, end, end, insert, end + insert.length, end + insert.length);
    } else {
      // COLLAPSE the caret to the END of the inserted suggestion (WT-F1 fix). Re-SELECTING the
      // suggestion (start..start+len) makes `setSelectionRange` queue a `select` event that fires
      // AFTER `clearSelection()` below — with a live non-empty selection but a null `sel()`, so the
      // unchanged-selection guard in `onBodySelect` can't fire and the bubble RE-FLOATS after Accept.
      // A collapsed caret leaves no selection → the queued `select` closes cleanly → bubble stays gone.
      this.replaceRange(
        el,
        start,
        end,
        edit.suggestion,
        start + edit.suggestion.length,
        start + edit.suggestion.length,
      );
    }
    this.clearSelection();
  }

  /** Copy assistant output to the clipboard (best-effort; toast on either path). */
  private async copyToClipboard(text: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(text);
      this.toast.success("Copied to clipboard");
    } catch {
      this.toast.danger("Couldn’t copy to clipboard");
    }
  }

  /**
   * Create a NEW note from a spin-off draft (title + body) in the CURRENT folder
   * and open it. The assistant command returned only a draft — note creation is
   * the user's explicit action here (keeps the command pure, per the seam
   * contract). Best-effort; a failure toasts and leaves the editor untouched.
   */
  private async createSpinoffNote(title: string, body: string): Promise<void> {
    try {
      const folderId = this.note()?.folderId ?? null;
      const id = await this.ipc.createNote(folderId, title);
      await this.ipc.updateNoteDoc(id, title, body);
      void this.notes.loadNotes(null);
      void this.router.navigate(["/notes", id]);
    } catch (e) {
      // Surface a sealed-folder refusal (`[folder-locked]`) plainly, like the main "New note"
      // action and the share panel — never a bare "couldn't create".
      this.toast.danger(
        this.errorCopy.is(e, "folder-locked")
          ? "This folder is locked — unlock it first to add a note."
          : "Couldn’t create the note",
      );
    }
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
