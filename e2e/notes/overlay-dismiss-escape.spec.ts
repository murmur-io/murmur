import { expect, test } from "@playwright/test";
import { enterEditMode, mockNotes } from "./mock-invoke";

async function selectBodyText(page: import("@playwright/test").Page): Promise<void> {
  const body = page.locator(".body-area");
  await expect(body).toBeVisible();
  await body.evaluate((el: HTMLTextAreaElement) => {
    const start = el.value.indexOf("body text");
    el.focus();
    el.setSelectionRange(start, start + "body text".length);
    el.dispatchEvent(new MouseEvent("mouseup", { bubbles: true }));
  });
}

async function expectNoTransientNoteUi(
  page: import("@playwright/test").Page,
): Promise<void> {
  for (const selector of [
    ".sel-bar",
    ".brain-pop",
    ".link-pop",
    ".slash-menu",
    ".head-menu",
    ".menu-backdrop",
  ]) {
    await expect(page.locator(selector)).toHaveCount(0);
  }
}

async function backgroundAndReturnToCachedNote(
  page: import("@playwright/test").Page,
): Promise<void> {
  await page
    .getByRole("navigation", { name: "Primary navigation" })
    .getByRole("link", { name: "Capture", exact: true })
    .dispatchEvent("click");
  await expect(page).toHaveURL(/\/record$/);
  await expectNoTransientNoteUi(page);

  // Browser Back drives Angular's router back to the stored `/notes/:id`
  // handle without creating a fresh page or injecting a pointer event.
  await page.goBack();
  await expect(page).toHaveURL(/\/notes\/n1$/);
  await expect(page.locator(".body-area")).toBeVisible();
  await expectNoTransientNoteUi(page);
}

test("note selection overlays stay interactive and dismiss when the pointer leaves the editor", async ({
  page,
}) => {
  await mockNotes(page);
  await page.goto("/notes/n1");
  await enterEditMode(page);

  await selectBodyText(page);
  const toolbar = page.locator(".sel-bar");
  await expect(toolbar).toBeVisible();

  // Other note chrome is outside the body selection's owning surface.
  await page.locator(".note-title-input").click();
  await expect(toolbar).toHaveCount(0);

  await selectBodyText(page);
  await expect(toolbar).toBeVisible();

  // The teleported toolbar is part of the editor interaction even though its
  // DOM node lives under <body>. Clicking it must not trigger click-away.
  await toolbar.getByRole("button", { name: "Ask Brain" }).click();
  const popover = page.locator(".brain-pop");
  await expect(popover).toBeVisible();

  await popover.getByPlaceholder("Ask Brain to edit…").click();
  await expect(popover).toBeVisible();

  await page.locator(".note-title-input").click();
  await expect(popover).toHaveCount(0);
  await expect(toolbar).toHaveCount(0);

  await selectBodyText(page);
  await toolbar.getByRole("button", { name: "Ask Brain" }).click();
  await expect(popover).toBeVisible();

  // Leaving the owning editor for app chrome dismisses the transient overlay.
  await page
    .getByRole("navigation", { name: "Primary navigation" })
    .getByRole("link", { name: "Capture", exact: true })
    .click();
  await expect(page).toHaveURL(/\/record$/);
  await expect(popover).toHaveCount(0);
  await expect(toolbar).toHaveCount(0);
});

test("a cached note route clears every editing overlay and never resurrects it on return", async ({
  page,
}) => {
  await mockNotes(page, {
    list_link_candidates: () => [
      { kind: "note", id: "n2", title: "Weekly plan", snippet: "" },
    ],
  });
  await page.goto("/notes/n1");
  await enterEditMode(page);

  // Brain is teleported to <body>; pointer-free backgrounding must detach it.
  await selectBodyText(page);
  await page.locator(".sel-bar").getByRole("button", { name: "Ask Brain" }).click();
  await expect(page.locator(".brain-pop")).toBeVisible();
  await backgroundAndReturnToCachedNote(page);

  // The link picker is also teleported and carries async candidate state.
  let body = page.locator(".body-area");
  await body.click();
  await body.evaluate((el: HTMLTextAreaElement) => {
    el.setSelectionRange(el.value.length, el.value.length);
  });
  await body.press("Enter");
  await body.press("/");
  await page
    .locator(".slash-menu")
    .getByRole("option", { name: "Link to note" })
    .click();
  await expect(page.locator(".link-pop")).toBeVisible();
  await backgroundAndReturnToCachedNote(page);

  // Inline slash-menu state must be cleared even though it has no teleported node.
  body = page.locator(".body-area");
  await body.click();
  await body.evaluate((el: HTMLTextAreaElement) => {
    el.setSelectionRange(el.value.length, el.value.length);
  });
  await body.press("Enter");
  await body.press("/");
  await expect(page.locator(".slash-menu")).toBeVisible();
  await backgroundAndReturnToCachedNote(page);

  // Header menu + backdrop are cached component state too.
  await page.getByRole("button", { name: "More actions" }).click();
  await expect(page.locator(".head-menu")).toBeVisible();
  await expect(page.locator(".menu-backdrop")).toBeVisible();
  await backgroundAndReturnToCachedNote(page);
});

test("outside-dismiss pointerdown does not consume header or slash-menu actions", async ({
  page,
}) => {
  await mockNotes(page, {
    list_link_candidates: () => [
      { kind: "note", id: "n2", title: "Weekly plan", snippet: "" },
    ],
  });
  await page.goto("/notes/n1");
  await enterEditMode(page);

  await page.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitem", { name: /Share/ }).click();
  await expect(page.locator(".share-modal")).toBeVisible();
  await page.getByRole("button", { name: "Close", exact: true }).click();

  const body = page.locator(".body-area");
  await body.click();
  await body.evaluate((el: HTMLTextAreaElement) => {
    el.setSelectionRange(el.value.length, el.value.length);
  });
  await body.press("Enter");
  await body.press("/");

  const slashMenu = page.locator(".slash-menu");
  await expect(slashMenu).toBeVisible();
  await slashMenu.getByRole("option", { name: "Link to note" }).click();
  const picker = page.locator(".link-pop");
  await expect(picker).toBeVisible();
  await picker.getByText("Weekly plan").click();
  await expect(body).toHaveValue(/\[\[Weekly plan\]\]/);
});

test("main-shell Escape is consumed after its document-level overlay handler runs", async ({
  page,
}) => {
  await mockNotes(page);
  await page.goto("/notes/n1");

  await page
    .getByRole("navigation", { name: "Primary navigation" })
    .getByRole("button", { name: "Search", exact: true })
    .click();
  await expect(page.locator(".qs-scrim")).toBeVisible();

  const prevented = await page.evaluate(() => {
    const event = new KeyboardEvent("keydown", {
      key: "Escape",
      bubbles: true,
      cancelable: true,
    });
    document.body.dispatchEvent(event);
    return event.defaultPrevented;
  });

  // Existing document-level Escape behavior still gets first chance to close
  // the overlay; the shell then blocks the native window fallback.
  await expect(page.locator(".qs-scrim")).toHaveCount(0);
  expect(prevented).toBe(true);
});
