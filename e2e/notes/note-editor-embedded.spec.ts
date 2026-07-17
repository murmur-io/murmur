import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * EMBEDDED-mode smoke (2026-07-17) — the shipped `NoteEditorComponent` gains an
 * additive `embedded` mode so the recording panel's "Note" tab can host the REAL
 * create-note editing experience on a companion note. This spec proves BOTH
 * halves of the contract:
 *
 *   (a) the ROUTED `/notes/:id` path is unchanged — header + title + properties
 *       still render (regression gate for `embedded()===false`);
 *   (b) a `<app-note-editor [embedded]="true" [noteIdInput]="'n1'">` mount shows
 *       ONLY the body editor + a working selection toolbar / Ask Brain popover —
 *       NO header, NO title input, NO properties bar, NO backlinks — and loads
 *       its note from `noteIdInput`, NOT the route.
 *
 * There is no bare-component test-host route in the app (the real host is the
 * recording panel, owned by a separate change), so (b) mounts a SECOND, real
 * embedded instance via Angular's dev-mode global (`window.ng`) — it grabs the
 * routed instance's constructor + the app's root EnvironmentInjector, then
 * `createComponent` + `setInput('embedded', true)` / `setInput('noteIdInput',
 * 'n1')`. This exercises the ACTUAL signal-input codepath (not a CSS fake) with
 * ZERO production-route changes. `ng serve` uses the development config
 * (optimization off, dev mode ON) so `window.ng` is present. Smoke aid, not a
 * gate — the hard gates are `ng lint` + `ng build`.
 */

test("(a) routed /notes/:id still renders header + title + properties (embedded=false unchanged)", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") consoleErrors.push(msg.text());
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page);
  await page.goto("/notes/n1");

  // The full routed chrome is present.
  await expect(page.locator(".editor-head")).toBeVisible();
  await expect(page.locator(".note-title-input")).toHaveValue("My First Note");
  await expect(page.locator(".props")).toBeVisible();
  await expect(page.getByRole("button", { name: "Preview", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Share", exact: true })).toBeVisible();

  // Not embedded → the section has no is-embedded class.
  await expect(page.locator("section.editor.is-embedded")).toHaveCount(0);

  expect(consoleErrors).toEqual([]);
});

test("(b) embedded mount shows ONLY the body + working Ask Brain — no header/title/properties/backlinks", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") consoleErrors.push(msg.text());
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page);
  // Load the routed editor first so the note-editor lazy chunk + component are
  // registered; we then mount a SECOND, real embedded instance from it.
  await page.goto("/notes/n1");
  await expect(page.locator(".note-title-input")).toBeVisible();

  // Mount `<app-note-editor [embedded]="true" [noteIdInput]="'n1'">` for REAL via
  // the dev-mode Angular globals. The instance renders into a fresh host div
  // appended to <body> (scoped `data-embed-host`). We resolve `@angular/core`'s
  // `createComponent` + `ApplicationRef` by dynamically importing the loaded core
  // dev-chunk (found by scanning the already-fetched module URLs), create the
  // standalone component into the host with the app's root EnvironmentInjector
  // (so DI — IpcService etc. — resolves), attach its view to the running
  // ApplicationRef, then `setInput` drives the REAL signal inputs and CD flushes
  // the (zoneless) input-keyed load effect + template.
  await page.evaluate(async () => {
    const ng = (window as unknown as { ng: any }).ng;
    const routedHost = document.querySelector("app-note-editor")!;
    const routedCmp = ng.getComponent(routedHost);
    const Ctor = routedCmp.constructor;
    const rootInjector = ng.getInjector(document.querySelector("app-root")!);

    // Find + import the loaded @angular/core dev chunk (the one exporting
    // `createComponent` + `ApplicationRef`). Dev-serve emits per-package chunks;
    // scan the fetched script URLs and pick the module that has both exports.
    const urls = performance
      .getEntriesByType("resource")
      .map((e) => (e as PerformanceResourceTiming).name)
      .filter((n) => n.endsWith(".js") || n.includes("chunk") || n.includes("@angular"));
    let core: any = null;
    for (const url of urls) {
      try {
        const mod = await import(/* @vite-ignore */ url);
        if (typeof mod.createComponent === "function" && mod.ApplicationRef) {
          core = mod;
          break;
        }
      } catch {
        /* not an importable module URL — skip */
      }
    }
    if (!core) {
      throw new Error("could not locate @angular/core createComponent/ApplicationRef");
    }

    // Host the component in a real <app-note-editor> element (its own selector)
    // wrapped in a scoping div — `createComponent(hostElement)` renders the view
    // INTO the given element, so using the matching tag keeps the DOM shape the
    // real router produces.
    const wrap = document.createElement("div");
    wrap.setAttribute("data-embed-host", "");
    wrap.style.cssText =
      "position:fixed;top:0;left:0;width:420px;height:600px;z-index:9999;background:#111";
    const host = document.createElement("app-note-editor");
    wrap.appendChild(host);
    document.body.appendChild(wrap);

    // `createComponent` needs the ROOT EnvironmentInjector (it hosts
    // RendererFactory2); the element injector from `getInjector(app-root)` chains
    // up to it, and `ApplicationRef.injector` IS that root environment injector.
    const appRef = rootInjector.get(core.ApplicationRef);
    const envInjector = appRef.injector;
    const compRef = core.createComponent(Ctor, {
      environmentInjector: envInjector,
      hostElement: host,
    });
    appRef.attachView(compRef.hostView);
    compRef.setInput("embedded", true);
    compRef.setInput("noteIdInput", "n1");
    ng.applyChanges(compRef.instance);
    (window as unknown as { __embedRef: any }).__embedRef = compRef;
  });

  // The embedded instance renders inside the host div.
  const embed = page.locator("[data-embed-host] app-note-editor");
  await expect(embed).toBeVisible();
  await expect(embed.locator("section.editor.is-embedded")).toBeVisible();

  // Its body loads from noteIdInput (get_note('n1')) — the same body text.
  const body = embed.locator(".body-area");
  await expect(body).toBeVisible();
  await expect(body).toHaveValue(/Some body text to select\./);

  // CHROME IS GONE: no header, no title input, no properties bar, no backlinks,
  // no Preview/Share toggle.
  await expect(embed.locator(".editor-head")).toHaveCount(0);
  await expect(embed.locator(".note-title-input")).toHaveCount(0);
  await expect(embed.locator(".props")).toHaveCount(0);
  await expect(embed.locator("app-backlinks")).toHaveCount(0);
  await expect(embed.getByRole("button", { name: "Preview", exact: true })).toHaveCount(0);

  // The IN-NOTE Ask Brain works embedded: select body text → the formatting
  // bubble floats → "Ask Brain" opens the brain popover over the SAME selection.
  await body.evaluate((el: HTMLTextAreaElement) => {
    const start = el.value.indexOf("body text");
    el.focus();
    el.setSelectionRange(start, start + "body text".length);
    el.dispatchEvent(new MouseEvent("mouseup", { bubbles: true }));
  });
  const bubble = embed.locator("app-note-selection-toolbar");
  await expect(bubble).toBeVisible();
  await bubble.getByRole("button", { name: "Ask Brain" }).dispatchEvent("click");
  await expect(embed.locator("app-note-brain-popover")).toBeVisible();

  // Tear the embedded instance down cleanly (no leaked effect across tests).
  await page.evaluate(() => {
    const w = window as unknown as { __embedRef: any };
    w.__embedRef?.destroy?.();
    document.querySelector("[data-embed-host]")?.remove();
  });

  expect(consoleErrors).toEqual([]);
});
