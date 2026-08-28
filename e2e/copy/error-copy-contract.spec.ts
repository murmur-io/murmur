import { test, expect, type Page, type Locator } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * The three properties `ErrorCopyService` exists to guarantee, pinned exhaustively.
 *
 * 1. ALL FOURTEEN `AppError` variant prefixes are stripped before the `[code]` is read — a code
 *    that only survives the prefixes someone remembered to test is a code that silently stops
 *    working the first time a failure travels through a different variant.
 * 2. DENY-BY-DEFAULT: an un-coded failure renders the generic sentence no matter how harmless it
 *    looks, and an unknown code is treated as un-coded rather than trusted.
 * 3. The former `brain.component.ts::friendlyImportError` LADDER still classifies every document
 *    failure the way it used to — including the one ordering that was load-bearing, where
 *    `no text found` had to beat `unsupported document type` or a scanned PDF was reported as an
 *    unsupported file type.
 *
 * # Why an e2e and not a unit test
 *
 * This repo has no frontend unit runner (`angular.json` declares `build`, `serve` and `lint`
 * only) and adding one means adding npm packages, which is forbidden here. Playwright is the
 * project's frontend oracle, so the properties are pinned where they actually matter: on a real
 * rendered surface, through the real IPC boundary, against the real built code.
 *
 * # Why `/graph` is the vehicle
 *
 * There is exactly ONE decision table — `humanizeError`; `ErrorCopyService.humanize` delegates to
 * it and every surface in the app reaches it through one of those two doors. So the properties
 * above are properties of that table, and the surface only has to be an honest, minimal way to
 * put a failure in and read a rendered sentence out. `/graph` is the smallest one in the app: a
 * single `get_graph` call on load whose rejection is rendered verbatim by
 * `graph.component.html`'s `<p class="empty">{{ error() }}</p>`, with no retry, no toast queue
 * and no debounce to race. The rejection travels the whole real path — Tauri invoke → `catch` →
 * `errorCopy.humanize(e)` → template — so nothing here is a stub of the thing under test.
 *
 * The failure fixtures deliberately carry realistic INTERNAL prose after the code (the actual
 * vocabulary `errcode.rs` names as never-show: `HKDF expand failed`, `mutex poisoned`, an
 * absolute path). Every assertion checks both halves: the right sentence appeared AND none of
 * that prose did.
 */

/** Rendered copy, hard-coded on purpose: importing it from the module under test proves nothing. */
const COPY = {
  folderLocked: "That folder is locked — unlock it first.",
  recordingLinkedNote:
    "This recording has a linked note. Move it between open folders, or remove the destination folder lock first.",
  docNoText: "No readable text was found in that file, even after reading the images.",
  docUnsupported:
    "That file type can’t be imported. Try Markdown, text, PDF, Word, PowerPoint, Excel, HTML or an image.",
  docTooLarge:
    "That file goes past the safe import limit — either the file itself or the text it unpacks to is too big to add.",
  docPassword: "That PDF is password-protected — unlock it and try again.",
  docUnreadable:
    "That file couldn’t be read — it may be damaged or in an unexpected format.",
  generic: "Something went wrong. Please try again.",
} as const;

/**
 * Every `AppError` variant tag, exactly as `Display` writes it
 * (`src-tauri/src/error.rs` — `#[error("…: {0}")]` on each variant).
 *
 * Fourteen, and the list is deliberately complete rather than a sample. `Other(#[from]
 * anyhow::Error)` is `#[error(transparent)]` and contributes no tag at all, which is why it is
 * absent here and why an anonymous `Other` is covered by the deny-by-default test instead.
 */
const VARIANT_TAGS = [
  "audio capture error",
  "transcription error",
  "summarizer error",
  "export error",
  "storage error",
  "migration error",
  "authentication error",
  "locked",
  "secrets error",
  "keychain access denied",
  "biometric authentication failed or was cancelled",
  "config error",
  "provider unavailable",
  "invalid argument",
] as const;

/** Developer prose of exactly the kind that must never reach a screen. */
const INTERNAL = "db::conn mutex poisoned, HKDF expand failed at /Users/x/Library/App";

/**
 * Install the mock once. The override is self-contained (it closes over nothing in test scope —
 * `mockTauri` replays it page-side via `Function.prototype.toString()`), and reads which failure
 * to produce from the URL, because a page-side counter would reset on every navigation.
 */
async function boot(page: Page): Promise<void> {
  await mockTauri(page, {
    // `URLSearchParams.get` already percent-decodes, so this is the exact string the test
    // passed to `encodeURIComponent` — no second decode, which would corrupt a body
    // containing a literal `%`.
    get_graph: () =>
      Promise.reject(new URLSearchParams(location.search).get("rej") ?? ""),
  });
}

/**
 * Load `/graph` with `get_graph` rejecting exactly `wire`, and return the rendered sentence.
 *
 * Scoped to the graph section's ERROR card specifically: the loading branch renders its own
 * `<p class="empty">Loading…</p>` inside `.state-card`, so waiting on `.empty-state .empty` is
 * also what waits for the error branch to be the one on screen.
 */
async function renderedFor(page: Page, wire: string): Promise<Locator> {
  await page.goto(`/graph?rej=${encodeURIComponent(wire)}`);
  const shown = page.locator("section.graph .empty-state .empty");
  await expect(shown).toBeVisible({ timeout: 10_000 });
  return shown;
}

test.describe("Error copy — the rendered sentence is owned, never the wire string", () => {
  test("all fourteen AppError variant prefixes are stripped before the code is read", async ({
    page,
  }) => {
    // Fourteen navigations on one already-warm page; well inside the tripled budget.
    test.slow();
    await boot(page);

    for (const tag of VARIANT_TAGS) {
      const shown = await renderedFor(
        page,
        `${tag}: [folder-locked] ${INTERNAL}`,
      );
      // The code was found, which is only possible if the prefix was removed first.
      await expect(shown, `variant prefix "${tag}:" must be stripped`).toHaveText(
        COPY.folderLocked,
      );
      // …and the prefix itself never reached the screen either.
      //
      // Checked as a PREFIX, not as a substring. Several variant tags are ordinary English
      // words — `locked` is one — and the owned replacement legitimately uses them: "That
      // folder is locked — unlock it first." A `not.toContain("locked")` assertion therefore
      // failed on CORRECT copy, which is a defect in the assertion, not in the stripping.
      // What the contract actually promises is that no wire-format prefix survives, so that
      // is what this asserts.
      const text = (await shown.textContent()) ?? "";
      expect(text.startsWith(`${tag}:`), `"${tag}:" must not survive as a prefix`).toBe(
        false,
      );
      expect(text).not.toContain("[folder-locked]");
      expect(text).not.toContain("HKDF");
      expect(text).not.toContain("mutex");
      expect(text).not.toContain("/Users/");
    }
  });

  test("deny-by-default: an un-coded internal-looking failure renders the generic sentence", async ({
    page,
  }) => {
    await boot(page);

    // A tagged AppError with NO code — the ~2100 constructions that never opted in.
    let shown = await renderedFor(page, `storage error: ${INTERNAL}`);
    await expect(shown).toHaveText(COPY.generic);

    // A transparent `Other(anyhow)` — no variant tag at all, so nothing to strip.
    shown = await renderedFor(page, "brain sidecar stdin missing: broken pipe");
    await expect(shown).toHaveText(COPY.generic);

    // A frontend-thrown `Error`, which stringifies with an "Error: " wrapper.
    shown = await renderedFor(page, "Error: account-session mutex poisoned");
    await expect(shown).toHaveText(COPY.generic);

    // An UNKNOWN code is not trusted just because it is shaped like one. This is the half that
    // makes the allowlist real: if the Rust side ever emits a code the frontend map does not
    // carry, the user gets the generic sentence, never the developer prose behind it.
    shown = await renderedFor(
      page,
      "storage error: [totally-made-up] E2EE decrypt/authentication failed",
    );
    await expect(shown).toHaveText(COPY.generic);
    expect(await shown.textContent()).not.toContain("E2EE");
  });

  test("a linked-note filing refusal renders an actionable restriction", async ({
    page,
  }) => {
    await boot(page);

    const shown = await renderedFor(
      page,
      `provider unavailable: [recording-linked-note] ${INTERNAL}`,
    );
    await expect(shown).toHaveText(COPY.recordingLinkedNote);
    expect(await shown.textContent()).not.toContain("provider unavailable");
    expect(await shown.textContent()).not.toContain("HKDF");
  });

  test("the document-import ladder is decided by the code, never by the prose", async ({
    page,
  }) => {
    test.slow();
    await boot(page);

    // THE ORDERING THAT WAS LOAD-BEARING. `friendlyImportError` tested `no text found` before
    // `unsupported document type` because a scanned PDF's failure message could satisfy the
    // second test too, and losing that order told the user their PDF was an unsupported file
    // type. Here the prose contains BOTH phrases and the code is the only thing consulted, so
    // the ordering is not merely preserved — it is structurally impossible to get wrong.
    let shown = await renderedFor(
      page,
      "invalid argument: [doc-no-text] no text found; unsupported document type fallback",
    );
    await expect(shown).toHaveText(COPY.docNoText);

    // The mirror image: unsupported-type prose that also mentions text still classifies by code.
    shown = await renderedFor(
      page,
      "invalid argument: [doc-unsupported] unsupported document type; no text found either",
    );
    await expect(shown).toHaveText(COPY.docUnsupported);

    // The remaining rungs the old ladder had, each still its own sentence rather than collapsing
    // into the generic "unsupported type" catch-all the ladder's last arm used to swallow them
    // with. Each is an `invalid argument` in Rust, so without the codes they would be
    // indistinguishable.
    shown = await renderedFor(
      page,
      "invalid argument: [doc-too-large] possible zip bomb: 512 MiB expanded",
    );
    await expect(shown).toHaveText(COPY.docTooLarge);

    shown = await renderedFor(
      page,
      "invalid argument: [doc-password] pdf is password-protected",
    );
    await expect(shown).toHaveText(COPY.docPassword);

    shown = await renderedFor(
      page,
      "invalid argument: [doc-unreadable] malformed docx: central directory not found",
    );
    await expect(shown).toHaveText(COPY.docUnreadable);

    // And the lock arm, which the old ladder tested FIRST with a bare `/\block/i` — a substring
    // test that also matched "blocked", "unlocked" and "block size".
    shown = await renderedFor(
      page,
      "locked: [folder-locked] folder f1 is sealed this session",
    );
    await expect(shown).toHaveText(COPY.folderLocked);
  });
});
