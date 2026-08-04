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
  properties: {},
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

test("meeting follow-ups are reachable without scrolling past the whole note", async ({
  page,
}) => {
  await mockTauri(page, {}, { audit_reminder_suggestions: ONE_SUGGESTION });
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.goto("/meeting/m-q2-roadmap");

  const actions = page.locator("app-meeting-actions");
  await expect(actions).toBeVisible();

  const box = await actions.boundingBox();
  expect(box).not.toBeNull();
  // Inside the first viewport height. Previously this sat at line ~455 of a
  // 481-line template — below Summary, Decisions, Related and the Q&A log.
  expect(box!.y).toBeLessThan(900);
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

  const card = page.locator("app-smart-reminder-card");
  await expect(card.getByRole("button", { name: "New reminder" })).toBeVisible();
  // The kicker and the imperative heading are gone when there is nothing to
  // review. NOT asserted by its literal text: the template ships a CURLY
  // apostrophe (U+2019) in "Don’t", so a straight-quote assertion would match
  // zero nodes on unchanged code and be vacuously green forever.
  await expect(card.locator(".smart-kicker")).toHaveCount(0);
  await expect(card.locator("section.smart-card.is-strip")).toHaveCount(1);
});

test("composer opens with a resolved due readback and preset chips", async ({
  page,
}) => {
  await mockTauri(page, {}, { get_note: LONG_NOTE, audit_reminder_suggestions: [] });
  await page.goto("/notes/n-atlas-prd");

  await page
    .locator("app-smart-reminder-card")
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
  await mockTauri(page, {}, { get_note: LONG_NOTE, audit_reminder_suggestions: [] });
  await page.goto("/notes/n-atlas-prd");
  await page
    .locator("app-smart-reminder-card")
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
