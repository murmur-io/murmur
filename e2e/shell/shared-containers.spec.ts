import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Shared containers in the Workspaces sidebar: a received Workspace as its own
 * top-level row, loose received content inside the virtual "Shared Brains"
 * Workspace, the shared marker on both sides, and the read-only structure a
 * received container keeps at every access level.
 *
 * Every fixture key below was copied from the RUST DTOs
 * (`SharedWorkspace` / `SharedContainerNode` / `SharedItemRow` /
 * `ContainerShareStatus` in `commands/org_containers.rs`), not from the
 * frontend's own interface. A hand-written mock DEFINES a shape; it does not
 * verify one — the serialized-key oracle in
 * `commands/tests/container_share_tests.rs` is what proves the backend agrees.
 */
const LOCAL_FOREST = [
  {
    id: "p-acme",
    name: "Acme",
    kind: "note",
    level: "project",
    emoji: null,
    tint: null,
    locked: false,
    unlocked: false,
    isRoot: false,
    folders: [],
    groups: [],
  },
  {
    id: "p-clients",
    name: "Clients",
    kind: "note",
    level: "project",
    emoji: null,
    tint: null,
    locked: false,
    unlocked: false,
    isRoot: false,
    folders: [],
    groups: [],
  },
  {
    id: "p-sealed",
    name: "Personal",
    kind: "note",
    level: "project",
    emoji: null,
    tint: null,
    locked: true,
    unlocked: false,
    isRoot: false,
    folders: [],
    groups: [],
  },
];

const sharedItem = (itemId: string, title: string, kind: string) => ({
  itemId,
  docId: `doc-${itemId}`,
  title,
  kind,
  authorHint: "kgm004a",
  createdAt: "2026-08-20T09:00:00Z",
  orgId: "org-siema",
  orgName: "Siema",
  access: "view",
  position: 0,
});

const sharedNode = (
  containerId: string | null,
  name: string,
  level: string,
  extra: Record<string, unknown> = {},
) => ({
  containerId,
  orgId: "org-siema",
  orgName: "Siema",
  name,
  level,
  emoji: null,
  tint: null,
  access: "view",
  authorHint: "kgm004a",
  folders: [],
  items: [],
  localParentId: null,
  position: 0,
  ...extra,
});

const SHARED_WORKSPACE = {
  spaces: [
    sharedNode("c-partners", "Partners", "space", {
      folders: [
        sharedNode("c-contracts", "Contracts", "folder", {
          items: [sharedItem("si-contract", "Reseller agreement", "document")],
        }),
      ],
      items: [sharedItem("si-kickoff", "Partner kickoff", "meeting")],
    }),
  ],
  sharedBrains: {
    ...sharedNode(null, "Shared Brains", "virtual", {
      folders: [sharedNode("c-loose", "Research", "folder")],
      items: [sharedItem("si-loose", "Pricing thoughts", "document")],
    }),
    orgId: "",
    orgName: "",
    authorHint: "",
  },
};

const CONTAINER_SHARES = [
  {
    orgId: "org-siema",
    orgName: "Siema",
    folderId: "p-clients",
    containerId: "c-clients",
    access: "view",
    isRoot: true,
    state: "published",
  },
];

async function openSidebar(
  page: Page,
  constants: Record<string, unknown> = {},
): Promise<void> {
  await mockTauri(
    page,
    {},
    {
      list_workspace_tree: LOCAL_FOREST,
      list_shared_workspace: SHARED_WORKSPACE,
      list_container_share_status: CONTAINER_SHARES,
      ...constants,
    },
  );
  await page.goto("/");
  await expect(
    page.getByRole("navigation", { name: "Primary navigation" }),
  ).toBeVisible();
}

test("a received Workspace is its own top-level sidebar row", async ({ page }) => {
  await openSidebar(page);
  const partners = page.getByRole("treeitem", { name: /Partners/ });
  await expect(partners).toBeVisible();
  // Top level, beside the user's own Workspaces — not buried inside Shared Brains.
  await expect(partners).toHaveAttribute("aria-level", "1");
});

test("loose received content lives inside the virtual Shared Brains Workspace", async ({
  page,
}) => {
  await openSidebar(page);
  const brains = page.getByRole("treeitem", { name: /Shared Brains/ });
  await expect(brains).toBeVisible();
  await expect(brains).toHaveAttribute("aria-level", "1");

  // Its contents appear only once expanded — a received folder and a loose note.
  await expect(page.getByRole("treeitem", { name: /Research/ })).toHaveCount(0);
  await brains.getByRole("button", { name: /Expand/ }).click();
  await expect(page.getByRole("treeitem", { name: /Research/ })).toBeVisible();
  await expect(
    page.getByRole("treeitem", { name: /Pricing thoughts/ }),
  ).toBeVisible();
});

test("the shared marker names the organization on both sides", async ({
  page,
}) => {
  await openSidebar(page);

  // RECEIVED: who shared it, and at what access.
  const received = page.getByRole("img", {
    name: /From Siema · kgm004a · View only/,
  });
  await expect(received).toHaveCount(1);

  // SENT: where it goes, and at what access. One glyph, the sentence on hover.
  const sent = page.getByRole("img", { name: /Shared to Siema · View only/ });
  await expect(sent).toHaveCount(1);
});

test("a received container offers arrangement but never structure", async ({
  page,
}) => {
  await openSidebar(page);
  const partners = page.getByRole("treeitem", { name: /Partners/ });
  await partners.getByRole("button", { name: /Actions for shared/ }).click();

  // The user may file it in their OWN tree — that is device-local.
  await expect(page.getByRole("menuitem", { name: /Keep in my Workspace/ })).toBeVisible();

  // But its structure belongs to whoever shared it, at ANY access level.
  await expect(page.getByRole("menuitem", { name: /^Rename/ })).toHaveCount(0);
  await expect(page.getByRole("menuitem", { name: /^Delete/ })).toHaveCount(0);
  await expect(
    page.getByRole("menuitem", { name: /Create (note|folder|dashboard) here/ }),
  ).toHaveCount(0);
});

test("a local Workspace offers Share to Org, and a sealed one does not", async ({
  page,
}) => {
  await openSidebar(page);

  const acme = page.getByRole("treeitem", { name: /Acme/ });
  await acme.getByRole("button", { name: /Actions for Acme/ }).click();
  await expect(page.getByTestId("share-container")).toHaveText(/Share to Org/);
  await page.keyboard.press("Escape");

  // An already-shared container says so instead of offering to share again.
  const clients = page.getByRole("treeitem", { name: /Clients/ });
  await clients.getByRole("button", { name: /Actions for Clients/ }).click();
  await expect(page.getByTestId("share-container")).toHaveText(/Sharing/);
  await page.keyboard.press("Escape");

  // A SEALED container cannot be shared: its content is not readable, so
  // offering the action would promise something the backend refuses.
  const sealed = page.getByRole("treeitem", { name: /Personal/ });
  await sealed.getByRole("button", { name: /Actions for Personal/ }).click();
  await expect(page.getByTestId("share-container")).toHaveCount(0);
});

test("the share sheet names what is left behind before anything leaves", async ({
  page,
}) => {
  await openSidebar(page, {
    org_list_statuses: [
      {
        orgId: "org-siema",
        name: "Siema",
        role: "owner",
        memberCount: 3,
        consented: true,
        lastSeq: 4,
        itemCount: 2,
        receivedCount: 5,
        pendingShares: 0,
        contextEnabled: true,
      },
    ],
  });

  const acme = page.getByRole("treeitem", { name: /Acme/ });
  await acme.getByRole("button", { name: /Actions for Acme/ }).click();
  await page.getByTestId("share-container").click();

  const sheet = page.getByRole("dialog", { name: /Share this Workspace/ });
  await expect(sheet).toBeVisible();
  // The counts, and — the honesty invariant — what is deliberately NOT going.
  await expect(sheet.getByText(/3 notes/)).toBeVisible();
  await expect(sheet.getByText(/locked folder stays behind/)).toBeVisible();
  await expect(sheet.getByText(/dashboard is not shared yet/)).toBeVisible();
  await expect(
    sheet.getByText(/Transcripts and audio are never shared/),
  ).toBeVisible();
  // View only is the fail-closed default.
  await expect(
    sheet.getByRole("button", { name: /View only/ }),
  ).toHaveAttribute("aria-pressed", "true");
});

test("a received Workspace can be filed under a local Workspace, privately", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      set_shared_placement: (args: unknown) => {
        (
          window as unknown as { __placements?: unknown[] }
        ).__placements ??= [];
        (window as unknown as { __placements: unknown[] }).__placements.push(
          args,
        );
        return null;
      },
    },
    {
      list_workspace_tree: LOCAL_FOREST,
      list_shared_workspace: SHARED_WORKSPACE,
      list_container_share_status: CONTAINER_SHARES,
    },
  );
  await page.goto("/");

  const partners = page.getByRole("treeitem", { name: /Partners/ });
  await partners.getByRole("button", { name: /Actions for shared/ }).click();
  await page.getByTestId("place-shared").click();

  const sheet = page.getByRole("dialog");
  await expect(sheet).toBeVisible();
  await sheet.getByRole("button", { name: /Move to Acme/ }).click();

  // Device-local: the placement is recorded, and NOTHING is published.
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __placements?: unknown[] }).__placements ??
          [],
      ),
    )
    .toEqual([
      {
        orgId: "org-siema",
        targetKind: "container",
        targetId: "c-partners",
        localParentId: "p-acme",
        position: 0,
      },
    ]);
});

test("a nested received folder can be filed too, and renders in exactly one place", async ({
  page,
}) => {
  // The "Keep in my Workspace…" action is offered on EVERY received container, so
  // the merge must find a nested one's placement as well — and must not then
  // render it under BOTH its shared parent and its new local host.
  const placed = {
    ...SHARED_WORKSPACE,
    spaces: [
      {
        ...SHARED_WORKSPACE.spaces[0],
        folders: [
          {
            ...SHARED_WORKSPACE.spaces[0].folders[0],
            localParentId: "p-acme",
          },
        ],
      },
    ],
  };
  await mockTauri(
    page,
    {},
    {
      list_workspace_tree: LOCAL_FOREST,
      list_shared_workspace: placed,
      list_container_share_status: CONTAINER_SHARES,
    },
  );
  await page.goto("/");

  // Expand both possible hosts, then assert the row exists exactly once.
  await page
    .getByRole("treeitem", { name: /Acme/ })
    .getByRole("button", { name: /Expand/ })
    .click();
  await page
    .getByRole("treeitem", { name: /Partners/ })
    .getByRole("button", { name: /Expand/ })
    .click();
  await expect(page.getByRole("treeitem", { name: /Contracts/ })).toHaveCount(1);
});

test("a received loose item and an own standalone share each carry the marker", async ({
  page,
}) => {
  await mockTauri(
    page,
    {},
    {
      list_workspace_tree: [
        {
          ...LOCAL_FOREST[0],
          groups: [
            {
              kind: "note",
              total: 2,
              items: [
                { kind: "note", id: "n-shared", title: "Roadmap", durationS: null, sortAt: 20 },
                { kind: "note", id: "n-private", title: "Scratch", durationS: null, sortAt: 10 },
              ],
            },
          ],
        },
        ...LOCAL_FOREST.slice(1),
      ],
      list_shared_workspace: SHARED_WORKSPACE,
      list_container_share_status: CONTAINER_SHARES,
      list_org_share_targets: [
        {
          kind: "note",
          id: "n-shared",
          orgId: "org-siema",
          orgName: "Siema",
          access: "edit",
        },
      ],
    },
  );
  await page.goto("/");
  await page
    .getByRole("treeitem", { name: /Acme/ })
    .getByRole("button", { name: /Expand/ })
    .click();

  // The user's OWN note, published on its own, says where it went.
  await expect(
    page.getByRole("img", { name: /Shared to Siema · Can edit/ }),
  ).toHaveCount(1);
  // A note they did not share carries nothing — the marker must mean something.
  const scratch = page.getByRole("treeitem", { name: /Scratch/ });
  await expect(scratch.getByRole("img", { name: /Shared to/ })).toHaveCount(0);

  // A RECEIVED loose item names who shared it.
  await page
    .getByRole("treeitem", { name: /Shared Brains/ })
    .getByRole("button", { name: /Expand/ })
    .click();
  await expect(
    page.getByRole("img", { name: /From Siema · kgm004a · View only/ }),
  ).not.toHaveCount(0);
});

test("an item inside a shared container is not marked twice", async ({ page }) => {
  // The container's own row already says it. Repeating the glyph on every child
  // turns a quiet signal into noise, which is why the backend read excludes
  // container-owned rows outright.
  await openSidebar(page);
  await page
    .getByRole("treeitem", { name: /Partners/ })
    .getByRole("button", { name: /Expand/ })
    .click();
  const kickoff = page.getByRole("treeitem", { name: /Partner kickoff/ });
  await expect(kickoff).toBeVisible();
  await expect(kickoff.getByRole("img", { name: /Shared to/ })).toHaveCount(0);
});

/// A failed shared-workspace read must SAY so, not render as "nothing shared with you".
///
/// Every read in `SharedWorkspaceService.load` swallowed its rejection into `null`/`[]`, so an
/// unreachable relay produced an empty workspace — indistinguishable from a team that has shared
/// nothing. The user was, in effect, told something false, with no retry offered because nothing
/// indicated there was anything to retry.
///
/// The control is the second half: with the same fixtures resolving normally, the banner must be
/// ABSENT. Without that, a component that always showed the message would pass the first assertion
/// and be worse than what it replaced.
test("an unreachable shared workspace shows an error, not an empty one", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      list_shared_workspace: () => {
        throw new Error("relay unreachable");
      },
    },
    {
      list_workspace_tree: LOCAL_FOREST,
      list_container_share_status: CONTAINER_SHARES,
    },
  );
  await page.goto("/");
  await expect(
    page.getByRole("navigation", { name: "Primary navigation" }),
  ).toBeVisible();

  await expect(page.getByText(/Couldn't read what's shared with you/)).toBeVisible();
  await expect(page.getByText("Nothing shared with you yet")).toHaveCount(0);
});

/// A shared read still IN FLIGHT must say so, not assert that nothing is shared.
///
/// `WorkspaceTreeComponent` renders both halves of the tree, and `loading` read
/// `WorkspaceService.loading` in BOTH — `SharedWorkspaceService.loading` was never referenced
/// anywhere in the component. So while the shared fetch was outstanding the shared section fell
/// through to its empty state and told the user "Nothing shared with you yet" about content that
/// was still arriving. That is wrong content, not merely a missing spinner, and it is the spinner
/// half of the same reload-flash contract the template already spells out for cached rows.
///
/// RED CONTROL: revert `loading` to `this.workspace.loading` and this fails on the first
/// assertion — the own-workspace read resolves immediately, so its flag is already false while the
/// shared one is still true.
test("a shared workspace still loading says so instead of claiming nothing is shared", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      // Slow enough to sample mid-flight, short enough not to pad the suite.
      list_shared_workspace: async () => {
        await new Promise((resolve) => setTimeout(resolve, 1500));
        return SHARED_WORKSPACE;
      },
    },
    {
      list_workspace_tree: LOCAL_FOREST,
      list_container_share_status: CONTAINER_SHARES,
    },
  );
  await page.goto("/");
  await expect(
    page.getByRole("navigation", { name: "Primary navigation" }),
  ).toBeVisible();

  await expect(page.getByText("Loading shared content…")).toBeVisible();
  await expect(page.getByText("Nothing shared with you yet")).toHaveCount(0);

  // And the control: once it resolves, the loading line goes away and the rows arrive — so the
  // assertion above is about the in-flight window, not about a spinner that never clears.
  await expect(page.getByText("Loading shared content…")).toHaveCount(0, {
    timeout: 5000,
  });
});

test("a healthy shared workspace shows no error banner", async ({ page }) => {
  await openSidebar(page);
  await expect(page.getByText(/Couldn't read what's shared with you/)).toHaveCount(0);
});
