import { test, expect } from "@playwright/test";
import { enterEditMode, mockNotes } from "./mock-invoke";

/**
 * The note body's focus frame must not hug the text. The textarea keeps zero padding so its
 * lines sit exactly where Preview renders them, so the frame is pushed OUTWARD with
 * outline-offset — and it must not be clipped by an ancestor, or the fix is invisible.
 */
test("note body focus frame stands off the text and is not clipped", async ({ page }) => {
  await mockNotes(page);
  await page.goto("/notes/n1");
  await enterEditMode(page);

  const body = page.locator(".body-area");
  await body.focus();

  const frame = await body.evaluate((el) => {
    const cs = getComputedStyle(el);
    const offset = parseFloat(cs.outlineOffset);
    const width = parseFloat(cs.outlineWidth);
    const r = el.getBoundingClientRect();
    const reach = offset + width;
    // Every ancestor that clips must leave room for the ring on the left and right.
    let clipped = false;
    for (let p = el.parentElement; p; p = p.parentElement) {
      const pcs = getComputedStyle(p);
      if (pcs.overflowX === "visible" && pcs.overflow === "visible") continue;
      const pr = p.getBoundingClientRect();
      if (r.left - reach < pr.left - 0.5 || r.right + reach > pr.right + 0.5) clipped = true;
    }
    return { offset, width, style: cs.outlineStyle, paddingLeft: parseFloat(cs.paddingLeft), clipped };
  });

  expect(frame.style).not.toBe("none");
  expect(frame.width).toBeGreaterThan(0);
  expect(frame.offset).toBeGreaterThanOrEqual(8);
  // Text alignment with Preview is preserved: the gap comes from the offset, not padding.
  expect(frame.paddingLeft).toBe(0);
  expect(frame.clipped).toBe(false);

  await page.screenshot({ path: test.info().outputPath("focus-frame.png") });
});
