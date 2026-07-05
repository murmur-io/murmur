<!-- Generated 2026-07-04 via /research (murmur-researcher fan-out: competition / feasibility / architecture-fit / UX-demand). Pricing/funding/version = point-in-time. -->
# Research: Secure + easy sharing of meeting notes in Murmur

## TL;DR / Verdict

**Build it in three phases; the first is small, differentiated, and ships with ZERO cloud egress and ZERO new dependencies.**

Murmur is already ~80% of the way to a great *local* share story — it has gated `.md`/PDF/audio/canvas export and clipboard copy — but it has **no "Share" concept**, and the one thing users reach for most (paste a clean recap into email/Slack) is **actively broken today**: "Copy Markdown" pastes raw YAML frontmatter + literal `[[wikilinks]]`.

- **Phase 1 (ship now, S, zero egress, zero new deps):** reframe the export group as **"Share"**, add **"Copy clean summary"** (strip frontmatter, flatten wikilinks, summary-only by default), keep PDF/Markdown. Fixes the concrete "pastes junk" defect that blocks the #1 real job.
- **Phase 2 (differentiator, M, still zero cloud, zero new deps):** a **password-protected self-decrypting HTML bundle** — Murmur encrypts the note client-side (native WebCrypto AES-GCM + PBKDF2), writes **one standalone `.html`** the user hands over on any channel; the recipient opens it in any browser, types the password (shared out-of-band), and reads it. **No server, no recipient install.** This is the "secure + easy" flagship *no competitor offers as a local artifact*. Optional on-device redaction toggle lives here.
- **Phase 3 (defer, L, opt-in cloud):** a **zero-knowledge relay link** (AES key in URL `#fragment`, server stores only ciphertext) with **expiry / burn-after-read**. This is the only mechanism that needs standing cloud infra; expiry/burn are inherently server-side. Must be **loud, opt-in, self-hostable**, redaction-forced, egress-ledgered.

**Skip an Obsidian-Publish clone** — users who want a hosted site can already point Obsidian Publish at the same vault; rebuilding it fights local-first for no edge. **The market wound to own:** Granola's April-2026 "notes are public-by-default, anyone-with-link, no login" scandal — Murmur's pitch becomes *"a share link even we can't read."*

## Co już mamy (z repo, z file:line)

Sharing is **greenfield** (no `share`/`publish`/`public-link` anywhere), but the export seam it rides on is mature and **every path is already lock-gated**:

- **`export_note`** (`commands.rs:1489`) — writes note markdown to a user-chosen path; **fails closed** on a sealed-not-unlocked meeting via `meeting_is_unlocked` (`commands.rs:1496`, `AppError::Locked`). This is the exact gate any new share command copies.
- **`export_audio` / `export_mic_master` / `export_sys_master` / `export_master`** (`commands.rs:1411/1469/1479/1445`) — gated WAV export; `.enc` at rest, no plaintext to copy while sealed.
- **`export_canvas`** (`commands.rs:3120`) — gated + path-containment `assert_in_vault` (`:3160`).
- **FE export group** in `detail.component.ts` (`<div class="export">` ~`:296`): `copyMarkdown` (`:2771`, copies `note.markdown` **raw**), `saveMarkdown` (`:2793`), `saveAsPdf` (`:2904`, `window.print()` → WKWebView native "Save as PDF", print CSS ~`:1823`), `saveAudio`/`exportMaster`, `exportCanvas`. IPC in `ipc.service.ts:571-627`.
- **Masked DTO** (`masked_detail`, `commands.rs:4369`): sealed meeting → `note: None`, `segments: []`, `audio_path: None`, title `🔒 Locked`. **`copyMarkdown` is already safe** because it reads `detail()?.note?.markdown`, which is `None` when locked — anything consuming `MeetingDetailDto` inherits the gate for free.
- **Crypto primitives already present:** `crypto::encrypt`/`encrypt_file` (AES-256-GCM, verify-before-destroy, `crypto.rs`); `aes-gcm 0.10` in `Cargo.toml`. **`marked ^14.1.4`** already renders markdown in the FE. **`window.print()` PDF works with zero deps.**
- **Redaction firewall + reusable scrubbers:** `RedactingProvider` (`redact.rs:307`) is wired **only** on the LLM-egress path (`summarize/mod.rs`); exports are **not** redacted today. But the standalone `redact::redact(text)` (`redact.rs:183`, email/card/phone) and `active_name_redactor()` (`redact.rs:79`, PERSON→`⟪NAME_n⟫` **only if NER model downloaded**, else no-op) are directly callable by a share path.
- **Provenance / privacy receipt:** `inject_provenance_frontmatter` (`obsidian.rs:501`) + `inject_privacy_receipt_frontmatter` (`obsidian.rs:594`) — content-free honesty keys, idempotent. Code's own caveat: the receipt is "self-declared, **not** cryptographic" (`obsidian.rs:566`) — the gap ed25519 signing would close later.
- **Capability surface is minimal** (`capabilities/default.json`: only `dialog:allow-open/save`) — no fs/shell/clipboard write exposed to the webview, so **every new file write must go through a gated Rust command**. A feature, not a blocker: it keeps writes behind the lock gate.
- **Active precedent to inherit:** current branch is `fix/vault-titles-egress-leak` — titles/wikilinks have been a real egress-leak vector. A share artifact **must not** embed cross-note `[[wikilinks]]` or title-derived data (see truth-audit 2026-07-04, `vault_titles` LEAK).

## Findings (per angle)

### 1. Competition — the market splits into "link = ACL, content on our cloud" vs. a tiny privacy-first minority

- **Granola = the cautionary tale and the strongest signal.** Every note got a **public URL, anyone-with-link, no login**, while marketed "private by default"; default was "Anyone with link"; notes used for training by default. Blew into an Apr-2026 press cycle. PromptArmor added: unauthenticated visitors fall outside the contractual data protections, and shared notes become a prompt-injection/exfil surface. [techbuzz](https://www.techbuzz.ai/articles/granola-s-private-ai-notes-are-public-by-default), [PromptArmor](https://www.promptarmor.com/resources/granola-ai-security-risks-and-remediations), [Granola sharing docs](https://docs.granola.ai/help-center/consent-security-privacy/sharing-controls). *High.*
- **Fireflies** — most granular SaaS: anyone-with-link, per-email/group, **link expiry (24h/7d/14d/30d/none)**, **password links (Business+)**; still fully cloud-hosted. [share](https://guide.fireflies.ai/articles/2474667467-share-meeting-recaps-with-teammates-participants-specific-people-user-groups-and-non-fireflies-users), [public](https://guide.fireflies.ai/articles/2479453517-public-meeting-access-how-to-allow-non-fireflies-users-to-view-shared-meeting-recaps). *High/med.*
- **Fathom / Otter / Notion Publish** — all "anyone-with-link, no login, content on our server" variants; Notion indexing default-off but toggleable. [Fathom](https://help.fathom.video/en/articles/295616), [Otter](https://help.otter.ai/hc/en-us/articles/360048338793-Share-a-conversation), [Notion](https://www.notion.com/help/public-pages-and-web-publishing). *High/med.*
- **The privacy-respecting minority — two patterns to copy:**
  - **Local self-contained files (Obsidian ethos):** export an owned file, hand it over yourself. **Obsidian Publish** ($8/site/mo) is the opt-in hosted upgrade, private-by-default, per-note `publish:false`, password. [Publish](https://obsidian.md/publish). *High.*
  - **Zero-knowledge relay — AES key in the URL `#fragment` (gold standard).** Two Obsidian plugins nail it and are directly transferable: **Share Note / note.sx** (683★, client-side encrypt, key in `#`, server sees only ciphertext, self-hostable — **caveat: attachments stored UNENCRYPTED**) and **QuickShare / Noteshare.space** (AES-256 + HMAC, key never sent, self-host). **Proton** is the commercial exemplar (OpenPGP, password + expiry links). [Share Note](https://github.com/alangrainger/share-note) + [encryption docs](https://docs.note.sx/notes/encryption), [QuickShare](https://github.com/mcndt/obsidian-quickshare) + [design](https://www.mcndt.dev/posts/how-to-e2e-encryption), [Proton](https://proton.me/drive/docs). *High.*
- **The `#fragment` is never sent in HTTP requests or `Referer`** → a Slack/iMessage unfurl bot fetching the URL still gets only ciphertext. Residual exposure = recipient's browser history holding the key (same tradeoff Proton/Bitwarden Send accept). *High.*

### 2. Feasibility — the local tier needs almost no new dependencies

- **PDF: already shipped, zero deps.** `window.print()` → WKWebView → macOS "Save as PDF" (`detail.component.ts:2904`). Improving it is a print-CSS exercise. A deterministic backend PDF (batch/headless) would need a **new crate** — best candidate `markdown2pdf` (MIT, pure-Rust, rustls not OpenSSL) — only if a real requirement appears. *High on crate facts; PDF fidelity in the packaged build "needs a real Mac".*
- **Standalone single-file HTML:** `marked` (already present) → inline CSS + base64 images → a gated `export_note_html` write command. Zero new deps. *High.*
- **Password self-decrypting HTML bundle (the "secure + easy" winner):** embed ciphertext in a standalone HTML page whose built-in JS decrypts in the recipient's browser after a password. **Both encrypt and decrypt use native WebCrypto (`SubtleCrypto`) — AES-GCM + PBKDF2-SHA256 are built into every browser**, so neither Murmur nor the recipient needs a new library. Prior art: [self-decrypting-html-page](https://github.com/derhuerst/self-decrypting-html-page), [hat.sh](https://github.com/sh-dv/hat.sh). PBKDF2 ≥600k iters is the zero-dep KDF (Argon2id is stronger but not in WebCrypto → a new FE dep). *High.*
- **`.age` passphrase bundle** (Rust `age` crate, MIT/Apache) is standard but **requires the recipient to install the `age` CLI** → breaks "easy". Disqualified for MVP. *High.*
- **Zero-knowledge relay (Phase 3):** verified pattern from PrivateBin (AES-GCM + PBKDF2, key in `#`, burn+expiry) and Yopass (key in `#`, Redis/Memcached blob store, auto-delete on read/TTL). Needs a **dumb ciphertext blob store with TTL** (S3/R2 bucket or tiny KV) + thin upload/fetch endpoint. Client crypto is free (WebCrypto). **Expiry/burn-after-read are inherently server-side** — a local file can only *stamp* an advisory "intended expiry", never enforce it. [PrivateBin FAQ](https://github.com/PrivateBin/PrivateBin/wiki/FAQ), [Yopass](https://github.com/jhaals/yopass). *High on architecture; infra/cost is real.*
- **ed25519 signing (defer):** `ed25519-dalek` (BSD-3) or `rsign2` (minisign) would upgrade the honest-but-unverifiable privacy receipt into a checkable attestation, but real-world value is low until there's a published-key/identity story. New crate. *High on crates; "low value now" is a product call.*

### 3. Architecture fit — "share" has two meanings, and that IS the decision

- **(a) Local handoff** (a clean self-contained artifact the user routes themselves) — **not new egress**; it's what `saveMarkdown`/`copyMarkdown` already do, just a better artifact. Passes lock-security almost for free (rides `meeting_is_unlocked`). Phases 1–2.
- **(b) Off-device send** (link/email/upload) — a **new class of egress**; needs its own consent gate + forced redaction + egress-ledger entry. Phase 3.
- **Redaction is reusable but asymmetric:** `redact::redact` catches email/card/phone deterministically; **person names only if the NER model is downloaded** (else no-op). A "Redact before share" toggle must say this honestly — don't promise "PII removed" when NER is absent. *High.*
- **Cross-note wikilink leak is the sharp edge:** a shared artifact can leak titles of *other* (possibly locked) meetings via `[[wikilinks]]`/backlinks — exactly the `fix/vault-titles-egress-leak` class. Any artifact leaving the device must **flatten/strip cross-note wikilinks**. *Med (design risk).*
- **Don't assume `notes.markdown` already carries provenance/receipt** — those are injected on the vault-write path; a `share_note` command should **explicitly** inject them rather than assume. *Med.*

### 4. UX / demand — clean summary to email/Slack is the #1 job; cloud link is not what privacy users want

- **Email recap to attendees is the universal SaaS default; Slack one-click is Granola's flagship** distribution. But **auto-emailing to all participants is itself the privacy complaint** (Otter class action, consent backlash) → the privacy-safe design is **manual, user-initiated, granular** sharing, never auto-send-to-all. [Fireflies recap](https://guide.fireflies.ai/articles/7339665361-how-to-configure-meeting-recap-emails-and-privacy-settings), [Granola Slack](https://docs.granola.ai/help-center/sharing/integrations/slack), [Computerworld](https://www.computerworld.com/article/4041849/enterprise-note-taking-apps-face-legal-scrutiny-as-otter-hit-with-privacy-suit.html). *High.*
- **Raw `.md` sharing is a known failure for non-Obsidian recipients** ("raw text littered with `##`, backticks, asterisks; wikilinks won't render") — this directly indicts Murmur's current "Copy Markdown". [Unmarkdown](https://unmarkdown.com/blog/how-to-share-obsidian-notes). *High.*
- **Granola's own guide states the pattern that maps onto Murmur's values:** external clients → **"Share transcript: No. Share summary: Yes, curated"**, tiered access (edit/view/summary-only), revoke when done. [Granola](https://www.granola.ai/blog/how-to-share-meeting-notes). *High.*
- **Consent UX bar (for any future cloud share) = Proton:** explicit encryption-state indicator (lock icon) + password + **out-of-band key** + default expiry + a **preview of exactly what leaves**. [Proton](https://proton.me/support/password-protected-emails). *High.*
- **Granularity is the single most valuable new behavior:** *summary only* (default) vs *+ transcript*; frontmatter stripped, wikilinks flattened, transcript/audio excluded by default.

## Fit z ograniczeniami Murmur

| Constraint | Phase 1 (clean summary/PDF) | Phase 2 (encrypted HTML bundle) | Phase 3 (relay link) |
|---|---|---|---|
| **Local-first / no egress** | ✅ zero egress | ✅ zero egress (file only) | ⚠️ NEW egress — loud/opt-in/self-host |
| **Obsidian-native / owned files** | ✅ owned `.md`/text | ✅ owned `.html` file | ✅ (link points to ciphertext) |
| **SQLite canonical** | ✅ pure export read | ✅ pure export read | ✅ transient ciphertext only |
| **Provider seam + redaction** | ✅ local strip (no provider) | ✅ optional reuse `redact::` | ⚠️ redaction should be forced |
| **macOS-first / CI honesty** | ✅ headless-testable | ✅ WebCrypto testable headless | "needs a real Mac"/server |
| **Lock model** | ✅ rides `meeting_is_unlocked` | ✅ same gate | ⚠️ hardest lock-security review |
| **No new deps** | ✅ none | ✅ none (WebCrypto native) | opt. libsodium-wasm / server |

## Opcje i tradeoffy

- **Phase 1 — "Share" reframe + Copy-clean-summary + PDF (S, low risk).** Rename export group → **Share**; add **Copy summary** (strip YAML, flatten `[[Name|alias]]`→plain, exclude transcript) as top item; keep PDF/Markdown; add **Summary-only (default) vs +transcript** toggle. Fixes the junk-paste defect. Zero egress, zero deps. **Recommended MVP.**
- **Phase 2 — password self-decrypting HTML bundle (M, med risk) + optional on-device redaction toggle.** The differentiated "secure + easy" artifact; 100% on-device; recipient needs only a browser + the out-of-band password. Risk = getting crypto params/UX right; redaction asymmetry must be stated honestly.
- **Phase 3 — zero-knowledge relay link + expiry/burn (L, high risk). DEFER.** Correct home for expiry/burn (server-side only). Needs standing infra + full consent scaffold + egress ledger + forced redaction + cross-note flatten. Revisit when a HostedProvider seam exists.
- **Skip:** Obsidian-Publish clone; `.age` bundle (recipient install); ed25519 signing (until identity story); auto-email-to-attendees (privacy anti-pattern); native macOS Share sheet (real-Mac FFI, defer).

## Rekomendacja i pierwszy krok

**Ship Phase 1 now; spec Phase 2 as the flagship; explicitly defer Phase 3.**

**Smallest verifiable slice:** add **"Copy summary"** to the detail Share menu — a deterministic transform that (1) takes the (already-gated) note markdown, (2) strips the YAML frontmatter block, (3) flattens `[[Name|alias]]`/`[[Name]]` → plain text, (4) drops any transcript section, and copies clean text. If it ever writes a file, add a gated `share_note(state, meeting_id, opts) -> Result<String>` in `commands.rs` next to `export_note`, **registered in `generate_handler!` in `lib.rs`**, first statement `meeting_is_unlocked(...)? else AppError::Locked`, explicitly injecting `inject_privacy_receipt_frontmatter`.

**De-risk spike (headless, no Mac, RED-before-GREEN):**
- RED: a sealed meeting → `share`/`copy_summary` returns `Locked`, never content.
- GREEN: a note with `---frontmatter---` + `[[Alice]]` → clean text, no `---`, no `[[`, no transcript.
- For Phase 2: generate a bundle → open the produced `.html` in a fresh Playwright page → type password → assert plaintext renders; assert a Slack-unfurl fetch of a (hypothetical Phase-3) URL gets only ciphertext.

**Invariants any share path MUST satisfy (to pass lock-security-reviewer):**
1. Gate every content read — `meeting_is_unlocked` (commands) / `visibility_clause` (db/MCP), fail-closed `AppError::Locked`.
2. Never hand the FE an on-disk path for a locked meeting; never add a `convertFileSrc`/`asset:` path that skips the gate.
3. Build the artifact from already-gated in-RAM content; never materialize an unsealed copy.
4. Optional redaction reuses `redact::redact` + `active_name_redactor()` (not new regexes); UI states names are removed **only** when the NER model is present.
5. Any off-device egress is opt-in, loud, one-time-consented (pattern: `cloud_egress_consented`) + egress-ledgered.
6. No PII in logs (IDs/stages/counts/sizes only).
7. Flatten/strip cross-note `[[wikilinks]]` from any artifact leaving the device (`vault-titles-egress-leak` class).
8. Refusals are `AppError::Locked`, clean over IPC.
9. If it writes into the vault, `assert_in_vault` path-containment (like `export_canvas`).

## Otwarte pytania / czego nie udało się zweryfikować

- **PDF fidelity** (tables/timeline/graph) via `window.print()` in the packaged WKWebView — needs a signed-build check, not `ng serve`.
- **WebCrypto KDF strength:** PBKDF2 is the zero-dep choice but weaker than Argon2id against offline cracking; if the bundle's threat model includes a determined attacker, Argon2id (new FE dep) may be warranted — a product decision.
- **Whether `notes.markdown` already carries provenance/receipt frontmatter, and whether the summarizer emits `[[wikilinks]]` inline** (vs only frontmatter) — grep a real generated note before finalizing the flatten regex.
- **Forum/Reddit sentiment on the file-vs-link split** — inferred from vendor defaults + Share Note's 683★ + privacy backlash, not a direct poll (confidence: medium). A pass through r/ObsidianMD "how do you share notes" would sharpen it.
- **Out-of-band password UX** for Phase 2 (auto-generate + copy vs user-chosen) — undesigned.
- `markdown2pdf` not build-proven against the heavy ML/SQLCipher tree; the "pure-Rust, no OpenSSL" claim is from lib.rs, not a local build.

## Sources

**Competition/UX (web):** Granola public-by-default [techbuzz](https://www.techbuzz.ai/articles/granola-s-private-ai-notes-are-public-by-default) · [PromptArmor](https://www.promptarmor.com/resources/granola-ai-security-risks-and-remediations) · [Granola sharing docs](https://docs.granola.ai/help-center/consent-security-privacy/sharing-controls) · [Granola share-securely guide](https://www.granola.ai/blog/how-to-share-meeting-notes) · [Granola Slack](https://docs.granola.ai/help-center/sharing/integrations/slack) · Fireflies [share](https://guide.fireflies.ai/articles/2474667467-share-meeting-recaps-with-teammates-participants-specific-people-user-groups-and-non-fireflies-users)/[public](https://guide.fireflies.ai/articles/2479453517-public-meeting-access-how-to-allow-non-fireflies-users-to-view-shared-meeting-recaps)/[recap](https://guide.fireflies.ai/articles/7339665361-how-to-configure-meeting-recap-emails-and-privacy-settings) · [Fathom](https://help.fathom.video/en/articles/295616) · [Otter](https://help.otter.ai/hc/en-us/articles/360048338793-Share-a-conversation) · [Notion Publish](https://www.notion.com/help/public-pages-and-web-publishing) · [Obsidian Publish](https://obsidian.md/publish) · [Unmarkdown: sharing Obsidian notes](https://unmarkdown.com/blog/how-to-share-obsidian-notes) · [Otter suit (Computerworld)](https://www.computerworld.com/article/4041849/enterprise-note-taking-apps-face-legal-scrutiny-as-otter-hit-with-privacy-suit.html) · [Bloomberg backlash](https://www.bloomberg.com/news/newsletters/2026-06-30/ai-meeting-notetakers-spark-privacy-concerns-recording-you-without-consent)
**Crypto/relay patterns:** [Share Note](https://github.com/alangrainger/share-note) + [encryption](https://docs.note.sx/notes/encryption) · [QuickShare](https://github.com/mcndt/obsidian-quickshare) + [E2E design](https://www.mcndt.dev/posts/how-to-e2e-encryption) · [Proton E2EE](https://proton.me/security/end-to-end-encryption) + [password emails](https://proton.me/support/password-protected-emails) · [PrivateBin FAQ](https://github.com/PrivateBin/PrivateBin/wiki/FAQ) · [Yopass](https://github.com/jhaals/yopass) · [self-decrypting-html-page](https://github.com/derhuerst/self-decrypting-html-page) · [hat.sh](https://github.com/sh-dv/hat.sh)
**PDF/crate:** [markdown2pdf](https://lib.rs/crates/markdown2pdf) · [printpdf](https://github.com/fschutt/printpdf) · [genpdfi](https://lib.rs/crates/genpdfi) · [age/rage](https://github.com/str4d/rage) · [ed25519-dalek](https://crates.io/crates/ed25519-dalek) · [rsign2](https://github.com/jedisct1/rsign2)
**Code:** `commands.rs` — `export_note:1489` (gate `:1496`), `export_audio:1411`, `export_canvas:3120`, `meeting_is_unlocked`, `masked_detail:4369`, `consent_to_cloud_egress` · `crypto.rs` (AES-256-GCM) · `summarize/redact.rs` — `redact:183`, `active_name_redactor:79`, `RedactingProvider:307` · `export/obsidian.rs` — `write_note:109`, `inject_provenance_frontmatter:501`, `inject_privacy_receipt_frontmatter:594` (self-declared caveat `:566`) · `detail.component.ts` — export group ~`:296`, `copyMarkdown:2771`, `saveMarkdown:2793`, `saveAsPdf:2904`, print CSS ~`:1823` · `capabilities/default.json` (dialog-only) · `package.json` (`marked ^14.1.4`) · `.claude/rules/lock-model.md` (gate-every-read + convertFileSrc trap).
