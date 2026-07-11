import type { Page } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Boot the app under the shared demo Tauri mock, then override the NOTE commands
 * with plausible payloads so the whole Notes feature is drivable with no Rust
 * core. The command NAMES + camelCase DTOs match `ipc.service.ts` exactly:
 *   list_note_folders / list_notes / get_note / create_note / update_note_doc /
 *   move_note_doc / delete_note / export_note_doc / note_assistant_action /
 *   plan_organize_notes / apply_organize_plan / create_note_folder /
 *   rename_note_folder / delete_note_folder / move_note_folder / list_my_shares.
 *
 * Overrides run PAGE-SIDE (serialized to strings by `mockTauri` — they must be
 * self-contained, no closures over test-scope). Unknown commands fall through to
 * the demo mock's benign defaults, so the app always boots. `get_config` here
 * carries the note-assistant toggles ON so the editor's popover picker renders.
 *
 * `extra` layers per-spec command overrides into the SAME `mockTauri` call (the base
 * mock re-installs `window.__TAURI_INTERNALS__` from scratch, so a SECOND `mockTauri`
 * call would wipe these Notes overrides — everything must go through one call). Keys
 * in `extra` win over the Notes defaults.
 */
export async function mockNotes(
  page: Page,
  extra: Record<string, (args: any) => unknown> = {},
): Promise<void> {
  await mockTauri(page, {
    // --- config (note-assistant toggles ON so the popover shows every action) ---
    get_config: () => ({
      onboarded: true,
      sharingChoiceMade: true,
      providerId: "claude_code",
      noteAssistRefine: true,
      noteAssistShorten: true,
      noteAssistEnhance: true,
    }),

    // --- folders (one open, one locked) ---
    list_note_folders: () => [
      { id: "nf1", name: "Notes", path: "Notes", parentId: null, locked: false, kind: "note" },
      { id: "nf2", name: "Work", path: "Notes/Work", parentId: null, locked: true, kind: "note" },
    ],

    // --- the note list: two visible rows + one masked (locked) row ---
    list_notes: () => [
      {
        id: "n1",
        title: "My First Note",
        folderId: "nf1",
        snippet: "Hello world from a note",
        tags: ["idea"],
        updatedAt: 1_720_000_000_000,
        createdAt: 1_719_000_000_000,
        locked: false,
        shared: false,
      },
      {
        id: "n2",
        title: "Weekly plan",
        folderId: "nf1",
        snippet: "Ship the notes feature",
        tags: [],
        updatedAt: 1_720_100_000_000,
        createdAt: 1_719_100_000_000,
        locked: false,
        shared: true,
      },
      {
        id: "nlk",
        title: "🔒 Locked",
        folderId: "nf2",
        snippet: "",
        tags: [],
        updatedAt: 1_720_050_000_000,
        createdAt: 1_719_050_000_000,
        locked: true,
        shared: false,
      },
    ],

    // --- the full note for the editor (masked shape for the locked id) ---
    get_note: (args: { id: string }) => {
      if (args.id === "nlk") {
        return {
          id: "nlk",
          title: "🔒 Locked",
          folderId: "nf2",
          markdown: "",
          tags: [],
          properties: {},
          updatedAt: 1_720_050_000_000,
          createdAt: 1_719_050_000_000,
          exportedPath: null,
          locked: true,
          shared: false,
        };
      }
      return {
        id: args.id,
        title: "My First Note",
        folderId: "nf1",
        markdown: "# Heading\n\nSome body text to select.",
        tags: ["idea"],
        properties: {},
        updatedAt: 1_720_000_000_000,
        createdAt: 1_719_000_000_000,
        exportedPath: null,
        locked: false,
        shared: false,
      };
    },

    // --- CRUD (echo / benign) ---
    create_note: () => "n-new",
    update_note_doc: (args: { id: string; title: string; markdown: string }) => ({
      id: args.id,
      title: args.title,
      folderId: "nf1",
      markdown: args.markdown,
      tags: ["idea"],
      properties: {},
      updatedAt: 1_720_000_100_000,
      createdAt: 1_719_000_000_000,
      exportedPath: null,
      locked: false,
      shared: false,
    }),
    move_note_doc: () => null,
    delete_note: () => null,
    export_note_doc: () => "/Users/demo/Obsidian/Vault/Notes/My-First-Note.md",

    // --- the selection Brain assistant ---
    note_assistant_action: (args: { req: { action: string } }) => ({
      action: args.req.action,
      suggestion: "A refined version of your text.",
      citations: [],
      modelLabel: "Claude",
      mode: "cloud",
      redacted: false,
    }),

    // --- auto-organize (one proposed move to a NEW folder) ---
    plan_organize_notes: () => ({
      moves: [
        {
          noteId: "n1",
          title: "My First Note",
          fromFolderId: "nf1",
          fromFolder: "Notes",
          toFolder: "Ideas",
          toFolderId: null,
          reason: "Groups your idea notes",
        },
      ],
    }),
    apply_organize_plan: () => null,

    // --- folder management (benign) ---
    create_note_folder: (args: { name: string; parentId: string | null }) => ({
      id: "nf-new",
      name: args.name,
      path: "Notes/" + args.name,
      parentId: args.parentId ?? null,
      locked: false,
      kind: "note",
    }),
    rename_note_folder: () => null,
    delete_note_folder: () => null,
    move_note_folder: () => null,

    // --- sharing (empty list; the editor warms this) ---
    list_my_shares: () => [],

    // --- per-spec overrides win over the Notes defaults above ---
    ...extra,
  });
}
