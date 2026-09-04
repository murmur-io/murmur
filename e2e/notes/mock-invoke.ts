import type { Page } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Boot the app under the shared demo Tauri mock, then override the NOTE commands
 * with plausible payloads so the whole Notes feature is drivable with no Rust
 * core. The command NAMES + camelCase DTOs match `ipc.service.ts` exactly:
 *   list_note_folders / list_notes / get_note / create_note / update_note_doc /
 *   add_note_attachment / list_note_attachments / delete_note_attachment /
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
  delayedEventListeners: string[] = [],
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
    // The ONE sidebar tree, carrying the same two folders `list_note_folders` describes.
    // The hierarchy replaced the two per-type trees, so a Notes spec that drives the sidebar
    // now drives this — and a fixture that fed only the old reader would render an empty
    // sidebar beside a populated page, which is not a state the app can be in.
    list_workspace_tree: () => [
      {
        id: "p-root",
        name: "Workspace",
        level: "project",
        emoji: null,
        tint: null,
        locked: false,
        unlocked: false,
        isRoot: false,
        folders: [
          {
            id: "nf1",
            name: "Notes",
            level: "folder",
            emoji: null,
            tint: null,
            locked: false,
            unlocked: false,
            // An ORDINARY folder, not the reserved root: the reserved root can never be
            // sealed, and marking it so would quietly remove the lock affordance the notes
            // specs exercise.
            isRoot: false,
            folders: [],
            groups: [],
          },
          {
            id: "nf2",
            name: "Work",
            level: "folder",
            emoji: null,
            tint: null,
            locked: true,
            unlocked: false,
            isRoot: false,
            folders: [],
            groups: [],
          },
        ],
        groups: [],
      },
    ],
    folder_active_shares: () => ({ links: 0, users: 0, org: [] }),

    // The container view a tree row opens. A sealed container reports itself locked and is
    // never asked for its items — the backend refuses that read, and asking would surface an
    // error where a deliberate refusal belongs.
    get_container: (args: { id: string }) =>
      args.id === "nf2"
        ? { id: "nf2", name: "Work", level: "folder", emoji: null, tint: null, locked: true, unlocked: false, isRoot: false, folders: [], groups: [] }
        : { id: args.id, name: "Notes", level: "folder", emoji: null, tint: null, locked: false, unlocked: false, isRoot: false, folders: [], groups: [] },
    list_container_items: (args: { kind: string }) => ({ kind: args.kind, total: 0, items: [] }),

    // --- the "Add related" hierarchy picker's gated reader ---
    // The Notes specs anchor on `/notes/n1`, which lives in `nf1`. The default
    // fixture therefore opens with `Workspace / Notes` expanded and `n1` marked
    // Current, plus a sealed Space (`nf2`) whose NAME is all it discloses.
    // Explicit handlers, not the base mock's `default:` fallback — an unhandled
    // `list_*` resolves to `[]` there, which would render exactly like a vault
    // with nothing in it and greenwash a command that never ran.
    get_related_picker_bootstrap: (args: {
      anchorKind: string;
      anchorId: string;
    }) => ({
      spaces: [
        {
          id: "p-root",
          name: "Workspace",
          level: "project",
          emoji: null,
          locked: false,
          unlocked: false,
          linkable: true,
          groups: [],
          folders: [
            {
              id: "nf1",
              name: "Notes",
              level: "folder",
              emoji: null,
              locked: false,
              unlocked: false,
              linkable: true,
              groups: [
                { kind: "note", total: 2 },
                { kind: "meeting", total: 1 },
              ],
              folders: [],
            },
            {
              id: "nf2",
              name: "Work",
              level: "folder",
              emoji: null,
              locked: true,
              unlocked: false,
              linkable: false,
              groups: [],
              folders: [],
            },
          ],
        },
      ],
      unclassified: [{ kind: "meeting", total: 1 }],
      anchor:
        args.anchorKind === "note" && args.anchorId === "n1"
          ? {
              kind: "note",
              containerId: "nf1",
              path: ["p-root", "nf1"],
              index: 0,
              offset: 0,
              items: [
                { kind: "note", id: "n1", title: "My First Note" },
                { kind: "note", id: "n2", title: "Weekly plan" },
              ],
              total: 2,
            }
          : null,
    }),
    list_related_picker_items: (args: {
      containerId: string | null;
      kind: string;
      offset: number;
      limit: number;
    }) => {
      const rows: Record<string, { kind: string; id: string; title: string }[]> =
        {
          "nf1|note": [
            { kind: "note", id: "n1", title: "My First Note" },
            { kind: "note", id: "n2", title: "Weekly plan" },
          ],
          "nf1|meeting": [
            { kind: "meeting", id: "m-cand", title: "Some meeting" },
          ],
          "u|meeting": [
            { kind: "meeting", id: "m-loose", title: "Unfiled recording" },
          ],
        };
      const key = `${args.containerId ?? "u"}|${args.kind}`;
      const items = rows[key] ?? [];
      return {
        kind: args.kind,
        offset: args.offset,
        items: items.slice(args.offset, args.offset + args.limit),
        total: items.length,
      };
    },
    search_related_picker: (args: { query: string }) => {
      const all = [
        {
          kind: "note",
          id: "n2",
          title: "Weekly plan",
          breadcrumb: ["Workspace", "Notes"],
        },
        {
          kind: "meeting",
          id: "m-loose",
          title: "Unfiled recording",
          breadcrumb: ["Not classified"],
        },
      ];
      const q = args.query.trim().toLowerCase();
      const hits = all.filter((row) => row.title.toLowerCase().includes(q));
      return { offset: 0, hits, total: hits.length };
    },

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
    list_note_attachments: (args: {
      ownerKind: "note" | "meeting" | "org";
      ownerId: string;
    }) => {
      const rows = ((window as any).__noteAttachments ?? []) as any[];
      return rows.filter(
        (row) => row.ownerKind === args.ownerKind && row.ownerId === args.ownerId,
      );
    },
    add_note_attachment: (args: {
      ownerKind: "note" | "meeting" | "org";
      ownerId: string;
      mimeType: string;
      dataBase64: string;
    }) => ({
      ...(() => {
        const standard = args.dataBase64.replace(/-/g, "+").replace(/_/g, "/");
        const padded = standard.padEnd(Math.ceil(standard.length / 4) * 4, "=");
        const row = {
          id: crypto.randomUUID(),
          ownerKind: args.ownerKind,
          ownerId: args.ownerId,
          mimeType: args.mimeType,
          extension: "webp",
          byteLen: Math.floor((padded.length * 3) / 4),
          width: 1,
          height: 1,
          sha256: "demo",
          dataUrl: `data:image/webp;base64,${padded}`,
        };
        (window as any).__noteAttachments = [
          ...((window as any).__noteAttachments ?? []),
          row,
        ];
        return row;
      })(),
    }),
    delete_note_attachment: (args: { attachmentId: string }) => {
      (window as any).__noteAttachments = (
        ((window as any).__noteAttachments ?? []) as any[]
      ).filter((row) => row.id !== args.attachmentId);
      return null;
    },
    move_note_doc: () => null,
    delete_note: () => null,
    export_note_doc: () => "/Users/demo/Obsidian/Vault/Notes/My-First-Note.md",

    // --- the selection Brain assistant ---
    note_assistant_action: (args: { req: { action: string } }) => {
      // Mirror the backend `note_assist_shape` so the popover's `@switch (res.shape)`
      // renders the right result phase (replace/insert/info/artifact).
      const a = args.req.action;
      const shape = ["find_related", "fact_check", "ask"].includes(a)
        ? "info"
        : ["enhance", "keypoints", "action_items", "decisions"].includes(a)
          ? "insert"
          : ["draft_followup", "spinoff_note"].includes(a)
            ? "artifact"
            : "replace";
      return {
        action: a,
        shape,
        title: shape === "artifact" ? "Re: Note" : null,
        suggestion: "A refined version of your text.",
        citations: [],
        modelLabel: "Claude",
        mode: "cloud",
        redacted: false,
      };
    },

    // --- auto-organize (current scoped plan DTO + honest apply receipt) ---
    plan_organize_notes: () => ({
      scopeFolderId: null,
      moves: [
        {
          noteId: "n1",
          title: "My First Note",
          fromFolderId: "nf1",
          fromFolder: "Notes",
          toFolder: "Ideas",
          toFolderId: null,
          reason: "Groups your idea notes",
          confidence: "high",
        },
      ],
      totalScanned: 3,
      alreadyOrganized: 1,
      deferred: 1,
      targets: [],
    }),
    apply_organize_plan: () => ({ appliedIds: ["n1"], failures: [] }),

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
  }, {}, [], delayedEventListeners);
}

/**
 * Put the ROUTED editor into Edit mode, and wait until the body textarea exists.
 *
 * A note that HAS a body now opens in Preview — the reading default, see
 * `note-editor.component.ts` `opensInPreview`. `.body-area` only exists in the Edit
 * branch of the template (`@if (previewActive()) { … } @else { … .body-area }`), so a
 * spec that drives the textarea has to ask for Edit first, exactly as a user would.
 * That is the honest encoding of the new default: the old suite silently assumed
 * "opening a note lands in a textarea", which is no longer what the product does.
 *
 * Idempotent, so it is safe to call unconditionally on any routed note with a body:
 * clicking Edit while Edit is already active hits `setPreview(false)`'s early return.
 *
 * Do NOT use it for a LOCKED note (the lock gate replaces the body entirely, so there
 * is no `.body-area` to wait for) or for the EMBEDDED host (the recording panel's Note
 * tab force-disables preview via `previewActive`, so it is already in Edit).
 */
export async function enterEditMode(page: Page): Promise<void> {
  await page.getByRole("button", { name: "Edit", exact: true }).click();
  await page.locator(".body-area").waitFor({ state: "visible" });
}
