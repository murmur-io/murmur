import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Analytics — 30-day activity chart hover tooltip must never be occluded by a
 * taller neighboring bar.
 *
 * `.chart-tip` is an absolutely-positioned box (`bottom: 100%` of its OWN
 * `.chart-bar`), so its vertical position tracks the hovered bar's height —
 * when a later sibling column has a taller bar, that neighbor's `.chart-bar`
 * (each bar is its own `position: relative` stacking context, painted in DOM
 * order) visually covers the tip's text, because the tip's own `z-index: 2`
 * only wins WITHIN its own bar's stacking context, not against a sibling's.
 *
 * Fix: `.chart-col:hover` gets `z-index: 3` (and `.chart-col` gets
 * `position: relative` so z-index applies) — lifting the ENTIRE hovered
 * column (bar + tip) above every sibling regardless of neighbor height.
 *
 * RED contract: `.chart-tip` has `pointer-events: none` (intentional — the
 * tip must not eat hover off the bar underneath it), so a naive
 * `elementFromPoint` at the tip's center always reports whatever is
 * genuinely painted there, skipping the tip only if something else paints
 * OVER it — which is exactly the occlusion bug. Pre-fix, a taller neighbor
 * bar wins that pixel; post-fix, the tip itself wins (nothing paints over
 * it, so `elementFromPoint` still reports the bar geometry only where the
 * tip does NOT cover, and reports NOTHING taller-neighbor at the tip's
 * horizontal center once the whole column is lifted). This test asserts the
 * concrete stacking mechanism the fix relies on: the hovered column's
 * resolved z-index must be numerically higher than a taller, vertically
 * overlapping neighbor's.
 */
test.describe("Analytics — chart tooltip stacking", () => {
  test("a hovered bar's tooltip is not occluded by a taller neighboring bar", async ({
    page,
  }) => {
    await mockTauri(page);
    await page.goto("/analytics");

    const cols = page.locator(".chart-col");
    await expect(cols.first()).toBeVisible({ timeout: 10_000 });

    const count = await cols.count();
    expect(count).toBe(30);

    // Let the entrance animations (`grow-bar`, 560ms) settle before measuring
    // bar heights, otherwise every bar samples mid-animation near `scaleY(0)`.
    await page.waitForTimeout(700);

    // Find a column whose bar has a SHORTER neighbor 1-3 columns to the right
    // that is taller (reproduces the reported repro: "hover over any of the
    // first few chart bars ... where a taller bar sits immediately to the
    // right"). The demo mock's counts are non-uniform, so this is present.
    const heights = await page.evaluate(() =>
      Array.from(document.querySelectorAll(".chart-bar")).map(
        (el) => el.getBoundingClientRect().height,
      ),
    );
    let hoverIdx = -1;
    let tallerIdx = -1;
    for (let i = 0; i < heights.length - 3; i++) {
      for (let j = i + 1; j <= i + 3 && j < heights.length; j++) {
        if (heights[j] > heights[i] + 10) {
          hoverIdx = i;
          tallerIdx = j;
          break;
        }
      }
      if (hoverIdx >= 0) break;
    }
    expect(hoverIdx, "expected a bar with a taller neighbor within 3 columns").toBeGreaterThanOrEqual(0);

    const hoveredBar = cols.nth(hoverIdx).locator(".chart-bar");
    const box = await hoveredBar.boundingBox();
    expect(box).not.toBeNull();
    await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height / 2);

    const tip = cols.nth(hoverIdx).locator(".chart-tip");
    await expect(tip).toHaveCSS("opacity", "1");

    const stacking = await page.evaluate(
      ({ hoverIdx, tallerIdx }) => {
        const allCols = document.querySelectorAll(".chart-col");
        const hoveredCol = allCols[hoverIdx] as HTMLElement;
        const tallerCol = allCols[tallerIdx] as HTMLElement;
        const hoveredZ = getComputedStyle(hoveredCol).zIndex;
        const tallerZ = getComputedStyle(tallerCol).zIndex;
        const tipRect = hoveredCol
          .querySelector(".chart-tip")!
          .getBoundingClientRect();
        const tallerBarRect = tallerCol
          .querySelector(".chart-bar")!
          .getBoundingClientRect();
        // Do the tip and the taller neighbor's bar geometrically overlap
        // vertically? (the exact condition that produces the reported bug)
        const verticallyOverlaps =
          tipRect.bottom > tallerBarRect.top && tipRect.top < tallerBarRect.bottom;
        return {
          hoveredZ,
          tallerZ,
          verticallyOverlaps,
          hoveredIsHovered: hoveredCol.matches(":hover"),
        };
      },
      { hoverIdx, tallerIdx },
    );

    expect(stacking.hoveredIsHovered).toBe(true);
    expect(
      stacking.verticallyOverlaps,
      "test setup: the tip must actually vertically overlap the taller neighbor's bar to reproduce the bug",
    ).toBe(true);

    // The regression assertion: the hovered column must resolve to a HIGHER
    // stacking order than the taller neighbor it geometrically overlaps —
    // otherwise the neighbor paints over the tip (the reported bug).
    const hoveredZNum = stacking.hoveredZ === "auto" ? 0 : Number(stacking.hoveredZ);
    const tallerZNum = stacking.tallerZ === "auto" ? 0 : Number(stacking.tallerZ);
    expect(hoveredZNum).toBeGreaterThan(tallerZNum);
  });
});
