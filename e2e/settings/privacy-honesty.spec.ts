import { test, expect, type Locator } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * THE ANTI-SIMPLIFICATION GATE.
 *
 * P3 rewrites Murmur's privacy copy to be plainer. The failure mode that rewrite invites is the
 * one this app can least afford: "emails, card numbers and phone numbers are removed — people's
 * NAMES are not" compresses beautifully into "your data is protected", which is shorter, calmer,
 * and a lie. Murmur's differentiator is telling the truth about what leaves the Mac; a de-jargoning
 * pass that quietly deletes a noun phrase destroys exactly that.
 *
 * So this spec pins FIFTEEN protected clauses BY FACT, never by exact sentence. Each assertion
 * names the *fact* that must survive (e.g. the Privacy section must contain "emails" AND "card"
 * AND "phone" AND an explicit negative about names) and says nothing about the wording around it.
 * That is deliberate: rewording is allowed and expected; dropping the fact is not.
 *
 * The implementer rule these encode: **a rewrite of a protected clause that is shorter AND drops a
 * noun phrase is wrong.**
 *
 * If one of these fails, do not change the assertion to match the new copy. Put the fact back.
 */

/** The Privacy & Integrations section's card, after navigating there. */
async function openPrivacySection(page: import("@playwright/test").Page): Promise<Locator> {
  await mockTauri(page, {
    // Name masking OFF — the DEFAULT, and the state whose honest negatives matter most.
    ner_model_present: () => false,
    get_mcp_config: () =>
      '{"mcpServers":{"murmur":{"url":"http://127.0.0.1:8765/mcp","headers":{"Authorization":"Bearer test-token"}}}}',
  });
  await page.goto("/settings");
  await page.getByText("Privacy & Integrations").first().click();
  const card = page.locator("app-settings-privacy-section .privacy-card");
  await expect(card).toBeVisible({ timeout: 10_000 });
  return card;
}

test.describe("Privacy copy — the clauses a simplification would destroy", () => {
  test("Privacy section: what is removed is ENUMERATED, and what is NOT removed is stated", async ({
    page,
  }) => {
    const card = await openPrivacySection(page);
    const text = (await card.innerText()).replace(/\s+/g, " ");

    // (1) The redaction promise is an enumeration, never a bare noun like "PII" or "your data".
    expect(text, "must name emails").toMatch(/emails/i);
    expect(text, "must name card numbers").toMatch(/card/i);
    expect(text, "must name phone numbers").toMatch(/phone/i);

    // (4) THE most destroyable sentence in the app: the explicit NEGATIVE about names. With the
    // optional model absent, names DO leave with the transcript, and the user must be told.
    expect(text, "must state that names are NOT removed").toMatch(
      /names?\s+(are|is)\s+not|not\s+removed|NAMES are not/i,
    );
    expect(text).toMatch(/name/i);

    // (5) …and that the protection is an OPTIONAL download, in named languages — not something
    // that is quietly already on.
    expect(text, "must say name masking needs a download").toMatch(
      /download/i,
    );
    expect(text, "must name the languages it covers").toMatch(/polish/i);
    expect(text).toMatch(/english/i);
  });

  test("Privacy section: every cloud destination is NAMED, including the ones users assume are local", async ({
    page,
  }) => {
    const card = await openPrivacySection(page);
    const text = (await card.innerText()).replace(/\s+/g, " ");

    // (2) Claude Code is not a local tool: the `claude` CLI uploads the transcript to Anthropic.
    // Users routinely believe otherwise, which is why this clause exists.
    expect(text, "must name Claude Code").toMatch(/claude code/i);
    expect(text, "must name Anthropic as the destination").toMatch(/anthropic/i);

    // (3) A REMOTE Ollama server counts as cloud — the single most-assumed-local case.
    expect(text, "must name Ollama").toMatch(/ollama/i);
    expect(text, "must distinguish a REMOTE Ollama from a local one").toMatch(
      /remote ollama/i,
    );
    expect(
      text,
      "must say only Ollama running on THIS Mac keeps everything here",
    ).toMatch(/ollama running on this mac/i);

    // …and the gateway, which is a third-party hop.
    expect(text).toMatch(/gateway/i);
  });

  test("Privacy section: the consent gate is FAIL-CLOSED, and the download sends nothing", async ({
    page,
  }) => {
    const card = await openPrivacySection(page);
    const text = (await card.innerText()).replace(/\s+/g, " ");

    // (7) The promise that makes the Allow button meaningful: nothing runs until you allow it.
    expect(
      text,
      "must promise cloud summaries do not run before consent",
    ).toMatch(/won'?t run|will not run|turned off/i);

    // (6) The name-masking download is itself not an egress of content.
    expect(text, "must say the model download sends no meeting content").toMatch(
      /sends no meeting content|no meeting content is sent/i,
    );
  });

  test("Privacy section: locked folders LEAVE the vault, and the rest stays readable", async ({
    page,
  }) => {
    const card = await openPrivacySection(page);
    const text = (await card.innerText()).replace(/\s+/g, " ");

    // (8) Both halves matter. A locked folder is encrypted AND removed from the vault (Obsidian
    // stops seeing it — a real, surprising consequence); everything else stays a plain file the
    // user owns. "Obsidian vault" and "Markdown" are the RIGHT words here and are NOT jargon.
    expect(text, "must say locked content is encrypted").toMatch(/encrypt/i);
    expect(text, "must say it leaves the Obsidian vault").toMatch(/obsidian/i);
    expect(text, "must say unlocked content stays plain files").toMatch(
      /markdown|\.md/i,
    );
  });

  test("Privacy section: the local Claude server is read-only, local, and key-protected", async ({
    page,
  }) => {
    const card = await openPrivacySection(page);
    const text = (await card.innerText()).replace(/\s+/g, " ");

    // (9) It exposes your meetings to another app. Read-only and local-only are the two facts
    // that make that acceptable; neither may be dropped for brevity.
    expect(text, "must say the server only reads").toMatch(/read/i);
    expect(text, "must say it is confined to this Mac").toMatch(/this mac/i);

    // (10) The pasted config carries a credential, and the consequence is spelled out.
    expect(text, "must warn the config carries a private key").toMatch(
      /private key|access token/i,
    );
    expect(
      text,
      "must state the consequence of leaking it",
    ).toMatch(/read your meetings|keep it to yourself/i);
  });

  test("Privacy section: cross-meeting memory is local, lock-gated, and forgettable", async ({
    page,
  }) => {
    const card = await openPrivacySection(page);
    const text = (await card.innerText()).replace(/\s+/g, " ");

    // (11) Three facts, all load-bearing: it stays here, the lock still governs it, and it can be
    // cleared. A rewrite that keeps only "stored locally" has removed the user's exit.
    expect(text, "memory must be described as stored on this Mac").toMatch(
      /locally|on this mac/i,
    );
    expect(text, "memory must be described as lock-gated").toMatch(/locked/i);
    expect(text, "memory must be described as clearable").toMatch(
      /clear|forget/i,
    );
  });

  test("AI & Models privacy strip: the on-device list and the names caveat both survive", async ({
    page,
  }) => {
    // The demo mock's DEFAULT config is `providerId: "claude_code"` — a cloud provider — so the
    // "leaves this Mac" line, and therefore the names caveat, renders without overriding
    // `get_config` (which would drop every other field the Settings page reads).
    await mockTauri(page, { ner_model_present: () => false });
    await page.goto("/settings");
    await page.getByText("AI & Models").first().click();
    const strip = page.locator("app-ai-privacy-strip");
    await expect(strip).toBeVisible({ timeout: 10_000 });
    const text = (await strip.innerText()).replace(/\s+/g, " ");

    // (12) The on-device list is a promise per item. Collapsing it to "most things stay local"
    // would be shorter and would say nothing.
    expect(text, "must claim transcription is on-device").toMatch(
      /transcription/i,
    );
    expect(text, "must claim the search index is on-device").toMatch(
      /search index/i,
    );
    expect(text, "must claim name masking is on-device").toMatch(
      /name masking/i,
    );

    // (13) When text DOES leave and masking is off, the strip repeats the negative rather than
    // relying on the user having read the Privacy section.
    expect(text, "must state names are not removed").toMatch(
      /names? (are|is) not|not removed/i,
    );
    expect(text, "must still enumerate what IS removed").toMatch(/email/i);
    expect(text).toMatch(/phone/i);
    expect(text).toMatch(/card/i);
  });

  test("Ledger: the token unit is glossed and the redaction count says what it excludes", async ({
    page,
  }) => {
    await mockTauri(page, {}, {
      get_egress_ledger: {
        totalCalls: 3,
        totalTokens: 4210,
        byModel: [{ model: "claude-sonnet-5", tokens: 4210 }],
        byDay: [{ day: "2026-07-27", tokens: 4210 }],
        // Name masking off ⇒ zero names masked, which must NOT read as "there were no names".
        totalRedactions: { email: 2, card: 1, phone: 4, name: 0 },
        recent: [],
      },
    });
    await page.goto("/analytics");
    const ledger = page.locator("app-egress-ledger");
    await expect(ledger).toBeVisible({ timeout: 10_000 });
    const text = (await ledger.innerText()).replace(/\s+/g, " ");

    // (14) A "token" means nothing to a person. The unit is NOT renamed (the number really does
    // count tokens) — it is glossed, which is the honest half of that trade.
    expect(text, "the token unit must carry a gloss").toMatch(
      /word/i,
    );

    // (15) The redaction tile enumerates, and — with zero names masked — says WHY that zero is
    // there. A bare "0" beside "PII scrubbed" would read as "nothing sensitive was in your text".
    expect(text, "must enumerate emails").toMatch(/email/i);
    expect(text, "must enumerate phones").toMatch(/phone/i);
    expect(text, "must enumerate cards").toMatch(/card/i);
    expect(
      text,
      "must explain that names are not removed unless masking is on",
    ).toMatch(/names are not removed unless/i);

    // And the never-show vocabulary is genuinely gone from the rendered surface.
    expect(text, "must not render the word 'egress'").not.toMatch(/egress/i);
    expect(text, "must not render the acronym 'PII'").not.toMatch(/\bPII\b/);
  });
});
