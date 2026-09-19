import { expect, test } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Oracles for the reminders placement + composer change.
 *
 * The placement case is the RED-before-GREEN one: with the card appended after
 * the note body, its distance from the viewport grows with the note, so a long
 * note put it several screens down. Seeding a deliberately long body makes that
 * mechanical rather than a matter of taste.
 */

const LONG_BODY = Array.from(
  { length: 90 },
  (_, i) => `## Section ${i}\n\nParagraph ${i} — ${"lorem ipsum dolor sit amet ".repeat(3)}`,
).join("\n\n");

const LONG_NOTE = {
  id: "n-atlas-prd",
  title: "Atlas — PRD v3",
  folderId: "nf-product",
  markdown: LONG_BODY,
  tags: ["atlas"],
  updatedAt: 1770000000000,
  createdAt: 1769000000000,
  exportedPath: "/vault/Notes/Atlas.md",
  locked: false,
  shared: false,
};

const ONE_SUGGESTION = [
  {
    id: "sg-1",
    title: "Send the revised sync-layer spec to Marcus",
    suggestedDueAt: 1770200000000,
    source: { kind: "note", id: "n-atlas-prd", title: "Atlas — PRD v3" },
  },
];

test("meeting follow-ups are one header click away, never buried in the note", async ({
  page,
}) => {
  await mockTauri(page, {}, { audit_reminder_suggestions: ONE_SUGGESTION });
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.goto("/meeting/m-q2-roadmap");

  // The affordance MOVED rather than disappeared (same shape as the note
  // editor's Reminders toggle): the panel no longer sits in the note flow at
  // all, so no note length can push it out of reach.
  await expect(page.locator("app-note-panel app-meeting-actions")).toHaveCount(0);
  const toggle = page.getByRole("button", { name: "Action items", exact: true });
  await expect(toggle).toBeVisible();
  await expect(toggle).toHaveAttribute("aria-expanded", "false");

  await toggle.click();
  const drawer = page.locator(".actions-drawer");
  await expect(drawer).toBeVisible();
  await expect(toggle).toHaveAttribute("aria-expanded", "true");

  // Fully inside the first viewport height, whatever the note is doing.
  const box = await drawer.boundingBox();
  expect(box).not.toBeNull();
  expect(box!.y).toBeLessThan(900);

  // The panel owns its close control; the header toggle owns the state.
  await drawer.getByRole("button", { name: "Close action items" }).click();
  await expect(drawer).toHaveCount(0);
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
});

test("the two right-docked drawers never stack on top of each other", async ({
  page,
}) => {
  await mockTauri(page, {}, { audit_reminder_suggestions: ONE_SUGGESTION });
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.goto("/meeting/m-q2-roadmap");

  await page.getByRole("button", { name: "Action items", exact: true }).click();
  await expect(page.locator(".actions-drawer")).toBeVisible();

  // Ask docks to the same right edge — opening it must retire the other one.
  await page.getByRole("button", { name: "Ask", exact: true }).click();
  await expect(page.locator(".ask-drawer")).toBeVisible();
  await expect(page.locator(".actions-drawer")).toHaveCount(0);

  await page.getByRole("button", { name: "Action items", exact: true }).click();
  await expect(page.locator(".actions-drawer")).toBeVisible();
  await expect(page.locator(".ask-drawer")).toHaveCount(0);
});

test("an opened drawer explains itself when the meeting has no action items", async ({
  page,
}) => {
  await mockTauri(page, {}, { get_action_items: [], audit_reminder_suggestions: [] });
  await page.goto("/meeting/m-q2-roadmap");

  await page.getByRole("button", { name: "Action items", exact: true }).click();
  const drawer = page.locator(".actions-drawer");
  await expect(drawer).toBeVisible();
  // An empty shell would read as a broken panel; the empty state says why.
  await expect(drawer.getByText("No action items in this note yet.")).toBeVisible();
});

test("idle surface keeps its affordance but drops the branded card", async ({
  page,
}) => {
  await mockTauri(
    page,
    {},
    { get_note: LONG_NOTE, audit_reminder_suggestions: [] },
  );
  await page.goto("/notes/n-atlas-prd");

  // The affordance MOVED rather than disappeared — twice now. 701be0fc replaced
  // the card's inline create button with the note's Reminders drawer; 2026-09-13
  // moved the card itself out of the note body into its own drawer. So an idle
  // note carries NO review chrome at all, and the affordance is the header
  // toggle beside the other two tool columns.
  const card = page.locator("app-smart-reminder-card");
  await expect(card).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Reminders", exact: true }),
  ).toBeVisible();

  // Summoned with nothing to review, the pane accounts for itself instead of
  // coming up blank. NOT asserted by the heading's literal text: the template
  // ships a CURLY apostrophe (U+2019) in "Don’t", so a straight-quote assertion
  // would match zero nodes on unchanged code and be vacuously green forever.
  await page
    .getByRole("button", { name: "Smart reminders", exact: true })
    .click();
  await expect(card.locator("section.smart-card")).toHaveCount(1);
  await expect(card.locator(".smart-kicker")).toHaveCount(1);
  await expect(card.getByText("Nothing to review right now.")).toBeVisible();
});

test("composer opens with a resolved due readback and preset chips", async ({
  page,
}) => {
  await mockTauri(page, {}, {
    get_note: LONG_NOTE,
    audit_reminder_suggestions: [],
    list_reminders: { inbox: [], upcoming: [], completed: [], dueInboxCount: 0 },
  });
  await page.goto("/notes/n-atlas-prd");

  // Through the drawer: the card's create button is gone (see the test above).
  await page.getByRole("button", { name: "Reminders", exact: true }).click();
  await page
    .locator("app-note-reminders-panel")
    .getByRole("button", { name: "New reminder" })
    .click();

  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();

  // The echo states the selected moment in words. Empty here would mean the
  // composer opened with no usable default — the `now + 1h` behaviour that
  // produced 00:09-style times.
  await expect(dialog.locator(".when-echo")).not.toBeEmpty();

  // "Tomorrow" resolves to the 09:00 convention the backend already uses
  // (reminder_audit.rs pins extracted suggestions to 09:00 local).
  await dialog.getByRole("button", { name: /^Tomorrow/ }).click();
  await expect(dialog.getByLabel("Time")).toHaveValue("09:00");
});

test("date and time fields are boxed like every other field", async ({ page }) => {
  await mockTauri(page, {}, {
    get_note: LONG_NOTE,
    audit_reminder_suggestions: [],
    list_reminders: { inbox: [], upcoming: [], completed: [], dueInboxCount: 0 },
  });
  await page.goto("/notes/n-atlas-prd");
  await page.getByRole("button", { name: "Reminders", exact: true }).click();
  await page
    .locator("app-note-reminders-panel")
    .getByRole("button", { name: "New reminder" })
    .click();

  const dialog = page.getByRole("dialog");
  const title = await dialog.getByLabel("Title").boundingBox();
  const date = await dialog.getByLabel("Date").boundingBox();
  const time = await dialog.getByLabel("Time").boundingBox();

  // `input[type=date|time]` were absent from the base-box selector list while the
  // bare `input:focus` rules still matched them — so they rendered borderless and
  // short next to every other control. Equal height is the mechanical proof the
  // selector fix landed, and it is the one assertion that only the real engine
  // can make (webkit runs this too).
  expect(date!.height).toBeCloseTo(title!.height, 0);
  expect(time!.height).toBeCloseTo(title!.height, 0);
});
