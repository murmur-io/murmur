import { test, expect } from "@playwright/test";
import { mockTauri } from "./mock-invoke";

/**
 * Smoke — the Live-context panel (connector enrichment) mounted right after the note summary in the
 * meeting-detail Note tab. Mocks the two enrich commands so the panel is fully drivable with no Rust
 * core:
 *   enrich_note_context  → a couple of ContextHits
 *   apply_note_enrichment → a NoteDto (records the hits it was called with)
 *
 * Confirms: the egress hint is present + loud; fetch → preview renders with (via …) attribution;
 * Add → apply called with the hits; Clear → apply called with []; NO NG0600 / ɵcmp / console error.
 * (Lane A "Refresh links" was removed — links auto-refresh on finalize.)
 */
test("live-context panel drives fetch/add/clear with attribution and no console errors", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      consoleErrors.push(msg.text());
    }
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockTauri(page, {
    enrich_note_context: () => [
      { source: "Jira", detail: "ATLAS-42 — Windows loopback spike (In Progress)", url: "https://example.atlassian.net/browse/ATLAS-42" },
      { source: "Slack", detail: "#eng — Marcus: shared sync layer merged", url: null },
    ],
    // Record what apply was called with on window so the test can assert it.
    apply_note_enrichment: (args: { meetingId: string; hits: unknown[] }) => {
      (window as unknown as { __applyCalls: unknown[] }).__applyCalls ??= [];
      (window as unknown as { __applyCalls: unknown[][] }).__applyCalls.push(args.hits);
      return {
        meetingId: args.meetingId,
        providerId: "claude_code",
        markdown: "# Q2 Roadmap Planning\n\n> [!context]- Live context\n> updated",
        exportedPath: "/Users/demo/Obsidian/Sonora/Meetings/Q2-Roadmap-Planning.md",
      };
    },
  });

  await page.goto("/meeting/m-atlas-roadmap");

  const panel = page.locator("app-stage2-panel");
  await expect(panel).toBeVisible();

  // The egress affordance is present and loud (compact one-line hint, not the old warning block).
  await expect(panel.locator(".live-hint")).toContainText(/sends a redacted query/i);

  // ── Fetch (the egress moment) → preview renders with (via …) attribution. ──
  await panel.getByRole("button", { name: "Fetch live context" }).click();
  const hits = panel.locator(".hit-item");
  await expect(hits).toHaveCount(2);
  await expect(hits.nth(0)).toContainText("via Jira");
  await expect(hits.nth(0)).toContainText("ATLAS-42");
  await expect(hits.nth(1)).toContainText("via Slack");

  // ── Add to note → apply_note_enrichment called with the 2 hits. ──
  await panel.getByRole("button", { name: "Add to note" }).click();
  await expect(panel.getByText(/the note now carries the live context/i)).toBeVisible();
  const afterAdd = await page.evaluate(
    () => (window as unknown as { __applyCalls: unknown[][] }).__applyCalls,
  );
  expect(afterAdd).toHaveLength(1);
  expect(afterAdd[0]).toHaveLength(2);

  // Re-fetch so Clear is available again (Add cleared the applied latch on next fetch).
  await panel.getByRole("button", { name: "Fetch live context" }).click();
  await expect(panel.locator(".hit-item")).toHaveCount(2);

  // ── Clear → apply_note_enrichment called with [] (byte-exact undo). ──
  await panel.getByRole("button", { name: "Clear" }).click();
  await expect(panel.getByText(/the live-context callout was removed/i)).toBeVisible();
  const afterClear = await page.evaluate(
    () => (window as unknown as { __applyCalls: unknown[][] }).__applyCalls,
  );
  expect(afterClear).toHaveLength(2);
  expect(afterClear[1]).toHaveLength(0);

  // No NG0600 / ɵcmp / any other console error surfaced through the whole drive.
  expect(consoleErrors).toEqual([]);
});
