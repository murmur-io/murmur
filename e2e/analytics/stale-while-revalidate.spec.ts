import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Analytics (+ its Topic-threads / Egress-ledger children) — a navigate-away
 * -and-back to `/analytics` must show the LAST-KNOWN numbers INSTANTLY, not
 * blank the whole dashboard to "Loading…" first. `/analytics` is not one of
 * the three routes `TabRouteReuseStrategy` keeps alive (`meeting/:id` /
 * `notes/:id` / `org-item/:id`), so `AnalyticsComponent` (and its
 * `TopicThreadsComponent` / `EgressLedgerComponent` children) are genuinely
 * destroyed and recreated on every visit — the stale-while-revalidate
 * contract (`angular-zoneless.md` §8) is what must survive that, via the
 * root-persisted `AnalyticsStore` / `TopicThreadsStore` / `EgressLedgerStore`.
 *
 * The IPC responses below carry an artificial delay so a REGRESSION (state
 * wiped back to component-local signals) is observable: on the return visit
 * the numbers would disappear behind "Loading…" until the delayed promise
 * resolves. On the fix, the numbers stay on screen the whole time — the
 * (still real, still unconditional) background refetch just replaces them
 * silently once it resolves.
 *
 * RED contract: reverting the `AnalyticsStore`/`TopicThreadsStore`/
 * `EgressLedgerStore` root services back to component-local `signal()`s (and
 * the templates back to gating solely on `loading()`) makes this spec fail —
 * the hero stat / thread label / egress tile assertions taken immediately
 * after the second `page.goto("/analytics")` would find "Loading…" instead.
 */
test("Analytics dashboard shows cached data instantly on a return visit, not a Loading flash", async ({
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
    // A deliberate delay on every relevant command so a wiped-state regression
    // would show "Loading…" for a beat on the SECOND visit too.
    get_analytics: () =>
      new Promise((resolve) =>
        setTimeout(
          () =>
            resolve({
              totalMeetings: 47,
              totalDurationS: 95880,
              avgDurationS: 2040,
              longestDurationS: 3720,
              meetings7d: 9,
              duration7dS: 17640,
              notesCount: 44,
              firstMeetingAt: new Date(
                Date.now() - 58 * 86_400_000,
              ).toISOString(),
              byStatus: [{ status: "EXPORTED", count: 41 }],
              perDay: [],
            }),
          400,
        ),
      ),
    topic_threads: () =>
      new Promise((resolve) =>
        setTimeout(
          () =>
            resolve([
              {
                label: "Loopback blocker",
                count: 3,
                mentions: [
                  {
                    meetingId: "m-1",
                    title: "Eng Sync",
                    startedAt: new Date().toISOString(),
                    startS: 12,
                  },
                ],
              },
            ]),
          400,
        ),
      ),
    get_egress_ledger: () =>
      new Promise((resolve) =>
        setTimeout(
          () =>
            resolve({
              totalCalls: 12,
              totalTokens: 48000,
              byModel: [{ model: "claude-sonnet-4-6", tokens: 48000 }],
              byDay: [],
              totalRedactions: { email: 2, card: 0, phone: 1, name: 3 },
              recent: [],
            }),
          400,
        ),
      ),
  });

  // First visit: the dashboard loads (spinner is fine — nothing cached yet).
  // A full `page.goto` is fine here — it's the initial app boot either way.
  await page.goto("/analytics");
  await expect(page.getByText("Total meetings")).toBeVisible({
    timeout: 5_000,
  });
  await expect(page.getByText("Loopback blocker")).toBeVisible({
    timeout: 5_000,
  });
  await expect(page.getByText("Cloud calls")).toBeVisible({ timeout: 5_000 });

  // Navigate away via an IN-APP router link (NOT `page.goto`, which forces a
  // full browser navigation and reboots the whole Angular app — including
  // the root services this fix relies on — defeating the very persistence
  // being tested). This is a plain SPA route swap to a route NOT covered by
  // `TabRouteReuseStrategy`, so `/analytics` and its children are genuinely
  // destroyed.
  // Browse is a disclosure group inside the one sidebar (it used to be a
  // separate "Browse sidebar" complementary panel), and it starts collapsed.
  const browseSidebar = page.getByRole("navigation", {
    name: "Browse destinations",
  });
  await browseSidebar
    .getByRole("button", { name: "Browse", exact: true })
    .click();
  await browseSidebar
    .getByRole("link", { name: "Meetings", exact: true })
    .click();
  await expect(page.getByText("Total meetings")).toBeHidden();

  // Navigate BACK — the cached numbers/threads/ledger must render INSTANTLY,
  // i.e. before the freshly-delayed IPC promises above have any chance to
  // resolve (they take 400ms; assert well inside that window).
  await browseSidebar
    .getByRole("link", { name: "Analytics", exact: true })
    .click();
  await expect(page.getByText("Total meetings")).toBeVisible({
    timeout: 250,
  });
  await expect(page.locator(".stat-value", { hasText: "47" })).toBeVisible({
    timeout: 250,
  });
  await expect(page.getByText("Loopback blocker")).toBeVisible({
    timeout: 250,
  });
  await expect(page.getByText("Cloud calls")).toBeVisible({ timeout: 250 });

  expect(consoleErrors).toEqual([]);
});
