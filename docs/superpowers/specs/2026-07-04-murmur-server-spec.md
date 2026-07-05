<!-- v0.2 — Generated 2026-07-04 via Workflow murmur-server-design (5-angle research) + murmur-server-spec-verify (3-lens adversarial review: 3 crit + 24 major + 12 minor findings folded in). Verdict: PASS_WITH_FIXES. Pricing/version = point-in-time. -->
# murmur-server — auth, zero-knowledge storage & sharing (spec + implementation plan)

**Status:** REVIEWED (v0.2) — all adversarial criticals + majors folded in; ready for implementation.
**Target repo:** `murmur-io/murmur-server` (new, separate). Client integration lands in `murmur-io/murmur`.
**Implementer:** AI agent (Claude Opus), milestone by milestone. Every milestone ends with its acceptance checks green + an adversarial-verifier pass; lock-touching client work also passes lock-security-reviewer.

---

## 0. What changed in v0.2 (from adversarial review)

Three reviewers (crypto-security, implementability, product-ops) attacked the v0.1 draft. Folded in: **decrypt-on-user-gesture** (unfurl bots auto-decrypted link shares — the critical), a **normative AAD table** + **canonical serialization** + **mandatory-signature accept rule**, **fixed the second-Mac contradiction** (password suffices; kit is for forgotten-password only), **recovery phrase now skippable at signup**, **abuse-report-with-key** flow, **two domains**, **free-beta + global caps** decision, **key endpoints moved M5→M1**, an **embedded API contract** (§13), **invite anti-spam + lawful basis**, and **ServerSetup stored in Postgres**. Open product decisions the user should confirm are in §14.

---

## 1. Verdict + tier model

The user's plan — *server + sessions first; no-account = 100% local; account = the consent boundary that unlocks server sharing* — is **architecturally sound and industry-converged** (Obsidian's free-local-core + opt-in account; Firefox Send's post-mortem fix was literally "require sign-in to share"). Three standing amendments:

1. **Storage = share-scoped only.** Server stores ONLY ciphertext of explicitly shared notes. Full vault backup/sync is a **written non-goal** (Obsidian Sync needed 3-way merge + version history + conflict UX — an L++ product). The blob model is a clean future extension point.
2. **Mode A (link-share) before Mode B (Murmur↔Murmur).** A exercises ~80% of B's substrate under a simpler threat model; B's day-1 utility is ~zero (no installed base). Every credible E2EE product shipped links before directory sharing.
3. **Ship the local "Copy clean summary" NOW (M-1), in parallel.** The everyday "paste pastes junk" defect (raw YAML + `[[wikilinks]]`) is fixed by the exact `flatten_wikilinks`/`strip_frontmatter` pure fn M3 needs anyway — so build it once, wire it locally on day one (zero egress, zero deps), reuse it in envelopes later. (Source: `docs/research/2026-07-04-secure-note-sharing.md`.)

| Tier | What | Promise |
|---|---|---|
| **0 — no account (default)** | Everything today. Zero server contact. | "Nothing leaves your device." Unchanged, forever. |
| **1 — account (opt-in, free beta)** | Login = THE consent boundary. Unlocks E2EE share-scoped storage + link-shares + user↔user shares. | "Nothing leaves your device *decrypted*. The server stores ciphertext + public keys — it cannot read your notes, and neither can we." |

### 1.1 Honest threat matrix (ship this, don't blend it into one sentence)

| | **Honest-but-curious server** | **Actively malicious server** |
|---|---|---|
| **Mode A (link-share)** | ZK at rest — sees ciphertext + sizes + fetch metadata only | **Can serve poisoned JS to the browser viewer → full plaintext at open-time.** Mitigated by "open in Murmur" native path + versioned open-source page. |
| **Mode B (Murmur↔Murmur)** | ZK — crypto is native; sees ciphertext + social graph | Limited to **first-contact key substitution** (TOFU + safety-words defend); post-pin it's ZK even against a malicious server |

Headline claim (never exceed): **"zero-knowledge at rest against an honest-but-curious server."** SRI is **not** a mitigation against a malicious first-party origin (the server also serves the hash) — do not cite it as one.

---

## 2. Architecture overview

```
┌────────────────────────────┐          ┌─────────────────────────────────────┐
│ Murmur.app (Tauri, Rust)   │  HTTPS   │ murmur-server (axum, Rust)          │
│ e2ee/  (MK, identity keys, │──rustls──│  auth/  (OPAQUE, sessions, devices) │
│        HPKE wrap, envelope)│          │  keys/  (directory, backups)        │
│ share/ (client.rs reqwest, │          │  shares/(blobs, inbox, GC, quota)   │
│        envelope.rs)        │          │  static/share/ (viewer, no npm)     │
│ keychain: device tokens    │          │  Postgres 17 (bytea blobs)          │
└────────────────────────────┘          │  lettre SMTP                        │
        │ SQLite stays canonical         └─────────────────────────────────────┘
        ▼                                        ▲ anonymous, decrypt-on-CLICK
  Obsidian vault                          Browser recipient (WebCrypto)
```

- **Server = dumb encrypted mailbox.** Ciphertext blobs, wrapped keys, public keys, delivery state. Never a note key, title, wikilink, or plaintext.
- **`murmur-protocol` shared crate** (in server repo; app pins by git tag): envelope codec (`nonce(12)||ct||tag(16)` + AAD builders, ported from `crypto.rs`), inner-envelope schema (§4.4), canonical-serialization codec (§4.5), all wire DTOs, route consts, error-code enum, size caps, golden vectors. Format drift → compile error. **License MIT OR Apache-2.0** (protocol only, keeps future clients possible); server crates **AGPL-3.0 + CLA from commit #1**, **permissive-only dependency policy**.
- **Two domains** (§9): an app/API domain and a **separate user-content share domain** — one Safe-Browsing flag on shared content must not take down auth.

---

## 3. Authentication (decision-complete)

**Protocol: OPAQUE (RFC 9807) via `opaque-ke = "4"` (+`argon2`).** NCC-audited lineage (WhatsApp-sponsored), 491k downloads, implements the RFC. Server never sees password-derived material — not even at registration. Ciphersuite: Ristretto255 + TripleDH + Argon2id KSF (**m=64 MiB, t=3, p=1**, `cipher_suite_version` per user for future migration). Rejected: SRP (pre-1.0 unaudited crate, no export_key), Standard-Notes split (server gets crackable material each login).

**Interlock:** OPAQUE's client-side **`export_key`** (secret from server, stable per password) is the ONLY output to the crypto layer: `KEK_pw = HKDF(export_key, "murmur:v1:mk-wrap")` wraps MK. Every password change/reset atomically re-wraps MK.

**Flows:**
1. **Signup:** `POST /v1/auth/signup {email}` → always 202, identical body/latency (anti-enumeration); new → 6-digit CSPRNG code (SHA-256 at rest, 15-min TTL, ≤5 attempts); existing → "you already have an account" email. ≤3 sends/addr/hour. Verify → single-use 10-min `signup_token` → **atomic provisioning endpoint** (§3.1a).
2. **Login:** 3-message OPAQUE; unknown email → **dummy-record path** (`ServerLogin::start(None)`) so responses are indistinguishable; transient state in `pending_logins` (60-s TTL, single-use). TOTP (if enabled) → 5-min single-use `mfa_token`. UI: 2FA protects the account/API surface, **not** E2EE confidentiality.
3. **Sessions (opaque DB-backed tokens, never JWT):** access = 256-bit CSPRNG, SHA-256 at rest, 30-min TTL; refresh = per-device, **rotated every use**, sliding 60 d / absolute 180 d; **reuse detection revokes the whole family** (RFC 9700 public-client MUST). Client stores `device_id` + refresh token in macOS Keychain (`com.meetnotes.app`, existing `set_secret`) — never SQLite, never logged.
4. **Devices:** `platform` enum only, **no user-set names** (PII min). List + revoke = immediate (DB-backed). Logout deletes Keychain entries.
5. **Password change:** one atomic endpoint swaps {new OPAQUE record + new MK_wrap_pw}; revokes all other device families.
6. **Reset & recovery:** email-code (same anti-enumeration) → **with recovery key**: client decrypts server-stored `MK_wrap_rk` locally (successful decrypt = proof-of-possession + authorizes 2FA reset), re-wraps under new export_key, uploads atomically — **data preserved**. **Without recovery key**: two scary confirms → OPAQUE record replaced, account survives, and a **new identity generation** is published (see §4.3 for the takeover caveat); **orphaned** = `key_backups`, `MK_wrap_rk`, and HPKE-wrapped inbox items *awaiting accept* (kept 30 d then purged). **NOT orphaned / must survive**: live link-shares (decrypted by `L` in the fragment, not MK) and already-accepted notes (local). UI states the loss verbatim-grade clear.
7. **Rate limiting:** `tower_governor` per-IP on `/v1/auth/*`; per-account exponential backoff (30 s→15-min cap after 5 fails; never a permanent unauthenticated lock).
8. **ServerSetup (ops-critical):** the OPAQUE `ServerSetup` is generated at first boot and **stored in a Postgres table** (so `pg_dump` covers it automatically — hosted restore drill must prove an existing user still logs in). Self-host README: losing it kills all password logins.

### 3.1a Atomic provisioning (fixes half-created accounts)
Signup's key-setup uploads {OPAQUE record + MK_wrap_pw + (optional) MK_wrap_rk + RK_wrap_mk + self-signed identity bundle} in **one transactional endpoint** that flips `active` only on full success. A pre-active row is **resumable via re-login for 24 h, then GC'd** (email freed). Share ops are blocked until `active`.

### 3.2 Client command surface (complete)
`account_signup`, `account_login`, `account_logout`, `account_status`, `account_change_password`, `account_reset` (with/without kit), `account_delete`, `account_export`, `totp_enroll`, `totp_verify`, `list_devices`, `revoke_device`, `show_recovery_key` (uses `RK_wrap_mk`), plus the share commands (§7). Each: `commands.rs` + `generate_handler!` + one `IpcService` method + `models.ts` type, same change.

---

## 4. E2EE key hierarchy (decision-complete)

**Pattern: random master key (Ente/Standard-Notes shape), not password-derived.** All wraps use the shipped AES-256-GCM format `nonce(12)||ct||tag(16)` + AAD (`crypto.rs` semantics). Version label `murmur-e2ee/v1`. **All GCM nonces are fresh 96-bit `getrandom` values; (key,nonce) never repeats — no counters.**

```
account password ─OPAQUE→ export_key ─HKDF "murmur:v1:mk-wrap"→ KEK_pw (never leaves device)
MK = random 32 B (account master key, on-device at signup)   wraps (AES-256-GCM, per-slot AAD §4.1):
  MK_wrap_local (under Touch-ID account KEK §4.2)  → local SQLite
  MK_wrap_pw    (under KEK_pw)                      → server   ← second-Mac login uses THIS (§4.3)
  MK_wrap_rk    (under RK)                          → server   (only if recovery key enabled)
  RK_wrap_mk    (RK under MK)                       → server   (re-show phrase while logged in)
  sk_enc (X25519 priv) under MK                     → server + local
  sk_sig (Ed25519 priv) under MK                    → server + local
RK  = 24-word BIP39 (256-bit, client-side, SKIPPABLE at signup §4.3)
bundle = {acct_id, generation, pk_enc, pk_sig, created_at} self-signed by sk_sig → append-only key log
NK  = random 32 B per share (fresh on every share + on "Update share")
```

### 4.1 Normative AAD table (each wrap/encrypt slot has a UNIQUE domain string — put builders in `murmur-protocol` + golden vectors; decrypt fails closed on mismatch)

| Slot | AAD string |
|---|---|
| MK_wrap_local | `murmur-e2ee/v1|mk-local|<acct_id>` |
| MK_wrap_pw | `murmur-e2ee/v1|mk-pw|<acct_id>` |
| MK_wrap_rk | `murmur-e2ee/v1|mk-rk|<acct_id>` |
| RK_wrap_mk | `murmur-e2ee/v1|rk-mk|<acct_id>` |
| sk_enc under MK | `murmur-e2ee/v1|sk-enc|<acct_id>|<generation>` |
| sk_sig under MK | `murmur-e2ee/v1|sk-sig|<acct_id>|<generation>` |
| share content C | `murmur-share/v1|<share_id>|<rev>` |
| link NK wrap | `murmur-link/v1|<share_id>|<rev>` |
| HPKE `info` (mode B) | `murmur-wrap/v1|<share_id>` |

`<generation>` in the identity-key AAD prevents a malicious server rolling back to an old (rotated-away) identity key. `<rev>` (§6) prevents stale-NK reuse across "Update share".

### 4.2 Account KEK ≠ folder-lock KEK
Give the account MK its **own** Touch-ID-gated Keychain KEK (clean domain separation from the folder-lock `master_kek`). Note the standing caveat: on unsigned/dev builds biometric degrades to `Ok(true)` (so real Touch-ID protection is only verifiable on a signed build).

### 4.3 Second Mac, recovery, rotation — the honest rules
- **Second Mac = password login only.** Login → `export_key` → unwrap `MK_wrap_pw` from server. **The recovery kit is NOT needed for a second device** — only for a *forgotten password*. (v0.1 wrongly required the kit here.)
- **Recovery phrase is SKIPPABLE at signup** ("Skip — your notes stay on your Mac; add a recovery key later") with a persistent nudge; **required** at the first moment it protects something not-locally-recreatable: enabling Mode B, adding it as the forgotten-password path, or first inbox accept. Skip state = no `MK_wrap_rk` row (keeps the reset-without-kit copy honest).
- **MK/RK do not rotate in v1** (written non-goal): password change re-wraps the same MK. A master-key or recovery-key *exposure* is unrecoverable — remedy is account teardown + fresh account. (An MK-rotation ceremony is future work.)
- **Reset-without-kit publishes a new identity generation with no continuity proof** → an attacker with the victim's **email** can take over the victim's identity for *future* Mode-B first-contacts. Mitigation: on any generation bump, **email-notify all pinned contacts** + cool-down; a legitimate rotation forces a **re-verify-safety-words** step before sharing resumes. Documented in the Mode-B honesty copy + risk register.

### 4.4 Inner plaintext envelope schema (defined once in `murmur-protocol`)
```
{ "v": 1, "title": string, "markdown": string (post-flatten),
  "createdAt": iso8601, "senderHint": string|null }
```
Both the browser viewer (§8 render) and `accept_share` ingest (§7) parse this exact structure. Golden vectors cover a full Rust-encode → JS-decode field-identical round-trip.

### 4.5 Canonical serialization (for ALL signed + AAD-bound payloads)
`serde_json` is **not** canonical → signatures would be malleable and Rust↔JS would drift. Define one canonical encoding in `murmur-protocol` — **deterministic length-prefixed field concatenation** (or canonical CBOR), exact field set + order enumerated — pinned by golden vectors consumed by both Rust and JS CI. Every signed structure explicitly lists the fields it covers.

### 4.6 Algorithms (fixed)
AES-256-GCM (content + wraps; WebCrypto-native — XChaCha20 rejected: not in WebCrypto); HKDF-SHA256; Argon2id m=64 MiB/t=3/p=1 (client-side password KDFs); **HPKE RFC 9180 Base mode, DHKEM(X25519)+HKDF-SHA256+AES-256-GCM + detached Ed25519 signature over the canonical envelope**; BIP39/24 words.

**Zeroize on drop:** `MK, export_key, KEK_pw, KEK_link, KEK_local, NK, RK, L, sk_enc, sk_sig`. Acknowledged residual: the browser JS viewer cannot zeroize key material in memory.

**Crates (new — approval checkpoint at T2.1):** `opaque-ke 4`, `argon2`, `hkdf`, `sha2`, `hpke` (rozbb — Cloudflare-reviewed, not formally audited; pinned suite, swappable behind `wrap_to_recipient()`), `ed25519-dalek 2` (+`x25519-dalek`), `bip39`, `totp-rs`, `tower_governor`. Reused: `aes-gcm 0.10` (NCC-audited), `zeroize`, `subtle`, `getrandom`, `reqwest`.

### 4.7 Mode A (link-share) crypto — with the anti-bot + anti-crack fixes
- `L = random 32 B` → URL on the **share domain** `https://<share-host>/s#<share_id>.<b64url(L)>`. Fragment is never sent over HTTP — **but anyone holding the URL holds the key** (see §8 gesture gate).
- `KEK_link = HKDF(ikm, salt_A, "murmur-link/v1|<share_id>|<rev>")` where `ikm = L` (no password) or `ikm = L || Argon2id(password, salt_p)`. **The optional password strengthens the encryption** (a leaked URL alone can't decrypt).
- **Fetch gate (fixed):** the server-side fetch gate secret is derived from **L, not the password** — `gate_secret = HKDF(L, "murmur-link/v1:gate")`, proven via a **server-issued challenge nonce (challenge-response, not a static bearer verifier)** to prevent replay. **Never store a password-only verifier** (it's offline-crackable by the very server the password defends against). If no password, `gate_secret` alone still forces possession of `L` to fetch.
- Browser needs only WebCrypto AES-GCM + HKDF + `hash-wasm` Argon2id (~11 kB; no audit — mitigated: the fragment carries 256-bit entropy, Argon2 only hardens).

### 4.8 Mode B (user↔user) crypto — with mandatory-signature + binding fixes
- Sender fetches recipient bundle by email → **TOFU-pin `{account_id, pk_sig fingerprint}`** locally (**pin on `account_id`, not email**, so an email change doesn't strand pins), show **BIP39 safety words** (over both sorted bundles) on first contact; key change = **blocking warning + out-of-band re-compare** (not click-through).
- `HPKE.seal(pk_enc_B, info="murmur-wrap/v1|<share_id>", NK)`; the whole canonical envelope is **Ed25519-signed by `sk_sig_A`**.
- **`accept_share` binding rules (BINDING — HPKE Base has no sender auth):** (1) **reject any envelope without a valid Ed25519 signature from the pinned sender key** — no unsigned path; (2) the signature MUST cover `{sender acct_id+generation, recipient acct_id+pk_enc, share_id, rev, HPKE enc, ciphertext, AAD}`; (3) recipient MUST verify `acct_B == self` and `acct_A == locally-pinned sender` before any HPKE open.
- **v1 MITM honesty (ship):** "The server is trusted to hand out the right public key the *first* time you share with someone. After first contact the key is pinned; any change blocks sharing with a warning. To rule out first-contact substitution, compare safety words out-of-band." Key transparency deferred (Signal reached KT public beta only May 2026); append-only key-log table exists day one for retrofit.

### 4.9 Recovery composition (verified)
Local vault never routes through account keys (SQLCipher DEK + folder CKs come from Keychain). Lose password+phrase+device ⇒ server-side shares unrecoverable (ZK working as intended); local notes untouched; if the Mac survives, nothing is lost.

---

## 5. Server tech stack (decision-complete)

| Decision | Choice | Why |
|---|---|---|
| Language/framework | **Rust, axum 0.8 + tokio** | Shared `murmur-protocol` crate kills client↔server drift; solo dev is a Rust expert; sqlx compile-time SQL = max guardrails per AI-generated line |
| Database | **Postgres 17 + sqlx 0.9** (`tls-rustls-ring-webpki`, embedded `migrate!`) | Relational/concurrent; compose one-liner; identical artifact hosted & self-host |
| Blob storage | **Postgres `bytea`** behind a `BlobStore` trait, 1 MiB/blob | KB ciphertext; one backup story (`pg_dump`); `S3BlobStore` is a later data-move |
| Share page | **Embedded static files, same binary**, hand-written HTML+JS, **no npm** | Self-host correctness; CSP `default-src 'none'; script-src 'self'`; golden vectors pin the Rust↔JS seam |
| Email | **`lettre` SMTP only**, env-configured | Self-host = any relay; hosted = Resend SMTP (→ SES eu-central-1 if EU residency required, env-only). Emails NEVER carry note content |
| Deploy (hosted) | **Hetzner CX22 (~€4.35/mo) + docker-compose** (server + postgres + Caddy auto-TLS + backup cron) | ~10× cheaper than Fly; hosted dogfoods the self-host artifact |
| Backups | nightly `pg_dump \| age -e \| rclone → R2`; **ServerSetup is IN Postgres** so it's covered; restore drill proves login | RPO ≤24 h, RTO = manual, single-node no-failover **by design** (state it) |
| Rate limiting | `tower-governor` (SmartIp behind Caddy) + per-account + **global storage cap + signup throttle** | Free-beta abuse can't fill the 40 GB disk |
| Observability | `tracing` JSON → stdout; healthchecks.io ping; **disk-usage + aggregate-blob-bytes alert** | first-write-failure must not be the first signal |
| CI | GH Actions (Linux): fmt, clippy -D warnings, `cargo test` + postgres service, docker build (cargo-chef), golden-vector JS test | Linux CI has docker (unlike the app's macOS CI) — the authoritative client↔server round-trip lives HERE |

**Repo layout:** workspace `crates/{murmur-protocol (MIT/Apache, no server deps), murmur-server (axum)}`, `crates/murmur-server/{migrations, static/share, tests (#[sqlx::test])}`, `docker/{Dockerfile cargo-chef, docker-compose.yml}`, `.github/workflows/ci.yml`, `LICENSE (AGPL-3.0)` + `CLA.md` (CLA bot from commit #1) + permissive-only dep policy in `CONTRIBUTING`. **Repo public from M0** (AGPL anyway) so the app's macOS CI can fetch the git dep.

**Protocol versioning:** DTO changes **additive-only** within v0.x; `deny_unknown_fields` **only on server-side REQUEST deserialization** (a security win) — **client-consumed RESPONSE DTOs tolerate unknown fields** (forward-compat, or a routine additive server deploy breaks every shipped app). Tag `protocol-vX.Y` at the END of each milestone that touches the crate (M0, M1, M3, M5); the app repo re-pins as the FIRST task of each consuming milestone. Golden vectors append-only per tag.

---

## 6. Data model + revision + share_id (summary; §13 is the endpoint contract)

Tables: `users` (incl. `server_setup` singleton row, `cipher_suite_version`, `active`, `pre_active_expires_at`), `devices`, `access_tokens`, `refresh_tokens` (family/rotation/reuse), `pending_logins`, `public_keys` (generations, **append-only**), `key_backups`, `blobs` (`sha256`, `storage_ref`), `shares` (**owner_id**, `mode`, `blob_id`, `rev int default 1`, `expires_at`, `revoked_at`, `max_downloads`, `download_count`, `gate_salt`), `share_recipients` (state: `pending_invite→awaiting_key→pending_accept→accepted|declined|stale_key`), `pending_invites` (+ `suppression`), `abuse_reports`, `rate_counters`.

- **`share_id` = client-minted UUIDv4** (server validates format), **globally unique + bound to `owner_id`**; `POST /v1/shares` on a duplicate returns the **uniform error shape** (no existence confirmation); all mutate/revoke/attach ops check ownership. Client-minting is for AAD pre-binding only, never authorization.
- **`rev`** = integer from 1, incremented by "Update share", stored on `shares`, returned in `GET meta` + inbox DTO, part of content AAD; in golden vectors.
- **GC (in-server tokio task, hourly):** expired blobs, orphans >1 h, unclaimed invites >14 d, stale sessions, `last_ip` >30 d, tombstones >30 d, **pre-active accounts >24 h**.
- **Quotas:** 100 MB/account, 1 MiB/blob, 50 active shares, 100 uploads/day, 20 key-lookups/day, **invites ≤10/day/account + ≤2 lifetime per target address (suppression-checked)**, link fetch 30/min/IP, link TTL default 30 d (max 365). **Global:** aggregate-storage cap + signup-rate throttle.
- **Metadata the server unavoidably learns (goes verbatim in the privacy policy):** emails; sharing social graph (who→whom, when); ciphertext sizes/timestamps/download counts; share settings; public keys; device platform+count; IPs (30-d, abuse only). **Deliberately absent: titles, filenames, note text, wikilinks, tags, folder names, local meeting ids** (title travels INSIDE the ciphertext).

---

## 7. Murmur client seam (binding invariants)

New modules `src-tauri/src/e2ee/` + `src-tauri/src/share/`. New local tables (additive `Db::migrate()`, `CREATE TABLE IF NOT EXISTS`): `outbound_shares(share_id, meeting_id, mode, nk BLOB, recipient_acct_id?, rev, state, created_at)` — **store `share_id` + `meeting_id` only for display; NO title column** (derive the title via the gated meeting read, so a sealed meeting's title can't leak from the share list) — and `inbound_shares(share_id, meeting_id, sender_acct_id, accepted_at)`.

**Binding invariants (lock-security-reviewer audits exactly these):**
1. `share_note_to_*`: **first statement** `meeting_is_unlocked` → `AppError::Locked` (copy `export_note`).
2. `accept_share(share_id, folder_id: Option<String>)`: **write-gate first** (mirror `ingest_into_folder`'s sealed-folder refusal); default target = an auto-created **unsealed** "Shared" folder; then the §4.8 signature+binding checks; then ingest = `insert_meeting` (status `Exported`) + `upsert_note` (`provider_id:"shared"`) + atomic `export::write_note` with `shared-by`/`shared-at`/`share-id` frontmatter; **idempotent on `share_id`** via `inbound_shares`.
3. Strip YAML frontmatter + **`flatten_wikilinks`** (`[[T]]`→`T`, `[[T|a]]`→`a`) + strip `obsidian://` refs before enveloping — pure fn, **TDD'd first** (the `vault-titles-egress-leak`/`e59672e` class).
4. Every upload writes a **content-free egress-ledger row** (host + byte sizes). **The fragment URL is key material: never logged, never in the ledger** (the `convertFileSrc`-trap discipline).
5. First-ever share = one-time loud consent modal ("this uploads the encrypted note to <host>"), fail-closed shape mirroring `consent_to_cloud_egress`. Share commands fail closed `AppError::Unavailable` when logged out.
6. **`list_my_shares` output for a sealed-not-unlocked meeting is masked** (`locked:true`, no title — route through `meeting_is_unlocked`).
7. **`lock_folder` enumerates its meetings' active server shares and warns** (v1: warn + offer revoke; auto-revoke is a product choice — default = warn). A locked note's live share still resolves server-side (the note left the device deliberately) — surface this honestly.
8. No content-derived strings in any request field, URL, or log; no PII in logs (ids/stages/counts/sizes).
9. Server URL config validated by the `validate_gateway_url` pattern (reject embedded creds; http only for loopback). Self-host = a Settings field.
10. Redaction firewall deliberately NOT applied to share payloads (intentional full-content transfer under explicit per-share action, E2EE) — different consent class than LLM egress; documented in the modal.
11. Tokens/device-id → Keychain only.

Share semantics: **snapshots**, explicit "Update share" (fresh NK + `rev++` + re-upload = new loud ledger event). No auto-updating shares. Revoke deletes server ciphertext; UI states already-downloaded copies can't be recalled.

---

## 8. Share page (Mode A viewer) — decrypt-on-user-gesture

Static, dependency-free HTML+JS served at `/s` on the **share domain**. **The page loads INERT** — a "user content" interstitial only; it calls `GET meta` + `POST fetch` + decrypt **ONLY after an explicit click** (link-preview/unfurl bots don't click → no auto-decrypt-to-third-party-cache; this closes the critical). Then: read `#<share_id>.<L>` → server challenge-response with `gate_secret=HKDF(L,…)` → optional password (Argon2id via hash-wasm) → HKDF → AES-GCM decrypt → render **sanitized** markdown (no raw HTML/JS from note content; `rel=noopener nofollow` + external-link click-through warning on links), report-abuse link (§9), no cookies/analytics/third-party origins, CSP-hardened. Honest footer: what the page can/can't guarantee (§1.1). **Real mitigations against a malicious origin** (SRI is not one): a prominent **"open in Murmur" native deep-link** for the native (non-JS-served) crypto path, plus a versioned, reproducible, open-source page with published hashes. Conformance: golden vectors from `murmur-protocol` consumed by a JS CI test.

---

## 9. Ops / legal (hosted instance)

- **Two domains** (one-way door — decide before the first real link): an **app/API domain** and a **separate user-content share domain** (may CNAME to the same box via Caddy vhosts). A Safe-Browsing flag on shared content must not take down login/share-creation.
- **GDPR:** murmur-io = data controller for emails + share metadata. Needed: privacy policy (with the §6 metadata list verbatim), ToS (min age 16, EU-safe), DPAs (Hetzner, Resend/SES), data-subject path = `DELETE /v1/account` (72-h erasure, re-auth) + `GET /v1/account/export`, breach readiness. **`pending_invites` (non-user PII):** state lawful basis (legitimate-interest balancing) + 14-day retention + a **suppression list** + an **unsubscribe link in every invite email**; declined/unsubscribed addresses are never re-emailed.
- **DSA Art. 16/17:** report-abuse on every share page + abuse mailbox + published contact — mandatory at any size.
- **Abuse pipeline (must actually adjudicate E2EE):** the viewer's report flow includes, **with the reporter's explicit consent, the full capability (share_id + fragment key)** so the solo operator can decrypt the *single* reported item to review → takedown/preserve/report. Report *without* a key → metadata/velocity heuristics only, **no automatic deletion** (else it's a free censorship button). Stated in the viewer footer so it isn't a silent ZK exception.
- **CSAM (US §2258A):** actual-knowledge reporting; no proactive-scan duty; on a keyed report that reveals CSAM → report NCMEC + preserve 1 y.
- **Email:** SPF+DKIM+DMARC aligned; transactional ESP; capability never travels in email; no attacker free-text.
- **Cost:** <€20/mo at 0–1000 users; global caps guard the disk. Snapshot semantics ⇒ a server outage breaks nothing locally (but recipients lose that-day's un-backed links; RPO ≤24 h stated on the status page).

---

## 10. Risk register (top 12)

| # | Risk | L | Mitigation |
|---|---|---|---|
| 1 | Unfurl bots auto-decrypt link shares | H | Decrypt-on-click; challenge-nonce fetch; honest "URL = key" copy (§8) |
| 2 | Scope-creep into full sync/backup | H | Written non-goal; share-scoped only |
| 3 | "ZK" overclaim vs server-served JS (mode A) | H | Threat matrix §1.1; native "open in Murmur"; no SRI theater |
| 4 | Key loss = #1 support burden | H | Recovery kit **when it matters** + "we cannot reset" UX |
| 5 | Email compromise → future-share identity takeover (reset-without-kit) | M | Notify pinned contacts on generation bump + re-verify safety words |
| 6 | Server key-swap MITM (mode B first contact) | M | TOFU + pin-on-account_id + blocking change-warning + safety words |
| 7 | Invite-spam / non-user PII | M | ≤10/day + ≤2/target, suppression, unsubscribe, lawful basis |
| 8 | Share domain = phishing host; takes down API | M | Two domains; sanitized text-only render; reports; TTLs; caps |
| 9 | Free-tier = free E2EE dropbox fills disk | M | Free-beta + global storage cap + signup throttle (§14 decision) |
| 10 | ServerSetup loss on hosted rebuild kills all logins | M | ServerSetup in Postgres → covered by pg_dump; restore drill proves login |
| 11 | Solo-dev ops: links availability-critical | M | Boring single-VPS; status page; RPO/RTO stated; nothing breaks locally |
| 12 | Copyleft dep / unsigned commit voids dual-license | L | Permissive-only policy + CLA bot from commit #1 |

---

## 11. Milestones (revised ordering)

Discipline: TDD where the artifact is logic; each milestone ends with acceptance green + adversarial-verifier; lock-touching client work also passes lock-security-reviewer. Which acceptance checks are **headless-CI**, **local-only**, or **need the deployed host (M4)** is marked per task.

### M-1 — Local "Copy clean summary" (app repo, parallel with M0; zero egress, zero deps)
- **T-1.1** `flatten_wikilinks` + `strip_frontmatter` pure fn (TDD RED first: `---`+`[[Alice]]` → clean). Wire to a "Copy clean summary" item on the detail Share menu, gated by `meeting_is_unlocked`. **This same fn is reused by T3.2.** (Headless-CI.)

### M0 — Repo scaffold + protocol crate (de-risking slice)
- **T0.1** Scaffold `murmur-io/murmur-server` workspace (§5); **public repo**; CI; AGPL + CLA bot + permissive-only policy. (Headless-CI.)
- **T0.2** `murmur-protocol`: envelope codec, **inner-envelope schema (§4.4)**, **canonical-serialization codec (§4.5)**, AAD builders (§4.1), wire DTOs (camelCase; `deny_unknown_fields` request-only), route consts, **error-code enum**, size caps, **golden vectors** (incl. inner-envelope + AAD + canonical-ser). (Headless-CI.)
- **T0.3** OPAQUE spike: axum + client crate registration→login round-trip vs a **Postgres-persisted** `ServerSetup`. Acceptance: export_key identical reg/login; server restart still authenticates; dummy-record path byte-shape-identical for unknown emails. (Server-Linux-CI.)
- **T0.4** `/healthz` + `POST/GET /v1/blobs` (bytea, 1 MiB) + migrations + docker-compose (server+postgres+caddy). Acceptance: compose up clean; blob round-trips. (Local + Server-CI.)

### M1 — Auth core + key/backup storage (server)
- **T1.1** Signup + email verify (lettre trait + dev MailCatcher; anti-enumeration: identical 202s, hashed codes, TTLs, attempt caps). (Server-CI.)
- **T1.2** OPAQUE register/login (`pending_logins`; dummy path; `cipher_suite_version`). (Server-CI.)
- **T1.3** Sessions/devices: opaque tokens (hashed), 30-min access, rotating refresh + **family reuse-detection revokes family** (test), device list/revoke, logout. (Server-CI.)
- **T1.4** TOTP enrol/verify + `mfa_token` leg. (Server-CI.)
- **T1.5** Rate limiting (per-IP + per-account backoff). Acceptance: brute-force hits backoff, never permanent lock; **byte-shape-identical responses** as the gate (timing measurement = local-only, non-blocking). (Server-CI.)
- **T1.6** Password change (atomic record+MK_wrap_pw swap) + reset flows (with/without kit; §3.6 orphan set only). Acceptance: both reset paths tested. (Server-CI.)
- **T1.7 (moved from M5)** **Key + backup storage**: `PUT /keys`, `GET /keys/me`, `PUT/GET /keys/backup` (MK_wrap_pw, MK_wrap_rk, RK_wrap_mk, self-signed bundle) — required by T2.4 signup + T1.6 recovery. (Server-CI.)
- **T1.8** **Account deletion + export**: `DELETE /v1/account` (72-h erasure job, re-auth, touches every table) + `GET /v1/account/export`. (Server-CI.)

### M2 — E2EE client core (app repo)
- **T2.1** **New-crates approval checkpoint** (§4.6) → add deps.
- **T2.2** MK gen + wraps (per §4.1 AAD) + RK (BIP39) + identity keypair + self-signed bundle; **own account KEK** (§4.2). Acceptance: round-trip + fail-closed + **cross-context wrong-AAD** tests (mirror `crypto.rs`). (Headless-CI.)
- **T2.3** HPKE NK wrap/unwrap + envelope sign/verify (canonical ser); `KEK_link` + `gate_secret` derivation. Acceptance: the **interop fixture test** — Rust emits JSON vectors; a ~50-line Node script decrypts with only WebCrypto + hash-wasm (proves Rust↔browser seam pre-server). (Headless-CI.)
- **T2.4** Account commands (§3.2) wired to server (atomic provisioning §3.1a; tokens in Keychain; **second-Mac = password path**). Acceptance: signup→login→re-login + the git-dep payoff (app-encrypted blob → server → back → byte-identical) as an `#[ignore]` integration test (`MURMUR_E2E_SERVER=…`); authoritative round-trip runs in **server-repo Linux CI**. (Local + Server-CI.)
- **T2.5 (split out)** **FE: account onboarding + settings** — signup/verify/login screens, **recovery-phrase show+confirm+re-entry ceremony** (skippable §4.3), account pane, logout, device list, scary no-kit reset confirms. Zoneless/signals. Acceptance: Playwright @:1420 w/ mocked invoke. (Headless-CI.)

### M3 — Link-share vertical slice (Mode A, end to end)
- **T3.1** Server: shares create/list/revoke (link), anonymous `meta`+`fetch` (**challenge-nonce gate §4.7**, expiry, max_downloads, uniform 404s), quotas + global caps, GC task, `POST /v1/report` (with-key path §9). (Server-CI.)
- **T3.2** Client: reuse M-1's `flatten_wikilinks`/`strip_frontmatter`; `share_note_to_link` + `revoke_share` + consent modal + ledger rows + gate/masking invariants (§7). (Headless-CI.)
- **T3.3** Share page (§8): inert-until-click viewer, challenge-response fetch, decrypt, sanitized render, CSP, report link, honest footer. Acceptance: Playwright full share→**click**→decrypt; a **headless prefetch without a click yields no plaintext**; fetch without the fragment yields only ciphertext; XSS in note content stays inert. (Server-CI + Playwright.)
- **T3.4** FE Share menu (link share w/ expiry+password, list my shares w/ masking, revoke). (Headless-CI.)
- **T3.5** Adversarial-verifier + lock-security-reviewer on the whole slice. Acceptance: sealed → `Locked`; URL never in logs/ledger; ledger row per upload; revoked link uniform-404s; sealed meeting masked in `list_my_shares`.

### M4 — Hosted deployment
- **T4.1** Hetzner CX22 + compose + Caddy TLS (both domains) + healthchecks + disk/blob alerts + nightly encrypted pg_dump→R2 + **restore drill proving an existing user still logs in**. (Deployed-host.)
- **T4.2** DNS/SPF/DKIM/DMARC; deliverability smoke. **Blocked on the domain/name decision (§14).**
- **T4.3** Privacy policy + ToS (min age; metadata list verbatim) + abuse mailbox + DSA contact; references `DELETE /account`/`export`.

### M5 — Murmur↔Murmur (Mode B, v1.1)
- **T5.1** Server: key **directory** (generations, append-only log, lookup rate+audit), inbox, `share_recipients` state machine, attach-wrapped-key (confirm step), `pending_invites` + claim emails + **suppression/anti-spam (§6)**, accept/decline. (Server-CI.)
- **T5.2** Client: `share_note_to_user` (TOFU pin-on-account_id + safety words + blocking change-warning), `share_rewrap_pending()` on launch + **pending-rewrap badge/nudge**, `list_share_inbox`, `accept_share` ingest (§7 inv. 2 + §4.8 signature rules), `decline_share`. (Headless-CI for logic.)
- **T5.3** FE: inbox UI, fingerprint-phrase verification sheet, invite flow — **link-share is the DEFAULT suggestion when the recipient email has no registered key** ("They don't use Murmur yet — send a protected link? [recommended]"); invite email states the macOS requirement. (Headless-CI.)
- **T5.4** Recovery interactions: `stale_key` re-wrap after reset-without-kit (+ notify-pinned-contacts §4.3); **second-Mac password-login path + recovery-kit fallback** (both tested — NOT kit-only). (Server + Headless-CI.)
- **T5.5** Full adversarial + lock-security pass (accept-side vault write = highest bar).

### Explicit non-goals (v1/v1.1)
No vault sync/backup/multi-device mirroring; no attachments/audio in shares; no live-updating shares; **no MK/RK rotation** (exposure = teardown); no key transparency (table retrofit-ready); no browser account dashboard; no passkeys (TOTP only); no push (inbox polls on launch/refresh); **no billing/payments** (free beta, §14); no email-change flow in v1 (pins are account_id-keyed so it's addable later).

---

## 12. API contract pointer
The full endpoint contract (25 endpoints: method, path, auth, request/response schema, error-code enum, `GET meta`/`POST fetch`/gate challenge-response, `gate` storage + constant-time compare) is **§13 below** and is normative; `murmur-protocol` (T0.2) is the executable copy of the DTOs + error codes.

## 13. Endpoint contract (normative — abbreviated shapes; `murmur-protocol` is authoritative)

`base /v1`, bearer opaque tokens, JSON errors `{code, message}` from the shared error enum, **uniform 404** for missing/expired/revoked/unauthorized/duplicate-share.

- **auth:** `POST /auth/signup {email}`→202; `POST /auth/verify-email {token}`; `POST /auth/provision {signup_token, opaque_record, mk_wrap_pw, mk_wrap_rk?, rk_wrap_mk?, bundle}`→201 (atomic, §3.1a); `POST /auth/login/start {email, ke1}`→`{login_id, ke2}` (dummy for unknown); `POST /auth/login/finish {login_id, ke3, device:{platform}}`→`{access, refresh, device_id, mfa_required?}`; `POST /auth/login/mfa {mfa_token, code}`; `POST /auth/refresh`; `POST /auth/logout`; `POST /account/password {new_opaque_record, new_mk_wrap_pw}`; `POST /account/reset/*` (with/without kit); `DELETE /account` (re-auth); `GET /account/export`.
- **devices:** `GET /devices`; `DELETE /devices/{id}`.
- **keys:** `PUT /keys {generation, algo, pk_enc, pk_sig, bundle_sig, prev_sig?}`; `GET /keys/me`; `POST /keys/lookup {email}`→`{registered, key?:{generation, pk_enc, pk_sig, fingerprint}}` (auth, 20/day, audited); `PUT/GET /keys/backup` (≤16 KiB opaque).
- **blobs:** `POST /blobs` (octet-stream, ≤1 MiB)→`{blob_id}`; `GET /blobs/{id}` (auth by recipiency/ownership).
- **shares:** `POST /shares {share_id(uuidv4), mode, blob_id, rev, expires_at?, max_downloads?, gate_salt, password_gate?, recipients?}`→201 (dup share_id → uniform error); `GET /shares?state=`; `PUT /shares/{id}/keys {recipient_acct_id, wrapped_key, key_generation}` (owner, confirm step); `DELETE /shares/{id}` (owner).
- **link (anonymous, share domain):** `GET /link/{id}/challenge`→`{nonce, password_required, gate_salt, argon_params?, rev, size, expires_at}`; `POST /link/{id}/fetch {gate_response, password_proof?}`→ciphertext stream (challenge-response; expiry/max_downloads/rate enforced; uniform 404 on any failure).
- **inbox:** `GET /inbox`→`[{share_id, sender_acct_id, sender_fingerprint, wrapped_key, key_generation, rev, size, created_at, state}]`; `POST /shares/{id}/accept`; `POST /shares/{id}/decline`.
- **abuse/ops:** `POST /report {share_id, capability?, reason, evidence?, contact?}` (anonymous; capability = opt-in with consent); `GET /healthz`; `GET /metrics` (internal; counters only).

## 14. Open product decisions (confirm before/at M4 — genuine forks, not implementation details)

1. **Free vs paid Tier 1.** Default set here = **free during beta, quota-limited, payments out of scope v1** (a paid tier is the planned funding path; adding it later touches signup/ToS/DPA — flagged). Global storage cap + signup throttle guard the disk regardless. *Confirm or switch to paid-now (adds billing to the plan).*
2. **The name/domain.** Two domains needed (app/API + share-content), and share links immortalize the share domain → the naming blocker (`docs/research/2026-07-03-launch-plan.md`) is now on the critical path for M4.2.
3. **`lock_folder` × active shares:** default = **warn** (offer revoke). *Confirm vs auto-revoke-on-lock.*
4. **Argon2id params** need a real-device benchmark (oldest supported Mac + a phone browser for the viewer) before freeze — params are versioned in the blob header, tunable.
5. `rust-hpke` exact API + `opaque-ke` v4 post-v1.2 audit gap (paid re-audit = later hosted-tier cost). EU CSAR ("chat control") status — re-check before hosted launch.
