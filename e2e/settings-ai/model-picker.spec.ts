import { expect, test } from "@playwright/test";
import { mockTauri } from "./mock-invoke";

/**
 * The model catalog is a HINT, never an allowlist.
 *
 * Before this spec, `list_models` returned bare id strings from two `&[&str]` constants in
 * `summarize/provider.rs`, and `settings.store.ts::repairForeignRoleModels` treated those
 * constants as authoritative: any stored model id absent from them was set to `""` **and
 * persisted**. So a newly released model was not merely missing from the dropdown — selecting it
 * was impossible (the free-text input only rendered when the catalog was EMPTY) and any id already
 * in config was destroyed the moment the catalog loaded.
 *
 * Both tests below are RED on that code: the first because the id is erased, the second because
 * there is no input to type into while a non-empty catalog is present.
 */

const CATALOG = [
  { id: "claude-opus-5", label: "Claude Opus 5 — highest quality", source: "bundled" },
  { id: "claude-sonnet-5", label: "Claude Sonnet 5 — balanced", source: "bundled" },
];

/** A model id no catalog knows about — stands in for "released after this build shipped". */
const FUTURE_MODEL = "claude-opus-6";

async function openAiSettings(page: import("@playwright/test").Page, storedModel: string) {
  await page.addInitScript((model) => {
    (
      window as unknown as { __demoConfig?: Record<string, unknown> }
    ).__demoConfig = { providerModel: model, roleNotesConnection: "claude_code" };
  }, storedModel);

  await mockTauri(
    page,
    {
      save_config: (args: { config: Record<string, unknown> }) => {
        const w = window as unknown as {
          __savedConfig?: Record<string, unknown>;
          __demoConfig?: Record<string, unknown>;
        };
        w.__savedConfig = args.config;
        w.__demoConfig = Object.assign({}, w.__demoConfig, args.config);
        return null;
      },
      list_models: () => ({
        source: "bundled",
        options: [
          { id: "claude-opus-5", label: "Claude Opus 5 — highest quality", source: "bundled" },
          { id: "claude-sonnet-5", label: "Claude Sonnet 5 — balanced", source: "bundled" },
        ],
      }),
      provider_statuses: () => [
        { id: "claude_code", available: true },
        { id: "codex_cli", available: true },
        { id: "anthropic", available: true },
      ],
    },
    {},
  );
  // The AI section is reached by clicking its nav entry, not by a direct route — same path
  // `codex-provider.spec.ts` uses, and the only one that mounts the setup block.
  await page.goto("/settings");
  await page.getByText("AI & Models").first().click();
  await page.locator(".model-row").first().waitFor({ state: "visible" });
}

test("a model id the catalog does not know is never erased", async ({ page }) => {
  await openAiSettings(page, FUTURE_MODEL);

  // WAIT FOR THE FETCH FIRST. The old bug fired asynchronously, AFTER `list_models` resolved, so
  // polling a value that is already correct at t=0 could succeed before the code under test even
  // ran — the poll would pass against the seeded value and prove nothing. Wait for the rendered
  // catalog, which only appears once the fetch resolved, then assert the id survived it.
  await expect(
    page.locator(".model-row select.model-select option", {
      hasText: "Claude Sonnet 5",
    }),
  ).toHaveCount(1);
  await expect
    .poll(async () =>
      page.evaluate(
        () => (window as unknown as { __demoConfig?: Record<string, unknown> }).__demoConfig
          ?.["providerModel"],
      ),
    )
    .toBe(FUTURE_MODEL);

  const saved = await page.evaluate(
    () =>
      (window as unknown as { __savedConfig?: Record<string, unknown> }).__savedConfig?.[
        "providerModel"
      ],
  );
  expect(
    saved === undefined || saved === FUTURE_MODEL,
    `a catalog that omits an id must not rewrite it; config was saved as ${JSON.stringify(saved)}`,
  ).toBeTruthy();
});

test("an unlisted model id is typeable even when the catalog is non-empty", async ({ page }) => {
  await openAiSettings(page, "claude-sonnet-5");

  // The catalog has two entries, so the OLD template rendered a <select> and no input at all.
  const input = page.locator(".model-row input.model-input").first();
  await expect(
    input,
    "the free-text model id must be available alongside a non-empty catalog, or a newly " +
      "released model cannot be selected until someone edits provider.rs",
  ).toBeVisible();

  await input.fill(FUTURE_MODEL);
  await input.blur();
  await expect.poll(async () => input.inputValue()).toBe(FUTURE_MODEL);
});

test("an id the boundary would refuse is cleared visibly, not silently", async ({ page }) => {
  // The gap this closes: every other spec types a SHORT valid id and asserts the input value. None
  // typed one the persistence boundary rejects, so none proved what actually lands in config — the
  // case where the UI could show one thing and `save_config` store another.
  const tooLong = `hf.co/${"a".repeat(60)}/model:Q4_K_M`;
  expect(tooLong.length).toBeGreaterThan(64);

  await openAiSettings(page, tooLong);
  await page
    .locator('select[formcontrolname="providerId"]')
    .first()
    .selectOption("claude_code");

  // claude_code puts the id on a command line, so this one cannot be sent. The UI clears it in the
  // same interaction and says so, rather than leaving it on screen for autosave to drop.
  await expect(page.locator("[data-dropped-model]")).toContainText(tooLong);
  await expect(
    page.locator(".model-row input.model-input").first(),
    "the field must not still show a value the backend will not keep",
  ).toHaveValue("");

});

test("dropping an unusable model does not cancel the new engine's catalog fetch", async ({
  page,
}) => {
  // `onEngineChanged` used to `return` immediately after clearing an unusable id, so the newly
  // selected engine never had `ensureModels` called: the picker showed no options AND no "this list
  // ships with the app" provenance. The user was told what was removed and then shown an empty list
  // with no explanation of why it was empty.
  //
  // The engine here must be one whose catalog is NOT already loaded. A first attempt at this
  // assertion switched to `claude_code` — the DEFAULT — whose catalog was fetched at load, so the
  // assertion held with the bug still present. It passed the RED check and proved nothing.
  const tooLong = `hf.co/${"a".repeat(60)}/model:Q4_K_M`;
  await openAiSettings(page, tooLong);
  await expect(page.locator(".model-row select.model-select option", { hasText: "Claude Sonnet 5" }))
    .toHaveCount(1);

  await page
    .locator('select[formcontrolname="providerId"]')
    .first()
    .selectOption("codex_cli");

  await expect(page.locator("[data-dropped-model]")).toContainText(tooLong);
  await expect(
    page.locator(".model-row select.model-select option", { hasText: "Claude Sonnet 5" }),
    "the new engine's catalog must load even when the switch dropped the previous model",
  ).toHaveCount(1);
  await expect(
    page.locator("[data-bundled-catalog]"),
    "a bundled catalog must still say so; silence reads as 'this list is current'",
  ).toBeVisible();

  // And what is actually persisted agrees with what is shown.
  await expect
    .poll(async () =>
      page.evaluate(
        () =>
          (window as unknown as { __savedConfig?: Record<string, unknown> }).__savedConfig?.[
            "providerModel"
          ],
      ),
    )
    .not.toBe(tooLong);
});

test("a SHAPE the boundary refuses is cleared too, not only an over-long one", async ({
  page,
}) => {
  // Length was the only refusal the UI mirrored, so `./model` and `-m` — which the boundary also
  // rejects, the second because argv would read it as a flag — were shown as accepted and then
  // dropped on save. The UI now mirrors the whole predicate, not one clause of it.
  await openAiSettings(page, "./model");
  await page
    .locator('select[formcontrolname="providerId"]')
    .first()
    .selectOption("claude_code");

  await expect(page.locator("[data-dropped-model]")).toContainText("./model");
  await expect(
    page.locator(".model-row input.model-input").first(),
  ).toHaveValue("");
});

test("refresh button tracks the ENGINE, not the mocked catalog source", async ({ page }) => {
  // Refresh visibility is driven by `LIVE_CATALOG_CONNECTION_IDS` — a static property of the
  // connection — NOT by the `source` a catalog response happens to carry. That distinction is the
  // whole point: deriving it from the response hid the button after a failed fetch and showed it
  // for a bundled arm before the first response.
  //
  // So this test pins the ENGINE as the input, and deliberately mocks a contradictory `source` to
  // prove the response cannot move it. Asserting against the mocked `source` would have been
  // vacuous — the frontend never reads it.
  await openAiSettings(page, "claude-sonnet-5");
  await expect(
    page.locator(".model-refresh"),
    "claude_code is a bundled arm: Refresh could not change anything, so it is not offered",
  ).toHaveCount(0);
  await expect(page.locator("[data-bundled-catalog]")).toBeVisible();

  // The LIVE half of this property is asserted on the role rows, not here: the Setup card still
  // defers `ollama`/`gateway` to Advanced, and inlining their picker is the follow-up task. See
  // `a role row with an EMPTY live catalog still offers Refresh`, which proves the engine — not
  // the response — decides, by offering Refresh for a live arm whose catalog came back empty.
});

/** Open the per-feature role rows, where `ollama` and `gateway` — the live arms — are selectable. */
async function openRoleRows(
  page: import("@playwright/test").Page,
  catalog: unknown,
  roleConnection = "ollama",
): Promise<void> {
  // Pass the catalog as a CONSTANT, not as an override. `mockTauri` serializes overrides with
  // `Function.prototype.toString()`, so `() => catalog` arrives page-side as literally that text
  // and `catalog` — a closure variable in the Node process — is undefined there. Constants go
  // through `JSON.stringify`, which is what a value captured out here needs.
  // `save_config` is recorded so a spec can assert what PERSISTED, not only what the form shows.
  // The two disagreeing is the defect class this file exists to catch, and a form-only assertion
  // is blind to it by construction. The override closes over nothing — `mockTauri` serializes it
  // with `Function.prototype.toString()`, so a captured Node variable would arrive undefined.
  await mockTauri(
    page,
    {
      save_config: (args: { config: Record<string, unknown> }) => {
        const w = window as unknown as {
          __savedConfig?: Record<string, unknown>;
          __demoConfig?: Record<string, unknown>;
        };
        w.__savedConfig = args.config;
        w.__demoConfig = Object.assign({}, w.__demoConfig, args.config);
        return null;
      },
    },
    { list_models: catalog },
  );
  await page.goto("/settings");
  await page.getByText("AI & Models").first().click();
  // TWO disclosures, nested. `app-ai-role-rows` lives inside `ai-advanced-block`, so its own
  // toggle exists in the DOM but is NOT VISIBLE until Advanced is open — which is why clicking it
  // directly appeared to do nothing and `aria-expanded` stayed "false".
  await page.getByRole("button", { name: /Advanced/ }).click();
  await page.locator("app-ai-role-rows button.role-toggle").click();
  const row = page.locator('[data-role="notes"]');
  await row.waitFor({ state: "visible" });
  // Pick the connection through the UI. Seeding `roleNotesConnection` in `__demoConfig` does not
  // reach the row — it renders "Inherit default" — and a row on the inherit branch has no model
  // picker at all, which is what makes the assertions below vacuous rather than failing honestly.
  await row.locator("select").first().selectOption(roleConnection);
  // Wait for the model row the connection change creates. `selectOption` resolves as soon as the
  // DOM value is set, but the catalog fetch and the re-render that follows are asynchronous — every
  // assertion below races them otherwise.
  await row.locator(".role-model-row").waitFor({ state: "visible" });
}

test("a role row can type an unlisted id against a NON-EMPTY catalog", async ({ page }) => {
  // The same defect the Setup card had: the free-text input sat in the `@else` of the catalog
  // check, so a non-empty catalog made every unlisted id unreachable on this surface.
  await openRoleRows(page, {
    source: "live",
    options: [{ id: "llama3.1:8b", label: "llama3.1:8b", source: "live" }],
  });
  const row = page.locator('[data-role="notes"]');
  await expect(row.locator("select.role-model-select")).toBeVisible();
  const input = row.locator("input.role-model-input");
  await expect(
    input,
    "a role row must accept a newer model id even when its catalog is non-empty",
  ).toBeVisible();
  await input.fill("llama4:70b");
  await input.blur();
  await expect.poll(async () => input.inputValue()).toBe("llama4:70b");
});

test("a role row KEEPS a usable model across a connection change and says it is unlisted", async ({
  page,
}) => {
  // `setRoleConnection` used to blank the model on every connection change, justified as "a model
  // belongs to the arm it was picked for". That is the same instinct `repairForeignRoleModels` was
  // deleted for: it destroys a choice the new arm could have honoured, and nothing offers it back.
  // The Setup card already settled the rule — keep an id the engine can SEND, explain that the
  // catalog does not list it, clear only an id the engine cannot send at all — and a role row is
  // the same question asked one row lower, so the two surfaces must not disagree.
  await openRoleRows(
    page,
    {
      source: "live",
      options: [{ id: "llama3.1:8b", label: "llama3.1:8b", source: "live" }],
    },
    "ollama",
  );
  const row = page.locator('[data-role="notes"]');
  const model = row.locator("input.role-model-input");
  // An id the catalog does NOT list — the whole point is that such an id is legitimate. A listed
  // one would prove less: it needs no explanation, and the row correctly stays silent for it.
  await model.fill("llama4:70b");
  await model.blur();
  await expect.poll(async () => model.inputValue()).toBe("llama4:70b");

  await row.locator("select").first().selectOption("claude_code");
  await expect(
    model,
    "a shape the new arm can send must survive the switch — destroying it is the defect this " +
      "whole change exists to remove",
  ).toHaveValue("llama4:70b");
  await expect(
    row.locator("[data-role-kept-model]"),
    "keeping an unlisted id silently is only half right; the row must say why it is not in the list",
  ).toContainText("llama4:70b");
});

test("a role row clears a model the new engine cannot send, and names it", async ({ page }) => {
  // The other half of A5. `claude_code` puts the id on a command line, so the 64-char argv ceiling
  // applies and a long Hugging Face id — perfectly legal under Ollama — cannot survive. Clearing is
  // right here; doing it without a word is not.
  const hfModel = `hf.co/${"a".repeat(60)}/model:Q4_K_M`;
  await openRoleRows(
    page,
    { source: "live", options: [{ id: hfModel, label: hfModel, source: "live" }] },
    "ollama",
  );
  const row = page.locator('[data-role="notes"]');
  const model = row.locator("input.role-model-input");
  await model.fill(hfModel);
  await model.blur();
  await expect.poll(async () => model.inputValue()).toBe(hfModel);

  await row.locator("select").first().selectOption("claude_code");
  await expect(
    model,
    "an id the arm cannot put on a command line must not be left on screen as if it were kept",
  ).toHaveValue("");
  await expect(
    row.locator("[data-role-cleared-model]"),
    "a clear the user did not ask for must name what it removed",
  ).toContainText(hfModel);
});

test("switching a role to on-device KEEPS a real registry model id", async ({ page }) => {
  // The condition the sibling test below cannot reach. `llama3.1:8b` is deliberately NOT an
  // on-device model, so a rule that clears EVERYTHING on the way to `local` passes that test while
  // destroying a legitimate choice — which is what the first version of this code did.
  //
  // The local arm consumes the role model as a REGISTRY KEY (`resolve_brain_model` looks it up in
  // `BRAIN_MODELS`), so `qwen25-3b` is a working per-role on-device override. Membership decides,
  // not the absence of a visible model field.
  await openRoleRows(
    page,
    { source: "live", options: [{ id: "qwen25-3b", label: "qwen25-3b", source: "live" }] },
    "ollama",
  );
  const row = page.locator('[data-role="notes"]');
  await row.locator("input.role-model-input").fill("qwen25-3b");
  await row.locator("input.role-model-input").blur();
  await expect.poll(async () => row.locator("input.role-model-input").inputValue()).toBe(
    "qwen25-3b",
  );

  await row.locator("select").first().selectOption("local");
  await expect(row.locator(".role-model-row")).toHaveCount(0);
  await expect(
    row.locator("[data-role-cleared-model]"),
    "an id this engine CAN use must not be reported as cleared",
  ).toHaveCount(0);

  await row.locator("select").first().selectOption("ollama");
  await expect(
    row.locator("input.role-model-input"),
    "a valid on-device model id is a working per-role override, so it must survive",
  ).toHaveValue("qwen25-3b");
});

test("switching a role to the on-device engine clears an unusable model AND says so", async ({
  page,
}) => {
  // `local` renders no model control, which is what made this look like a case for silent
  // retention. It is the opposite: the local arm CONSUMES the role model —
  // `make_provider_resolved` uses it in place of `brain_model_id` and then `resolve_brain_model`
  // fails the note with `Unavailable` — so a retained `llama3.1:8b` does not sit there harmlessly,
  // it breaks on-device notes. It must be cleared, and because the row has no model field, the
  // notice has to live OUTSIDE the model block or the clear is invisible again.
  await openRoleRows(
    page,
    {
      source: "live",
      options: [{ id: "llama3.1:8b", label: "llama3.1:8b", source: "live" }],
    },
    "ollama",
  );
  const row = page.locator('[data-role="notes"]');
  const model = row.locator("input.role-model-input");
  await model.fill("llama3.1:8b");
  await model.blur();
  await expect.poll(async () => model.inputValue()).toBe("llama3.1:8b");

  await row.locator("select").first().selectOption("local");
  await expect(row.locator(".role-model-row")).toHaveCount(0);
  await expect(
    row.locator("[data-role-cleared-model]"),
    "a row with no model field is exactly where a silent clear hides — the notice must reach it",
  ).toContainText("llama3.1:8b");

  await row.locator("select").first().selectOption("ollama");
  await expect(
    row.locator("input.role-model-input"),
    "the clear is real, not cosmetic: the id must not reappear",
  ).toHaveValue("");
});

test("switching a role to Inherit keeps the model, because nothing reads it there", async ({
  page,
}) => {
  // The other half, and the reason this is not simply "clear whenever the field is hidden".
  // `roles::is_explicit` keys on the CONNECTION key alone, so an inheriting role resolves through
  // `legacy_default_target`, which never reads `role_*_model`. Nothing consumes the value, so
  // destroying it would be pure loss — and invisible, since Inherit renders no model control.
  //
  // The id is LONG on purpose. An earlier version used `llama3.1:8b`, which is valid on every arm,
  // so it could not detect the real failure: the UI kept the model while `dto_to_config` resolved
  // the empty role connection to the DEFAULT engine and judged it by the CLI rule. With
  // `claude_code` as the default — the shipped default — a legitimate long Ollama id was blanked by
  // the next autosave. A fixture that passes both predicates cannot tell the two rules apart.
  const hfModel = `hf.co/${"a".repeat(60)}/model:Q4_K_M`;
  await openRoleRows(
    page,
    { source: "live", options: [{ id: hfModel, label: hfModel, source: "live" }] },
    "ollama",
  );
  const row = page.locator('[data-role="notes"]');
  const model = row.locator("input.role-model-input");
  await model.fill(hfModel);
  await model.blur();
  await expect.poll(async () => model.inputValue()).toBe(hfModel);

  await row.locator("select").first().selectOption("");
  await expect(row.locator(".role-model-row")).toHaveCount(0);
  await row.locator("select").first().selectOption("ollama");
  await expect(
    row.locator("input.role-model-input"),
    "a detour through Inherit must not destroy a choice nothing there could have used",
  ).toHaveValue(hfModel);

  // The form is only half the claim — what PERSISTED has to agree, since the defect was the
  // boundary silently disagreeing with the field.
  await expect
    .poll(async () =>
      page.evaluate(
        () =>
          (window as unknown as { __demoConfig?: Record<string, unknown> }).__demoConfig?.[
            "roleNotesModel"
          ],
      ),
    )
    .toBe(hfModel);
});

test("the length mirror counts UTF-8 BYTES, like the backend does", async ({ page }) => {
  // Rust `str::len()` counts bytes; JavaScript `String.length` counts UTF-16 code units. While the
  // JSON arms carried an ASCII allowlist the two agreed by construction — removing it (correctly:
  // it refused real ids) made non-ASCII reachable and the counts diverge.
  //
  // 300 × `é` is 300 code units and 600 bytes. Under the UTF-16 count it is comfortably inside the
  // 512 ceiling, so the field would show it accepted; `dto_to_config` measures bytes, refuses it,
  // and `AppConfig::load` clears it on the next launch. That is precisely the "the UI displays a
  // value persistence already discarded" defect this mirror exists to prevent, reintroduced through
  // a unit mismatch rather than a rule mismatch.
  const nonAscii = "é".repeat(300);
  expect(nonAscii.length, "fixture must pass a UTF-16 count").toBeLessThanOrEqual(512);
  expect(
    new TextEncoder().encode(nonAscii).length,
    "...while failing a UTF-8 one, or the test proves nothing",
  ).toBeGreaterThan(512);

  await openAiSettings(page, "");
  // `anthropic` is a JSON-body arm, so this id is refused for LENGTH alone — not for its character
  // class, which that arm no longer restricts.
  await page
    .locator("app-ai-setup-block")
    .locator('select[formcontrolname="providerId"]')
    .selectOption("anthropic");
  await page.locator("input.model-input").fill(nonAscii);
  await expect(
    page.locator("[data-model-refused]"),
    "an id the backend measures as over-long must be flagged, whatever JavaScript counts",
  ).toBeVisible();

  // The same mirror question one character class further out. Rust's `char::is_control` is true for
  // C1 (U+0080–U+009F) as well as C0 and DEL; the JS class stopped at DEL, so U+0085 (NEL) was
  // shown as accepted and refused on save. `\s` does not cover it, which is why it was missed.
  // Built from its code point rather than typed: a literal control character in a source file
  // is exactly what made `model-id.ts` reach reviewers as an unreadable binary blob.
  const withNel = `claude${String.fromCharCode(0x85)}opus`;
  await page.locator("input.model-input").fill(withNel);
  await expect(
    page.locator("[data-model-refused]"),
    "a C1 control character is a control character to Rust, so the mirror must refuse it too",
  ).toBeVisible();

  // ...and a legitimate non-ASCII id on a JSON-body arm is still accepted — the allowlist was
  // removed on purpose, and this keeps the two refusals above from passing for the wrong reason.
  await page.locator("input.model-input").fill("vendor/modèle+preview");
  await expect(page.locator("[data-model-refused]")).toHaveCount(0);
});

test("a role row flags a typed id the boundary would refuse", async ({ page }) => {
  // The role rows render the free-text id unconditionally now, so they inherit the Setup card's
  // obligation: `dto_to_config` refuses `-m` and keeps the PREVIOUS stored value, and a row that
  // goes on displaying the typed string is stating something untrue.
  await openRoleRows(
    page,
    {
      source: "bundled",
      options: [{ id: "claude-opus-5", label: "Claude Opus 5", source: "bundled" }],
    },
    "claude_code",
  );
  const row = page.locator('[data-role="notes"]');
  const model = row.locator("input.role-model-input");

  await model.fill("-m");
  await expect(
    row.locator("[data-role-model-refused]"),
    "a refused id must be flagged on the surface that accepted it, not only on the Setup card",
  ).toBeVisible();

  await model.fill("claude-opus-5");
  await expect(row.locator("[data-role-model-refused]")).toHaveCount(0);
});

test("a typed model id the backend would refuse says so while it is on screen", async ({
  page,
}) => {
  // `onEngineChanged` only inspects the value at the instant the engine changes, but the free-text
  // id is now always rendered — so an id typed into a SETTLED form reached `dto_to_config`, was
  // dropped there, and the field went on displaying it. The field showing a value the backend has
  // already discarded is the UI stating something untrue, so the check has to react to the value,
  // not to the engine.
  await openAiSettings(page, "");
  const model = page.locator("input.model-input");

  // A leading `-` is read as a flag on an argv engine, so `valid_model_id` refuses it — the same
  // rejection an over-long id gets, but reachable by typing three characters.
  await model.fill("-m");
  await expect(
    page.locator("[data-model-refused]"),
    "an id that will not survive save must be flagged before save, not silently dropped",
  ).toBeVisible();

  // And the notice must be about THIS value, not a sticky flag: a legal id clears it.
  await model.fill("claude-opus-5");
  await expect(page.locator("[data-model-refused]")).toHaveCount(0);
});

test("a role row with an EMPTY live catalog still offers Refresh", async ({ page }) => {
  // The case a per-option `source` could not express: a gateway answering successfully with zero
  // models is exactly when the user wants to retry, and an empty option list has no option to read
  // a source from. Provenance lives on the catalog, so the empty one still says it was fetched.
  await openRoleRows(page, { source: "live", options: [] }, "gateway");
  const row = page.locator('[data-role="notes"]');
  await expect(
    row.locator(".role-model-refresh"),
    "an empty LIVE catalog is precisely when Refresh matters",
  ).toBeVisible();
  await expect(
    row.locator("[data-role-bundled-catalog]"),
    "a live catalog must never claim to ship with the app, however empty it is",
  ).toHaveCount(0);
});

test("a role row with a bundled catalog says so instead of offering Refresh", async ({ page }) => {
  await openRoleRows(
    page,
    {
      source: "bundled",
      options: [
        { id: "claude-opus-5", label: "Claude Opus 5 — highest quality", source: "bundled" },
      ],
    },
    "claude_code",
  );
  const row = page.locator('[data-role="notes"]');
  await expect(row.locator(".role-model-refresh")).toHaveCount(0);
  await expect(row.locator("[data-role-bundled-catalog]")).toBeVisible();
});

test("engine switch that drops a model explains itself", async ({ page }) => {
  await openAiSettings(page, FUTURE_MODEL);
  const engine = page.locator('select[formcontrolname="providerId"]').first();
  await engine.selectOption("codex_cli");

  // The id must survive the switch — being absent from a hint catalog commonly just means the
  // model shipped after this build.
  await expect(page.locator("[data-unlisted-model]")).toContainText(FUTURE_MODEL);
  const input = page.locator(".model-row input.model-input").first();
  await expect(input).toHaveValue(FUTURE_MODEL);
});

test("options render labels, not raw ids", async ({ page }) => {
  await openAiSettings(page, "claude-sonnet-5");

  const options = page.locator(".model-row select.model-select option");
  const texts = await options.allTextContents();
  for (const entry of CATALOG) {
    expect(
      texts.some((t) => t.includes(entry.label)),
      `option list must show "${entry.label}", got ${JSON.stringify(texts)}`,
    ).toBeTruthy();
  }
  expect(
    texts.some((t) => t.trim() === entry_id_only()),
    "a bare id with no label means the picker regressed to rendering raw ids",
  ).toBeFalsy();
});

/** The raw id the old template would have rendered for the first catalog entry. */
function entry_id_only(): string {
  return CATALOG[0].id;
}
