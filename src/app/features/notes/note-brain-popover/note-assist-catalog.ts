import type { NoteAssistAction, NoteAssistShape } from "../../../core/models";

/** The quiet section labels the actions group under, in display order. */
export type NoteAssistGroup =
  | "EDIT"
  | "STRUCTURE"
  | "FROM YOUR BRAIN"
  | "EXTRACT"
  | "CREATE";

/** Display order of the groups in the expanded (grouped) menu. */
export const NOTE_ASSIST_GROUPS: readonly NoteAssistGroup[] = [
  "EDIT",
  "STRUCTURE",
  "FROM YOUR BRAIN",
  "EXTRACT",
  "CREATE",
];

/**
 * One catalog entry for a selection-assistant action. The SINGLE source of truth
 * shared by the popover command menu AND the Settings toggle rows, so the id ↔
 * label ↔ group ↔ shape mapping never drifts between them. Mirrors the seam
 * contract's action table exactly (do not diverge the ids — the backend is built
 * to the same list).
 */
export interface NoteAssistCatalogEntry {
  /** The `NoteAssistAction` id sent to the backend. */
  id: Exclude<NoteAssistAction, "custom">;
  /** Human label ("Refine", "Change tone"…). */
  label: string;
  /** A single glyph shown in the action row's icon square. */
  icon: string;
  /** The section this action groups under. */
  group: NoteAssistGroup;
  /** How the backend result renders/applies — the shape legend dot. */
  shape: NoteAssistShape;
  /** True for the compact-default (quick) set. */
  quick?: boolean;
  /** One-line description shown under the label. */
  desc: string;
  /** Variant options (tones / languages) → opens a submenu instead of running. */
  sub?: readonly string[];
  /**
   * The three legacy actions bind to their own AppConfig bools; every OTHER action
   * is enabled unless its id is in `noteAssistActionsOff`. This flag marks the
   * legacy trio so the settings block + editor route them correctly.
   */
  legacy?: boolean;
}

/**
 * The full action catalog (mirrors the seam contract table + the approved
 * prototype). The Settings block renders these as grouped toggle rows; the
 * popover renders them as command-menu rows. `custom` is NOT in the catalog — it
 * is synthesized from the command input and is always available.
 */
export const NOTE_ASSIST_CATALOG: readonly NoteAssistCatalogEntry[] = [
  // EDIT
  {
    id: "refine",
    label: "Refine",
    icon: "✎",
    group: "EDIT",
    shape: "replace",
    quick: true,
    legacy: true,
    desc: "Clarity, grammar & flow — same meaning",
  },
  {
    id: "grammar",
    label: "Fix grammar",
    icon: "Aa",
    group: "EDIT",
    shape: "replace",
    desc: "Surgical spelling & grammar only",
  },
  {
    id: "shorten",
    label: "Shorten",
    icon: "⤶",
    group: "EDIT",
    shape: "replace",
    quick: true,
    legacy: true,
    desc: "About half the length, keep every fact",
  },
  {
    id: "expand",
    label: "Expand",
    icon: "⤢",
    group: "EDIT",
    shape: "replace",
    desc: "Terse notes → full prose",
  },
  {
    id: "simplify",
    label: "Simplify",
    icon: "≈",
    group: "EDIT",
    shape: "replace",
    desc: "Plain, jargon-free language",
  },
  {
    id: "tone",
    label: "Change tone",
    icon: "◑",
    group: "EDIT",
    shape: "replace",
    sub: ["Professional", "Casual", "Confident", "Friendly", "Direct"],
    desc: "Rewrite in a chosen voice",
  },
  {
    id: "translate",
    label: "Translate",
    icon: "⇄",
    group: "EDIT",
    shape: "replace",
    quick: true,
    sub: ["Polski", "English", "Deutsch", "Español", "Français", "日本語"],
    desc: "Into another language",
  },
  // STRUCTURE
  {
    id: "bullets",
    label: "Bullet points",
    icon: "☰",
    group: "STRUCTURE",
    shape: "replace",
    desc: "Prose → a clean list",
  },
  {
    id: "table",
    label: "Table",
    icon: "▦",
    group: "STRUCTURE",
    shape: "replace",
    desc: "Rows → a markdown table",
  },
  {
    id: "keypoints",
    label: "Key points",
    icon: "¶",
    group: "STRUCTURE",
    shape: "insert",
    desc: "A short TL;DR digest",
  },
  // FROM YOUR BRAIN
  {
    id: "enhance",
    label: "Enhance context",
    icon: "✦",
    group: "FROM YOUR BRAIN",
    shape: "insert",
    legacy: true,
    desc: "Add a grounded passage from your brain",
  },
  {
    id: "find_related",
    label: "Find related",
    icon: "◎",
    group: "FROM YOUR BRAIN",
    shape: "info",
    quick: true,
    desc: "Notes, meetings & people that match",
  },
  {
    id: "link_entities",
    label: "Link entities",
    icon: "[[",
    group: "FROM YOUR BRAIN",
    shape: "replace",
    desc: "Turn known names into [[wikilinks]]",
  },
  {
    id: "fact_check",
    label: "Fact-check",
    icon: "✓",
    group: "FROM YOUR BRAIN",
    shape: "info",
    desc: "Flag claims that clash with your notes",
  },
  {
    id: "ask",
    label: "Ask about this",
    icon: "?",
    group: "FROM YOUR BRAIN",
    shape: "info",
    desc: "Ask the brain about the selection",
  },
  // EXTRACT
  {
    id: "action_items",
    label: "Action items",
    icon: "☑",
    group: "EXTRACT",
    shape: "insert",
    quick: true,
    desc: "Pull TODOs into a checklist",
  },
  {
    id: "decisions",
    label: "Decisions",
    icon: "◆",
    group: "EXTRACT",
    shape: "insert",
    desc: "Pull out decisions made",
  },
  // CREATE
  {
    id: "draft_followup",
    label: "Draft follow-up",
    icon: "✉",
    group: "CREATE",
    shape: "artifact",
    desc: "A ready-to-send email or message",
  },
  {
    id: "spinoff_note",
    label: "Spin-off note",
    icon: "↗",
    group: "CREATE",
    shape: "artifact",
    desc: "A new linked note from the selection",
  },
];

/** The catalog ids of the NEW actions (everything except the legacy trio). */
export const NOTE_ASSIST_NEW_ACTION_IDS: readonly string[] =
  NOTE_ASSIST_CATALOG.filter((a) => !a.legacy).map((a) => a.id);

/** Look up a catalog entry by id (undefined for `custom`). */
export function noteAssistEntry(
  id: string,
): NoteAssistCatalogEntry | undefined {
  return NOTE_ASSIST_CATALOG.find((a) => a.id === id);
}
