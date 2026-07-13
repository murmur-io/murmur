import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * RED-before-GREEN — "Scheduled briefs" section never visually expands for a
 * pending brief despite `briefs.component.ts`'s doc comment claiming it does.
 *
 * `open` was a plain `signal(false)` never derived from `pendingCount()` — the
 * template's `@if (open() || pendingCount() > 0)` let the pending-run action
 * card peek through while the section header (`aria-expanded` + chevron
 * rotation), both driven by `open()` alone, stayed visually collapsed. A user
 * with a pending brief saw a lone Dismiss/Save-to-vault card floating under a
 * header that still looked collapsed, with no cue the schedules list / create
 * form exist underneath.
 *
 * RED contract: pre-fix, `aria-expanded` reads "false" and `.br-chevron` lacks
 * `.is-open` even though the pending-run card is visible — this spec's first
 * two assertions fail against the unpatched code. Post-fix, the header's
 * `isExpanded()` mirrors the same `open() || pendingCount() > 0` condition
 * that already gates the pending-run card, so the header visually agrees with
 * what's rendered beneath it.
 */
test("Scheduled briefs: header visually expands when a brief is pending", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") consoleErrors.push(msg.text());
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockTauri(page, {
    list_brief_schedules: () => [
      {
        id: "sched-1",
        label: "Monday kickoff",
        dayOfWeek: 0,
        hourLocal: 8,
        minuteLocal: 30,
        scopeDays: 7,
        promptHint: null,
        enabled: true,
        lastRunAt: null,
        createdAt: "2026-07-01T00:00:00Z",
      },
    ],
    list_brief_runs: () => [
      {
        id: "run-1",
        scheduleId: "sched-1",
        status: "pending",
        noteMd: "## Summary\nEverything is on track.",
        meetingIds: ["m-1"],
        proposedAt: "2026-07-12T08:30:00Z",
        acceptedAt: null,
      },
    ],
  });

  await page.goto("/brain");

  const section = page.locator("app-briefs");
  await expect(section).toBeVisible();

  // The pending-run card is visible without any click (existing behavior).
  await expect(section.locator(".br-run")).toBeVisible();

  // The header chrome must agree: expanded, chevron rotated open.
  const toggle = section.locator(".br-toggle");
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  await expect(section.locator(".br-chevron")).toHaveClass(/is-open/);

  expect(consoleErrors).toEqual([]);
});
