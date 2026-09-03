<!-- Architecture spec generated 2026-06-26 via multi-agent analysis (code-grounded + web research). Decision-ready draft, not yet implemented. -->

# Murmur — Always-Local / Optional-Cloud Architecture Spec

> **Status:** Architecture spec, decision-ready. Grounded in the actual tree at `/Users/jakubgawronski/Projects/meetnotes/src-tauri/src` (crate `murmur` / lib `meetnotes_lib`).
> **Non-negotiable invariant:** the app is **100% usable fully local**. Cloud is a per-user **opt-in** layer that is *dark by default*, *additive only*, and *degrades silently to local* on network loss or toggle-off. The **privacy gradient** is the moat: most-sensitive artifacts (audio, raw transcript) stay local; only the polished, shareable note can opt into cloud.

---

## 1. The Architecture (always-local core + optional cloud + the single seam)

### The always-local core (today, unchanged)
Everything runs in one macOS Tauri process, single-user, on-device. Audio capture → on-device Whisper transcription → provider-agnostic AI summary → atomic Markdown writes into the user's Obsidian vault. State lives in **one SQLite file** (`<app-data>/MeetNotes/meetnotes.sqlite`, opened once in `state.rs::AppState::init`) plus the vault `.md` files on disk. Secrets (today: one Anthropic key) live in the **macOS Keychain** (`secrets/keychain.rs`, service `com.meetnotes.app`). A **localhost-only MCP server** (`mcp.rs`, `127.0.0.1:8765`, bearer-token-required by default, read-only) exposes meetings to the user's own Claude Desktop/Code. The only network egress today is the optional Anthropic API call (already PII-scrubbed by the `RedactingProvider` decorator) and the one-time Whisper model download.

This is already ~6 of the 7 Ink & Switch local-first ideals. The two gaps — *"not trapped on one device"* and *"security/privacy by default"* — are exactly what the optional cloud layer + at-rest encryption fill.

### The optional cloud layer (new, opt-in)
Three coupled-but-independently-toggleable capabilities, each attaching at an existing seam:
- **Hosted inference / team brain** → a new arm in `summarize/mod.rs::make_provider` (Seam A).
- **Cloud auth tokens / sync keys** → new account constants in `secrets/keychain.rs` (Seam B).
- **Hosted/remote MCP** → re-host `mcp.rs::handle_rpc` (transport-decoupled) at a remote URL with auth + ACL (Seam C).
- **E2E-encrypted sync** → observe `Db` mutators + the vault dir; needs new change-tracking columns (Seam D — the *only* seam not yet abstracted enough).
- **Enrichment / mesh** → wrap the corpus builder `summarize/vault_context.rs::build_vault_context` (Seam E).
- **New commands** → `commands.rs` + the `generate_handler!` list in `lib.rs:48` (Seam F).

### The single seam where they meet (prose diagram)

```
        ALWAYS-LOCAL CORE                       │  THE SEAM (per-user opt-in)     │   OPTIONAL CLOUD
─────────────────────────────────────────────  │  ─────────────────────────────  │  ──────────────────────
 audio → whisper → segments ─┐                  │                                 │
                             │                  │  Keychain token present?        │
 make_provider() ────────────┼─ local provider  │     (Seam B)         ──── no ──▶ │  (nothing; stay local)
   (Seam A)                  │   (claude_code/   │                     ──── yes ─▶ │  hosted inference
                             │    ollama)        │                                 │  + RedactingProvider wrap
                             │                  │  config flag != off?            │
 build_vault_context() ──────┼─ local LIKE      │     (cloud.enabled,             │  enrichment-by-attachment
   (Seam E)                  │   search         │      enrichment_enabled,        │  (mesh) ; team retrieval
                             │                  │      visibility != 'local')     │  (ACL-filtered)
 Db writes ──────────────────┼─ SQLite (+SQLCipher) ──── sync_enabled? ─── yes ──▶ │  LWW push (E2E ciphertext)
   (Seam D)                  │                  │     (Seam D needs new cols)     │     to dumb relay
                             │                  │                                 │
 export::write_note() ───────┴─ vault .md (plaintext, Obsidian owns) ──▶ share?   │  upload rendered blob,
   (always local)               (the honest dent — Murmur can't OS-lock these)    │  ACL/expiry/revoke link
                                                 │                                 │
 mcp.rs (127.0.0.1:8765, bearer token, read-only) ── re-host handle_rpc ─── auth+ACL ─▶ │  hosted multi-tenant MCP
   (Seam C)                                      │  (RFC 9728/8414/7591/8707)      │  (same tool schema)
```

**Invariants (binding):**
1. **Local always works.** Every feature ships its full-local behavior first; cloud is strictly additive.
2. **Cloud is opt-in, fail-closed.** Default off; gated on a Keychain token AND a config flag AND (for sync) `visibility != 'local'`. Absent token / offline ⇒ silent fallback to local, zero data loss.
3. **Privacy gradient.** Audio never syncs. Raw transcript syncs only on explicit per-artifact opt-in. The polished note is the only thing designed to leave. Anything routed through a cloud LLM passes the redaction firewall first.

---

## 2. Per-Feature Plan

### Feature 1 — Note folders (manual create/organize + existing auto_organize)
- **Full-local:** Turn filing from one-shot write-time into first-class user control. New commands `list_folders` (recursive tree), `create_folder`, `move_note` (atomic rename + cross-FS copy-fallback, reusing `resolve_unique_path`), `set_folder_locked`. The `folder_locked` flag is the whole "manual + auto coexist" story — it makes `resolve_subfolder` early-return on resummarize so the AI doesn't re-file a hand-moved note.
- **+Cloud delta:** (a) `auto_organize` quality improves for free when a cloud provider is connected (rides `make_provider`, already PII-scrubbed; degrades to local classification, then to static `vault_subfolder`). (b) Folder *taxonomy* (just strings) is the cheapest sync unit — only under Feature 3; LWW on the `subfolder` scalar, a lost race = wrong folder, never data loss.
- **Code touchpoints:** `db.rs::migrate` (guarded `ALTER` adding `subfolder TEXT`, `folder_locked INTEGER`); `export/obsidian.rs` (recurse `list_subfolders`; new `move_note`, `create_folder`); reuse `organize.rs::sanitize_folder`; `pipeline.rs::resolve_subfolder` (the one behavioral edit — `folder_locked` early return); 4 new commands in `commands.rs` + `lib.rs:48`; optional `folder_filter` on `build_vault_context`.
- **Data/encryption:** Source of truth = vault FS; SQLite is derived cache. Folder names are **plaintext directory names in the vault** — Murmur cannot encrypt those. Don't treat a folder name as private.
- **Effort:** S–M.
- **Risks:** `move_note` atomicity across filesystems (DB `exported_path` must not commit before bytes are durable); drift if user moves files in Finder behind Murmur's back (treat `exported_path` as a hint); no migration framework makes the `ALTER` fiddly.

### Feature 2 — Biometric/password lock + at-rest encryption
- **Full-local (zero cloud — pure hardening):** **Layer 1:** flip `rusqlite` to `bundled-sqlcipher`, generate a random 256-bit DEK stored in Keychain (`murmur_db_dek`), emit `PRAGMA key` as the first statement in `Db::open` → whole file (pages, WAL, indexes) AES-256 encrypted (~5–15% overhead). **Layer 2:** Touch-ID/password app-unlock that **releases the DEK from the Keychain** (stored behind `kSecAttrAccessControl`/`.biometryCurrentSet`), so the file is physically undecryptable without a fresh biometric — *not* a cosmetic `if authenticated {}`.
- **+Cloud delta:** Almost nothing — this is the *prerequisite* for cloud, not a consumer. When sync lands, the **same DEK** (or an HKDF sub-key) roots the sync envelope; the **same biometric** gates the sync passphrase/KEK. Relay only ever sees `wrapped_dek`.
- **Code touchpoints:** `Cargo.toml` feature flip; `Db::open` (`PRAGMA key` before existing PRAGMA batch); `state.rs::AppState::init` (biometric-gated DEK fetch before open); `secrets/keychain.rs` (new account consts + likely a small `Security.framework` FFI shim for ACL flags); `config.rs` (`K_DB_ENCRYPTED`, `K_REQUIRE_UNLOCK`); new commands; `error.rs` (`Auth`/`Locked`/`Migration` variants); **`lib.rs:setup` MCP gotcha** — the headless MCP re-derives the DB path and opens it with no key; it silently breaks under encryption unless handed the DEK from `AppState`.
- **Data/encryption:** SQLCipher for the local file; do **not** double-encrypt columns locally (kills LIKE search). One-time file migration via `sqlcipher_export()` + backup + verified-open-before-delete.
- **Effort:** M (Layer 1 S–M; the biometric-bound DEK release is the M).
- **Risks (ranked):** (1) DEK release must be biometric-bound, not boolean, or the lock is theater. (2) Encrypt-migration is destructive if it fails mid-swap. (3) MCP keying. (4) Code-signing required for ACLs. **Honest dents:** vault `.md` stays plaintext (Obsidian owns it); audio WAVs stay unencrypted on disk — state both.

### Feature 3 — Sharing notes (local file/PDF/HTML ↔ cloud ACL/link)
- **Full-local:** New `share_note` produces a self-contained artifact: **Markdown** (existing path), **HTML** (pull-comrak pure-Rust render, inlined CSS, `[[wikilinks]]` flattened), **PDF** (via hidden WebView print-to-file — no new heavy dep; Typst as a +1-dep cleaner option). All routed through one **share-prep chokepoint** (`export/share.rs`) where redaction + scope-filtering happen.
- **+Cloud delta:** Distribution, not capability — `create_share_link` uploads the *already-rendered, already-redacted* bytes, returns `{url, expires_at}`. ACL tiers: `link-anyone` (plaintext-on-relay, TLS only — labeled honestly), `link-passphrase` (client-side AES-256-GCM envelope, zero-knowledge relay), `participants-only` (needs Feature 5 identity; degrades to passphrase). `revoke_share` deletes the blob — the one thing local can't do.
- **Code touchpoints:** new `export/share.rs`; **refactor** `redact.rs` to expose `scrub(text) -> (clean, found_pii)` standalone (the one non-additive change); `shares` table; `secrets/keychain.rs` (`murmur_share_token`); `config.rs` (`share_relay_url` — the BYO-relay seam); new commands; `error.rs` (`Network`/`Auth`); optional `cloud/relay.rs` (reqwest, mirrors `anthropic.rs`); best-effort revoke in `delete_meeting`.
- **Data/encryption:** Artifact bytes are ephemeral (re-rendered on demand). Guarded links = on-device Argon2id→AES-256-GCM envelope, same machinery as sync/at-rest.
- **Effort:** Local MD+HTML **S**; +PDF **S–M**; +cloud link/revoke **M client** but **implies an L relay backend** — the first thing in Murmur that *requires* server ops.
- **Risks:** Relay = new trust/ops surface (mitigate with BYO-relay default-none); orphaned blobs on meeting delete (mandatory expiry + revoke-on-delete); PDF fidelity is "what the WebView prints"; **redaction false-negatives are higher-stakes here** (a human keeps the copy) — the `found_pii` confirm step is load-bearing. **Revocation ≠ recall** — say so plainly.

### Feature 4 — Slack/Jira/ClickUp/Linear integrations (MCP-mesh + enrichment-by-attachment)
- **Full-local:** A reference-recognizer: a conservative regex over transcript + note detects external refs (`ENG-4521`, Slack permalinks, `CU-…`), writes them to a `meeting_refs` table + front-matter `enrich:`, renders them as inert links. Manual paste of a ticket/thread body (`context_attachments`) flows into Ask-My-Vault/Brief exactly like a fetched artifact.
- **+Cloud delta:** Automatic hydration. Murmur already lives inside Claude's MCP runtime; the user adds Linear/Slack/Jira/ClickUp as standard remote MCP nodes. **Recommended path = skill-mediated (option 1):** the `/spotkanie`+`/notatki` skills (which *are* the mesh client) fetch the one artifact and hand it back via `attach_enrichment` — **Murmur's Rust core gets zero new network egress or OAuth**, riding the host's existing per-server tokens. Attach-not-ingest: one artifact, same `(corpus, sources)` shape as a `[[link]]`, no vector store, no copy of the org's data.
- **Code touchpoints:** `config.rs` (`K_ENRICHMENT_ENABLED`); `db.rs::migrate` (`meeting_refs`, `context_attachments` tables); new `summarize/refs.rs::extract_refs`; **wrap `build_vault_context` with `enrich_context`** (single branch point); `attach_enrichment`/`list_meeting_refs` commands; route every fetched payload through `redact::scrub` **on ingest**; skill conditionals (no-op if tool absent); optional read-only `get_meeting_enrichments` MCP tool.
- **Data/encryption:** Cache lives in SQLite, **never** as `.md` in the vault. Excluded from any sync scope (it holds *other systems'* confidential data). Encrypted at rest once Feature 2 lands.
- **Effort:** **S** (option 1 — the hard network/OAuth part is borrowed from the host). Option 2 (relax `ClaudeCodeProvider` `--disallowedTools`) = M and punches a sandbox hole. Option 3 (native Rust MCP client + OAuth) = L, not recommended.
- **Risks:** (1) **Redact-on-ingest is load-bearing** — enrichment *inverts* the firewall's outbound assumption; un-scrubbed inbound org PII would back-door into the cloud LLM. (2) Ref-extraction precision (prefer URL-form over bare IDs). (3) Cache-as-attack-surface until Feature 2. (4) Skill↔core `attach_enrichment` schema drift — version it.

### Feature 5 — Cloud enterprise tier (shared context + hosted multi-tenant permission-aware MCP + local↔cloud sync)
- **Full-local:** The existing floor — local DB + vault + the localhost MCP serving the one user's own corpus. "Team of one."
- **+Cloud delta:** (1) A `team`/`org` note is E2E-encrypted and pushed to a hosted relay+index. (2) The same three MCP tools re-hosted at a remote URL, per-tenant scoped, **ACL-filtered before generation** (Glean-style: `asking_user ∈ participants`). (3) LWW pull hydrates a read-only `origin='remote'` mirror so local Ask-My-Vault/MCP also see team notes when online.
- **Code touchpoints:** `config.rs` (`K_CLOUD_ENABLED`, `K_TENANT_ID`, `K_TEAM_RELAY_URL`); `secrets/keychain.rs` (`murmur_cloud_token`/`_refresh`/`murmur_team_dek`); **new `make_provider` arm** `TeamBrainProvider` wrapped by `RedactingProvider`; branch in `build_vault_context` (local ∪ remote-ACL-filtered); **new `src-tauri/src/sync/` module** (LWW push after `upsert_note`; pull/merge in `AppState::init`); `mcp.rs` server-side (parameterize bind addr — the one `format!` at `mcp.rs:24`; add auth+ACL wrapper before `handle_tool_call`; reuse `handle_rpc` verbatim); `error.rs` (`Network`/`Auth`); mirror `EVENT_STATUS` for sync status.
- **Data/encryption:** **Forces the first real schema migration** — add to `meetings`/`notes`: `updated_at`, `rev`, `deleted`, `device_id`, `tenant_id`, `origin`, `visibility`, `sync_scope`, `participants` (JSON ACL). Audio never syncs; transcript only on explicit opt-in. Relay = dumb zero-knowledge store (`{tenant_id, record_id, ciphertext, rev, acl_hash}` + `wrapped_dek`). OAuth 2.1 + RFC 8707 audience-binding + per-tenant scope namespacing (`notes.read:tenant-X`).
- **Effort:** **L** (three subsystems: relay+AS+RS+SCIM/SSO backend, desktop sync engine, hosted-MCP ACL layer).
- **Risks:** (1) **No migration framework exists** — must be built first. (2) DB path hardcoded single-machine; no device/tenant identity today. (3) The moat dents the moment retrieval routes through a hosted LLM — keep relay (zero-knowledge store) strictly separate from any hosted-LLM reasoning surface. (4) macOS-only Keychain/`osascript` blocks non-Mac teammates. (5) **ACL correctness is security-critical** — participant identity must come from the verified IdP, not free-text `[[Person]]` nodes; needs adversarial testing.

---

## 3. Cross-Cutting Decisions

### Sync strategy — **LWW, not CRDT, for v1; E2E-encrypted**
Murmur's data is overwhelmingly write-once-by-one-device (a meeting is recorded on one machine; the note is generated once and lightly hand-edited). True concurrent multi-writer editing of the *same* note is not the use case. **Decision: per-record LWW** — add `updated_at` + `deleted` (tombstone) + `device_id`/`rev`. A genuinely concurrent edit silently loses one side; for Murmur's data that's acceptable (a folder-move lost race = wrong folder, never data loss). **Defer CRDTs** (Loro/Automerge Rust cores) until/unless real-time collaborative *note-body* co-editing is a product requirement — and then scope the CRDT to the **markdown body only**, leaving meetings/segments on LWW. The relay is a **dumb encrypted-message relay + ordering point** that never decrypts — this is what makes cloud truly optional.

### Encryption / key story — **envelope (KEK→DEK), Keychain-resident, SQLCipher at rest**
One key hierarchy, copied from Obsidian/Bitwarden/1Password/Standard Notes:
- Random 256-bit **DEK** encrypts the data. Stored in macOS Keychain behind a biometric ACL → day-to-day use needs no passphrase.
- For sync: **KEK** = Argon2id(sync passphrase + salt); the relay stores only `wrapped_dek` (DEK encrypted under KEK) → **zero-knowledge** (server never sees plaintext or passphrase).
- **At rest local:** SQLCipher whole-file (lowest diff, covers WAL/indexes). **In transit / on relay:** AES-256-GCM envelope — *same ciphertext* shared between at-rest-on-relay and in-transit. **Never double-encrypt locally** (kills LIKE search).
- **Zero-knowledge consequence:** lost passphrase = unrecoverable. Offer an explicit opt-in local DEK recovery export. Consider borrowing 1Password's device-stored secret-key to harden against a relay breach.
- **The honest crypto limit:** Murmur can fully protect its own DB, but **cannot** protect the plaintext `.md` it writes into the user's Obsidian vault — Obsidian owns a plaintext-markdown model. Biometrics in Murmur do nothing to a file Obsidian/Spotlight/Time-Machine can read. The gradient is the only answer; surface it in the export/sync UI (Feature 5's "honest vault boundary" UX, S, ship alongside Feature 2).

### Auth story — **local biometric (key release) vs cloud OIDC/OAuth 2.1**
- **Local:** Touch-ID via `tauri-plugin-biometry` / Security.framework, bound to **DEK release from the Keychain** (`.biometryCurrentSet`), not a boolean. Requires real code-signing (`entitlements.plist` already present).
- **Cloud sign-in:** OAuth 2.1 **Authorization Code + PKCE** + **loopback redirect** (`http://127.0.0.1:PORT/callback`, RFC 8252) + tokens in Keychain + refresh — the exact pattern Claude Code already uses. **For enrichment (Feature 4), reuse the host's per-server MCP OAuth — build nothing.** Only build a native OAuth client if/when Murmur ships its own backend (Feature 5).
- **Enterprise:** OIDC (who the user is) federated to the customer IdP + OAuth 2.1 (what tools they reach) + **per-tenant SCIM** (each tenant its own bearer token + `/Users`+`/Groups`) + RFC 8707 audience-binding so a stolen token can't replay cross-tenant.

### Local MCP ↔ future Hosted MCP — **same tool schema, different transport + scoping**
`mcp.rs::handle_rpc(db_path, body) -> Option<Value>` is already transport-decoupled (takes/returns strings/JSON). The hosted MCP **reuses the exact tool schema and `handle_rpc` verbatim**; only two things change, both server-side: (1) the bind address (the single `format!("127.0.0.1:{MCP_PORT}")` at `mcp.rs:24`), and (2) an **auth + ACL pre-filter wrapper** before `handle_tool_call` opens the DB (the loopback server now requires a bearer token by default, but it has no per-tenant ACL — that is the part hosting has to add). The desktop app keeps the loopback server running; a teammate adds the hosted node as a *second* MCP node (`claude mcp add --transport http murmur-team https://mcp.<tenant>.murmur.cloud/mcp`). Same tools, scoped per-tenant, ACL-filtered.

---

## 4. Sequenced Roadmap

**Phase 0 — Local hardening (cheap wins, no backend) — ship first**
1. **F2 Layer 1: SQLCipher at-rest** (S–M) — one Cargo flip + one `PRAGMA key` + one file migration. Foundation for everything: a trustworthy local DEK. *Watch the MCP keying gotcha.*
2. **F2 Layer 2: biometric DEK release** (M) — the cryptographically-real lock.
3. **F5's "honest vault boundary" UX copy** (S, parallel) — so users aren't misled about what the lock covers.
4. **F1 Note folders** (S–M, independent) — pure local feature, ships any time, no dependencies.

**Phase 1 — Cheapest, highest-value cloud (no Murmur backend needed)**
5. **F4 mesh + enrichment-by-attachment, skill-mediated** (S) — borrows the host's MCP client + OAuth; zero new egress from Murmur's process. **Requires redact-on-ingest wired before any fetch ships.** Soft-depends on F2 (encrypt the org-data cache).
6. **F3 local share (MD/HTML/PDF)** (S–M) — ships standalone; the `redact::scrub` refactor de-risks later envelope work.

**Phase 2 — Single-user multi-device (needs the sync substrate, not full multi-tenant)**
7. **First migration framework + LWW columns** (`updated_at`/`deleted`/`device_id`) — the prerequisite F5 and SQLCipher both flag.
8. **F4-style privacy-gradient `visibility:` field** (S–M) — the per-note toggle the sync filter reads.
9. **F3 cloud share-link tier** (M client + L relay) — first server, can be **BYO-relay** so Murmur ships only the client.
10. **LWW E2E sync** (L) — push/pull a read-only mirror; reuses F2's DEK as envelope root.

**Phase 3 — Enterprise (defer; the heaviest)**
11. **F5 hosted multi-tenant MCP + permission-mirroring + SSO/SCIM** (L) — only after Phases 0–2 prove the local+opt-in seam and the OAuth pattern.

**Smallest enterprise-credible slice:** SQLCipher-at-rest + biometric DEK-release (F2) + per-note `visibility` + a BYO-relay zero-knowledge LWW sync of `team` notes + the hosted MCP re-hosting `handle_rpc` with **OAuth 2.1 + RFC 8707 audience-binding + participant-ACL pre-filter**. That demonstrates: encrypted-at-rest, zero-knowledge sync, permission-aware team retrieval, and self-hostable relay — the four things an enterprise buyer checks — *without* building full SCIM/SSO first.

---

## 5. Honest Risk Register

**Where this dents the local moat (state plainly, never hand-wave):**
- **Vault `.md` is plaintext, always.** Murmur cannot OS-lock files Obsidian owns; SQLCipher + biometrics protect the DB, not the exported notes. Spotlight/backups/any process read them. The gradient (sensitive raw stays in the encrypted DB, only the shareable note is exported) is the *only* answer.
- **Audio WAVs are unencrypted on disk** (`<app-data>/MeetNotes/audio/*.wav`) — the rawest PII. v1 must at least document the gap.
- **Hosted-LLM team retrieval is a genuine step down from local-only inference.** Encrypted *storage* sync stays zero-knowledge, but a "global brain" that *reasons* needs a place to run inference — redacted-but-real content transits a server you run. Keep relay (zero-knowledge store) strictly separate from any LLM-reasoning surface, and surface this in the opt-in UI, not buried.
- **Enrichment cache** (Feature 4) is a new local plaintext store of *other systems'* confidential data until F2 lands.
- **The relay is the first thing requiring server ops** (Features 3 cloud + 5) — a real trust/cost/maintenance commitment. Mitigate with BYO-relay defaults.

**Where this enters crowded ground (no invented claims — these are the named competitors in the briefs):**
- **Glean** owns permission-aware enterprise retrieval (ACL-mirroring, filter-before-generation). Murmur's differentiator is **local-first + attach-not-ingest** — it never builds a derivative index of the org's data; it fetches one artifact on demand, permission-checked at the *source's* token. Do **not** try to out-Glean Glean on org-wide RAG.
- **Granola / other meeting-AI tools** live cloud-first. Murmur's moat is the inverse: **most-sensitive content never leaves the device by construction.** That is the only thing to compete on — not feature parity in the cloud.

**The 2–3 things that MUST be right:**
1. **Permission-aware retrieval (Feature 5).** A single off-by-one in the participant filter leaks a meeting cross-user. ACL must filter **before** content reaches the model, participant identity must come from the **verified IdP** (not free-text `[[Person]]` nodes), and it needs adversarial testing — never unit-mock confidence.
2. **Key management (Features 2/3/5).** DEK release must be **biometric-bound, not a boolean** (else the lock is theater); the encrypt-migration must be transactional + recoverable (a botched swap loses all history); zero-knowledge means lost-passphrase-is-unrecoverable — design the opt-in recovery export from day one.
3. **Focus / scope discipline.** The enrichment firewall must **redact on ingest** (inbound org PII would otherwise back-door to the cloud LLM); enrichment + org data must **never** enter a sync scope or the vault; and CRDTs / org-wide RAG must stay deferred — every premature heavy subsystem dilutes the local-first moat that is the entire reason to choose Murmur.
