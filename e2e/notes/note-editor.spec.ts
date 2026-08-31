import { test, expect, type Page } from "@playwright/test";
import { enterEditMode, mockNotes } from "./mock-invoke";

/** Linux WebKit lacks the codec; this test seam exercises the post-encode UI/IPC flow. */
async function mockCanvasWebpEncoder(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const original = HTMLCanvasElement.prototype.toBlob;
    HTMLCanvasElement.prototype.toBlob = function (
      callback: BlobCallback,
      type?: string,
      quality?: number,
    ): void {
      if (type === "image/webp") {
        callback(new Blob([new Uint8Array([0x52, 0x49, 0x46, 0x46])], { type }));
        return;
      }
      original.call(this, callback, type, quality);
    };
  });
}

/**
 * The note editor — loads a note via `get_note` and renders the title, body, and
 * Edit/Preview toggle. Formatting now lives in a floating bubble that appears on a
 * body selection (no persistent toolbar). Toggling to Preview renders the markdown
 * (`# Heading` → an `<h1>`). Runtime check: ZERO console/page errors.
 */
test("note editor loads a note, floats the formatting bubble on selection, and Preview renders the heading — no console errors", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      consoleErrors.push(msg.text());
    }
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page);
  await page.goto("/notes/n1");
  await enterEditMode(page);

  // Title hydrated from the mocked get_note.
  await expect(page.locator(".note-title-input")).toHaveValue("My First Note");

  // Body textarea carries the front-matter-stripped body.
  const body = page.locator(".body-area");
  await expect(body).toHaveValue(/Some body text to select\./);

  // No persistent toolbar — formatting floats on a selection. Simulate one and
  // assert the bubble (H1/Bold/… + Ask Brain) appears above the selected text.
  await body.evaluate((el: HTMLTextAreaElement) => {
    const start = el.value.indexOf("body text");
    el.focus();
    el.setSelectionRange(start, start + "body text".length);
    el.dispatchEvent(new MouseEvent("mouseup", { bubbles: true }));
  });
  const bubble = page.locator(".sel-bar");
  await expect(bubble).toBeVisible();
  // The button label is its text ("H1"); the descriptive "Heading 1" is its title.
  await expect(bubble.getByRole("button", { name: "H1", exact: true })).toBeVisible();
  await expect(bubble.getByRole("button", { name: "Ask Brain" })).toBeVisible();

  // The Edit/Preview segmented toggle.
  const previewBtn = page.getByRole("button", { name: "Preview", exact: true });
  await expect(previewBtn).toBeVisible();

  // Toggle to Preview → the markdown "# Heading" renders as an <h1>.
  await previewBtn.click();
  await expect(page.locator(".note-preview")).toBeVisible();
  await expect(page.locator(".note-preview h1")).toHaveText("Heading");

  expect(consoleErrors).toEqual([]);
});

test("Cmd-V keeps mixed text and a normalized image at the exact caret while typing continues", async ({
  page,
}) => {
  await mockCanvasWebpEncoder(page);
  await mockNotes(page, {
    save_note_text: (args: any) => {
      const w = window as any;
      w.__savedMarkdowns = [...(w.__savedMarkdowns ?? []), args.markdown];
      return 1_720_000_200_000;
    },
    update_note_doc: (args: any) => {
      const w = window as any;
      w.__savedMarkdowns = [...(w.__savedMarkdowns ?? []), args.markdown];
      return {
        id: args.id,
        title: args.title,
        folderId: "nf1",
        markdown: args.markdown,
        tags: [],
        properties: {},
        updatedAt: 1_720_000_200_000,
        createdAt: 1_719_000_000_000,
        exportedPath: "/Vault/Notes/My-First-Note.md",
        locked: false,
        shared: false,
      };
    },
    export_note_doc: () => {
      (window as any).__attachmentExported = true;
      return "/Vault/Notes/My-First-Note.md";
    },
    add_note_attachment: (args: any) => {
      const w = window as any;
      w.__attachmentArgs = args;
      return new Promise((resolve) => {
        w.__resolveAttachment = () => {
          const standard = String(args.dataBase64)
            .replace(/-/g, "+")
            .replace(/_/g, "/");
          const padded = standard.padEnd(Math.ceil(standard.length / 4) * 4, "=");
          resolve({
            id: "11111111-1111-4111-8111-111111111111",
            ownerKind: args.ownerKind,
            ownerId: args.ownerId,
            mimeType: args.mimeType,
            extension: "webp",
            byteLen: Math.floor((padded.length * 3) / 4),
            width: 1,
            height: 1,
            sha256: "demo",
            dataUrl: `data:image/webp;base64,${padded}`,
          });
        };
      });
    },
  });
  await page.goto("/notes/n1");
  await enterEditMode(page);

  const body = page.locator(".body-area");
  await body.evaluate((el: HTMLTextAreaElement) => {
    const at = el.value.indexOf("Some body");
    el.focus();
    el.setSelectionRange(at, at);
    const png = Uint8Array.from(
      atob(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
      ),
      (char) => char.charCodeAt(0),
    );
    const file = new File([png], "client-secret.png", { type: "image/png" });
    const transfer = new DataTransfer();
    transfer.setData(
      "text/html",
      '<p>Useful pasted context</p><img src="webkit-fake-url://screenshot" alt="Screenshot">',
    );
    transfer.setData("text/plain", "Useful pasted context");
    transfer.items.add(file);
    el.dispatchEvent(
      new ClipboardEvent("paste", {
        bubbles: true,
        cancelable: true,
        clipboardData: transfer,
      }),
    );
  });

  await expect(body).toHaveValue(/Useful pasted context[\s\S]*murmur-pending:\/\//);

  // Simulate typing after the paste while canvas/IPC work is still pending.
  await body.evaluate((el: HTMLTextAreaElement) => {
    el.value += "\n\nTyped while image was uploading";
    el.setSelectionRange(el.value.length, el.value.length);
    el.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText" }));
  });

  // Every boundary action is fail-closed while a pending marker exists.
  await page.getByRole("button", { name: "More actions" }).click();
  await expect(page.getByRole("menuitem", { name: /Share/ })).toBeDisabled();
  await expect(page.getByRole("menuitem", { name: /Save to vault/ })).toBeDisabled();
  await expect(page.getByRole("button", { name: "Preview", exact: true })).toBeDisabled();
  expect(await page.evaluate(() => (window as any).__savedMarkdowns ?? [])).toEqual([]);

  await page.evaluate(() => (window as any).__resolveAttachment());

  await expect(body).toHaveValue(/murmur-attachment:\/\/11111111-1111-4111-8111-111111111111/);
  const value = await body.inputValue();
  expect(value).not.toContain("murmur-pending://");
  expect(value).not.toContain("client-secret.png");
  expect(value.indexOf("Useful pasted context")).toBeLessThan(
    value.indexOf("murmur-attachment://"),
  );
  expect(value.indexOf("murmur-attachment://")).toBeLessThan(
    value.indexOf("Some body text to select."),
  );
  expect(value.indexOf("Some body text to select.")).toBeLessThan(
    value.indexOf("Typed while image was uploading"),
  );

  const args = await page.evaluate(() => (window as any).__attachmentArgs);
  expect(args.ownerKind).toBe("note");
  expect(args.ownerId).toBe("n1");
  expect(args.fileName).toBe("note-image.webp");
  expect(args.mimeType).toBe("image/webp");
  expect(args.dataBase64).toMatch(/^[A-Za-z0-9_-]+$/);

  await expect(page.getByRole("menuitem", { name: /Share/ })).toBeEnabled();
  await page.getByRole("menuitem", { name: /Share/ }).click();
  await expect(page.locator(".share-modal")).toBeVisible();
  let savedMarkdowns = await page.evaluate(
    () => ((window as any).__savedMarkdowns ?? []) as string[],
  );
  expect(savedMarkdowns.length).toBeGreaterThan(0);
  expect(savedMarkdowns.every((markdown) => !markdown.includes("murmur-pending://"))).toBe(true);
  await page.getByRole("button", { name: "Close", exact: true }).click();

  await page.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitem", { name: /Save to vault/ }).click();
  await expect.poll(() => page.evaluate(() => !!(window as any).__attachmentExported)).toBe(true);
  savedMarkdowns = await page.evaluate(
    () => ((window as any).__savedMarkdowns ?? []) as string[],
  );
  expect(savedMarkdowns.every((markdown) => !markdown.includes("murmur-pending://"))).toBe(true);

  // Both Markdown URL forms below are hostile; only the allow-listed internal
  // attachment may become an <img> in Preview.
  await body.evaluate((el: HTMLTextAreaElement) => {
    el.value +=
      '\n\n![Tracker](https://tracker.invalid/pixel.png)\n\n<img src="https://tracker.invalid/raw.png" alt="Raw">';
    el.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText" }));
  });
  const trackerRequests: string[] = [];
  page.on("request", (request) => {
    if (request.url().includes("tracker.invalid")) {
      trackerRequests.push(request.url());
    }
  });
  await page.getByRole("button", { name: "Preview", exact: true }).click();
  await expect(page.locator(".note-preview .md-attachment img")).toHaveCount(1);
  await expect(page.locator(".note-preview img")).toHaveCount(1);
  await expect(page.locator(".note-preview .md-image-blocked")).toHaveCount(2);
  expect(trackerRequests).toEqual([]);
  savedMarkdowns = await page.evaluate(
    () => ((window as any).__savedMarkdowns ?? []) as string[],
  );
  expect(savedMarkdowns.every((markdown) => !markdown.includes("murmur-pending://"))).toBe(true);
});

test("image-only Cmd-V inserts one internal image block and no filename text", async ({
  page,
}) => {
  await mockCanvasWebpEncoder(page);
  await mockNotes(page);
  await page.goto("/notes/n1");
  await enterEditMode(page);
  const body = page.locator(".body-area");
  await body.evaluate((el: HTMLTextAreaElement) => {
    const at = el.value.indexOf("Some body");
    el.focus();
    el.setSelectionRange(at, at);
    const png = Uint8Array.from(
      atob(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
      ),
      (char) => char.charCodeAt(0),
    );
    const transfer = new DataTransfer();
    transfer.items.add(
      new File([png], "private-project-screenshot.png", { type: "image/png" }),
    );
    el.dispatchEvent(
      new ClipboardEvent("paste", {
        bubbles: true,
        cancelable: true,
        clipboardData: transfer,
      }),
    );
  });

  await expect(body).toHaveValue(/!\[Screenshot\]\(murmur-attachment:\/\/[0-9a-f-]{36}\)/i);
  const value = await body.inputValue();
  expect(value).not.toContain("private-project-screenshot");
  expect(value.indexOf("murmur-attachment://")).toBeLessThan(
    value.indexOf("Some body text to select."),
  );
});

test("a compressed dimension bomb is rejected before the browser image decoder runs", async ({
  page,
}) => {
  await page.addInitScript(() => {
    (window as any).__imageDecodeCalls = 0;
    Object.defineProperty(window, "createImageBitmap", {
      configurable: true,
      value: async () => {
        (window as any).__imageDecodeCalls += 1;
        throw new Error("decoder should not run");
      },
    });
  });
  await mockNotes(page, {
    add_note_attachment: () => {
      (window as any).__dimensionBombCrossedIpc = true;
      throw new Error("dimension bomb crossed IPC");
    },
  });
  await page.goto("/notes/n1");
  await enterEditMode(page);
  const body = page.locator(".body-area");
  await body.evaluate((el: HTMLTextAreaElement) => {
    const png = Uint8Array.from(
      atob(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
      ),
      (char) => char.charCodeAt(0),
    );
    // Forge the authenticated IHDR dimensions to 12,001 x 12,001. The CRC is intentionally not
    // repaired: preflight must inspect the bounded header before any browser decoder allocation.
    for (const offset of [16, 20]) {
      png[offset] = 0;
      png[offset + 1] = 0;
      png[offset + 2] = 0x2e;
      png[offset + 3] = 0xe1;
    }
    const transfer = new DataTransfer();
    transfer.items.add(new File([png], "bomb.png", { type: "image/png" }));
    el.dispatchEvent(
      new ClipboardEvent("paste", {
        bubbles: true,
        cancelable: true,
        clipboardData: transfer,
      }),
    );
  });

  await expect(page.getByText(/dimensions are too large to process safely/i)).toBeVisible();
  expect(await page.evaluate(() => (window as any).__imageDecodeCalls)).toBe(0);
  expect(await page.evaluate(() => !!(window as any).__dimensionBombCrossedIpc)).toBe(false);
  await expect(body).not.toHaveValue(/murmur-pending:\/\//);
});

test("a locked note never asks for attachment bytes", async ({ page }) => {
  await mockNotes(page, {
    list_note_attachments: () => {
      (window as any).__lockedAttachmentReads =
        ((window as any).__lockedAttachmentReads ?? 0) + 1;
      return [];
    },
  });
  await page.goto("/notes/nlk");
  await expect(page.getByText(/locked folder/i)).toBeVisible();
  expect(
    await page.evaluate(() => (window as any).__lockedAttachmentReads ?? 0),
  ).toBe(0);
});


/**
 * A sealed-not-unlocked note (`get_note` returns the masked shape) renders the
 * lock gate instead of the body — no title/body leak — with no console errors.
 */
test("a locked note renders the lock gate (no body) with no console errors", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      consoleErrors.push(msg.text());
    }
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page);
  await page.goto("/notes/nlk");

  // The lock gate copy is present; the body textarea is NOT rendered.
  await expect(page.getByText(/locked folder/i)).toBeVisible();
  await expect(page.locator(".body-area")).toHaveCount(0);

  expect(consoleErrors).toEqual([]);
});

/**
 * The mode toggle: two GLYPHS, and which one starts pressed is decided by
 * whether there is anything to read.
 *
 * Both halves were previously unpinned. The default (`note-editor.component.ts`
 * `opensInPreview`) was only implied by the suite's `enterEditMode` helper —
 * which is idempotent, so it passes in EITHER mode and therefore proved nothing.
 * The labels were plain text, so nothing stopped a future change from dropping
 * the accessible name along with them.
 */
test("the mode toggle is icon-only, and a note with a body starts in Preview", async ({
  page,
}) => {
  await mockNotes(page);
  await page.goto("/notes/n1");

  const seg = page.getByRole("group", { name: "Edit or preview" });
  const edit = seg.getByRole("button", { name: "Edit", exact: true });
  const preview = seg.getByRole("button", { name: "Preview", exact: true });

  // Named for assistive tech and for this suite, but carrying no visible text —
  // the name now comes from `aria-label`, not from a label the eye can read.
  await expect(edit).toBeVisible();
  await expect(preview).toBeVisible();
  await expect(seg).not.toContainText(/\S/);
  await expect(edit.locator("svg")).toHaveCount(1);
  await expect(preview.locator("svg")).toHaveCount(1);

  // `get_note` returns "# Heading\n\nSome body text…", so there IS something to
  // read: the note opens rendered, not as raw markdown in a textarea.
  await expect(preview).toHaveAttribute("aria-pressed", "true");
  await expect(edit).toHaveAttribute("aria-pressed", "false");
  await expect(page.locator(".note-preview")).toBeVisible();
  await expect(page.locator(".body-area")).toHaveCount(0);

  // The toggle still toggles, and state is exposed programmatically rather than
  // by colour alone.
  await edit.click();
  await expect(edit).toHaveAttribute("aria-pressed", "true");
  await expect(preview).toHaveAttribute("aria-pressed", "false");
  await expect(page.locator(".body-area")).toBeVisible();
});

test("a brand-new empty note starts in Edit, not in a read-only empty pane", async ({
  page,
}) => {
  await mockNotes(page, {
    get_note: (args: { id: string }) => ({
      id: args.id,
      title: "",
      folderId: "nf1",
      markdown: "",
      tags: [],
      properties: {},
      updatedAt: 1_720_000_000_000,
      createdAt: 1_720_000_000_000,
      exportedPath: null,
      locked: false,
      shared: false,
    }),
  });
  await page.goto("/notes/n1");

  const seg = page.getByRole("group", { name: "Edit or preview" });
  await expect(seg.getByRole("button", { name: "Edit", exact: true })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  // A read-only empty pane is a dead end where the user meant to start typing.
  await expect(page.locator(".body-area")).toBeVisible();
  await expect(page.locator(".note-preview")).toHaveCount(0);
});
