# Murmur — Security Audit

> **Date:** 2026-06-26 · **Version audited:** 0.2.0 (`claude/funny-gates-drq48x`)
> **Scope:** full repository — Rust core (`src-tauri/`), Angular frontend (`src/`), Tauri
> config + capabilities. **Method:** manual source review of the whole attack surface (IPC
> command layer, localhost MCP server, subprocess spawning, AI-provider seam + redaction
> firewall, SQLite access, secrets handling, filesystem/vault writes, model download, webview
> rendering). No dynamic/runtime testing was performed. **Threat model:** a local-first,
> single-user macOS desktop app holding private meeting audio, transcripts, and AI notes; the
> assets worth protecting are that corpus and the user's Anthropic API key.

## Summary

| ID | Severity | Finding | Location |
|----|----------|---------|----------|
| **H1** | **High** | Localhost MCP server: no auth, no `Host`/`Origin` validation, always-on → local + drive-by (DNS-rebinding) read of all meetings | `src-tauri/src/mcp.rs`, `src-tauri/src/lib.rs:103` |
| **M1** | Medium | Whisper model download has no integrity check (TLS-only trust) → malicious GGUF → whisper.cpp (C/C++) parse | `src-tauri/src/transcribe/model.rs:112` |
| **M2** | Medium | `csp: null` — no Content-Security-Policy on the webview (defense-in-depth gap; actual markdown path is sanitized by Angular) | `src-tauri/tauri.conf.json:22` |
| **M3** | Medium | `export_note` / `export_audio` write to an arbitrary caller-supplied `dest_path` (arbitrary file write reachable from the webview) | `src-tauri/src/commands.rs:409,428` |
| **L1** | Low | `vault_subfolder` (user setting) joined into the path with no `..` containment check | `src-tauri/src/export/obsidian.rs:121` |
| **L2** | Low | `add_reminder` builds an AppleScript string; quote/backslash escaped, but control chars not stripped | `src-tauri/src/commands.rs:637` |
| **L3** | Low | MCP `Db::open` runs `migrate()` (a write txn) on every unauthenticated request | `src-tauri/src/mcp.rs:120` |
| **L4** | Info | Transient system-audio WAV in `temp_dir()` (per-user on macOS, fine; note for any future port) | `src-tauri/src/commands.rs:153` |
| **L5** | Info | Post-rebrand identifier/service still `com.meetnotes.app`; app-data dir `MeetNotes` | `tauri.conf.json:5`, `secrets/keychain.rs:5` |

The codebase is, overall, **carefully written** — see [§ Verified-good](#verified-good-controls-confirmed-present) for the substantial list of controls already in place (parameterized SQL, no `unsafe`, Keychain-only secrets, the redaction firewall, hermetic subprocess spawns, Angular-sanitized rendering, minimal Tauri capabilities). The one finding that should block a public/shipped release is **H1**.

---

## H1 — Localhost MCP server is unauthenticated, unvalidated, and always-on · **High**

**Where:** `src-tauri/src/mcp.rs` (whole module); spawned at `src-tauri/src/lib.rs:103-108`.

**What.** On every app launch, `setup()` calls `crate::mcp::spawn(db_path)` **unconditionally** —
there is no `mcp_enabled` field in `AppConfigDto` (`commands.rs:55-74`), so nothing in Settings
can turn it off. The server (`tiny_http`) listens on `127.0.0.1:8765` and answers JSON-RPC with
three read tools — `search_meetings`, `get_meeting`, `list_recent_meetings` — that return **full
meeting titles, transcripts, and AI notes** straight from SQLite. The request handler:

- performs **no authentication** of any kind;
- never inspects the **`Host`** header;
- never inspects the **`Origin`** header;
- never checks **`Content-Type`** (it `serde_json::from_str`s any POST body);
- returns responses with **no CORS headers**.

**Impact.**

1. **Local read of everything (unconditional).** Any process running as the user — or any other
   local account that can reach `127.0.0.1:8765` — can read the entire meeting corpus with three
   unauthenticated POSTs. For an app whose whole value is private meeting memory, this is a direct
   confidentiality break. Note the data is served **raw**: the redaction firewall (which only wraps
   the cloud summarizer) does not apply here, so PII in notes/transcripts is exposed verbatim.
2. **Drive-by read from the browser via DNS rebinding.** A web page the user merely visits can
   rebind its own hostname to `127.0.0.1` and then issue a **same-origin** POST to the server.
   Because there is no `Host`/`Origin` validation and no CORS dependency once same-origin, the
   page can both send the request **and read the response** — exfiltrating transcripts to a remote
   attacker with no local foothold. (Without rebinding, a cross-origin "simple request" with a
   `text/plain` body still reaches and is processed by the server, but the browser blocks reading
   the response — so rebinding is what turns this from blind into full read.)

This also **contradicts the product docs**: `KILLER-FEATURES.md` presents the MCP server as
"Settings → Local MCP server", implying an opt-in control that does not exist in code.

**Remediation (in priority order).**

1. **Gate it behind an explicit opt-in setting, default off.** Add `mcp_enabled` to config and only
   `spawn` when true. Match the documented "Settings → Local MCP server" control.
2. **Require a per-install bearer token.** Generate a random secret on first enable, render it in
   the Claude Desktop config snippet, and reject any request whose `Authorization` header doesn't
   match (constant-time compare).
3. **Validate the `Host` header** is exactly `127.0.0.1:8765` or `localhost:8765`; reject otherwise.
   This is the standard, decisive anti-DNS-rebinding control for local HTTP servers.
4. **Reject requests carrying a browser `Origin` header** — legitimate MCP stdio/HTTP clients don't
   send one; a present `Origin` means a browser is calling.

Keep the existing localhost-only bind (`127.0.0.1`, not `0.0.0.0`) — that part is already correct
and prevents LAN exposure.

---

## M1 — Whisper model download has no integrity verification · **Medium**

**Where:** `src-tauri/src/transcribe/model.rs:112-147` (`download_model`).

**What.** `download_model` fetches the GGUF from
`https://huggingface.co/ggerganov/whisper.cpp/resolve/main/<file>`, checks only that the body is
non-empty, then renames it into place. There is **no SHA-256 / signature check** — integrity rests
entirely on TLS.

**Impact.** A compromised or substituted upstream file, a malicious mirror, or a MITM holding a
CA the OS trusts could deliver a crafted model. The file is then parsed by **whisper.cpp (C/C++)
via `whisper-rs`**; a malformed GGUF is a memory-safety attack surface in native code (the Rust
type system does not protect the C++ parser). Likelihood is low (HTTPS + reputable host), but the
blast radius (native-code parse of attacker-controlled bytes) makes it Medium.

**Remediation.** Pin an expected SHA-256 per `(size, language)` model file and verify the digest of
`<name>.part` before the rename; fail closed on mismatch. Optionally add a download timeout and a
sanity bound on size.

---

## M2 — No Content-Security-Policy on the webview · **Medium**

**Where:** `src-tauri/tauri.conf.json:22` → `"csp": null`.

**What.** The Tauri webview ships with **no CSP**. The app renders LLM- and transcript-derived
Markdown into the DOM (`src/app/shared/markdown.component.ts`: `marked.parse()` → `[innerHTML]`).

**Mitigation already present (important).** That render path binds via Angular's `[innerHTML]`,
which runs Angular's **DomSanitizer** (strips `<script>`, event handlers, `javascript:` URLs), and
the component deliberately uses **no `bypassSecurityTrust`**; the `[[wikilink]]` substitution also
strips `<>`. So the concrete XSS path is currently defended **by the framework**, not by the
platform.

**Why still Medium.** A null CSP removes the second layer that Tauri hardening guidance expects.
If any future component introduces a non-Angular sink (a `bypassSecurityTrustHtml`, a direct DOM
write, a third-party widget), it is immediately exploitable — and via the IPC bridge an injected
script can reach the exposed commands (including the arbitrary-write of **M3** and
`delete_meeting`). A strict CSP also blocks unexpected `connect-src`/remote loads.

**Remediation.** Set a strict CSP, e.g. `default-src 'self'; script-src 'self'; style-src 'self'
'unsafe-inline'; img-src 'self' asset: data:; connect-src 'self' ipc:` (tune to the asset protocol
+ IPC). Keep relying on the Angular sanitizer as the inner layer.

---

## M3 — Unconstrained `dest_path` in export commands · **Medium**

**Where:** `src-tauri/src/commands.rs:409-425` (`export_audio`), `:428-441` (`export_note`).

**What.** Both commands take a caller-supplied `dest_path: String` and `std::fs::copy` / `std::fs::write`
to it with no validation. In normal use the frontend supplies a path from a native save dialog —
but the command is directly invokable over the IPC bridge by **any code running in the webview**.

**Impact.** An arbitrary file-write/overwrite primitive (note markdown or audio bytes) bounded only
by the app process's filesystem permissions. On its own it requires control of the webview; combined
with **M2** (no CSP backstop) it is the kind of primitive an injected script would reach for. The
Tauri capability set does **not** otherwise expose a generic fs plugin to the webview (good — see
Verified-good), so these two commands are the notable exception.

**Remediation.** Constrain writes to a known-safe base (e.g. the dialog-returned directory passed
through opaquely), or perform the save-dialog selection inside the Rust command rather than trusting
a path string from the frontend. At minimum, reject paths that aren't absolute + canonicalized into
an allowed root.

---

## Low / informational

- **L1 — `vault_subfolder` path containment** (`export/obsidian.rs:121-126`). `write_note` does
  `vault_dir.join(subfolder)` where `subfolder` comes from user settings, with no `..`/containment
  check. Self-inflicted only (the user sets their own subfolder), so Low — but defense-in-depth:
  canonicalize the target and assert it stays under `vault_dir`. (Note the **LLM-chosen** folder from
  auto-organize *is* sanitized in `organize.rs::sanitize_folder`, and note **titles/entity names** are
  sanitized against path separators — only the manual subfolder setting is unchecked.)
- **L2 — AppleScript construction in `add_reminder`** (`commands.rs:637-639`). The reminder name is
  escaped `\` → `\\` then `"` → `\"` (correct order), which blocks string-literal breakout, so this is
  not an injection today. Residual: control chars/newlines aren't stripped (can break the command, not
  inject). Prefer passing the value via argv/stdin to `osascript`, or strip control chars. The
  Calendar script (`next_calendar_event`) is static — no injection.
- **L3 — MCP runs migrations per request** (`mcp.rs:120`). Each call does `Db::open` → `migrate()`
  (a `CREATE TABLE IF NOT EXISTS` write transaction). Harmless under WAL but it means an
  unauthenticated endpoint triggers writes; open read-only / skip migrate on the MCP path.
- **L4 — Transient WAV in temp** (`commands.rs:153`). System-audio capture writes
  `temp_dir()/meetnotes-sys-<uuid>.wav`. On macOS `$TMPDIR` is per-user (not world-readable), so fine
  for the target OS; flag it if the app is ever ported to Linux/Windows where `/tmp` is shared.
- **L5 — Post-rebrand naming** (`tauri.conf.json:5`, `secrets/keychain.rs:5`). Bundle identifier and
  Keychain service are still `com.meetnotes.app`, app-data dir `MeetNotes`. Not a vulnerability;
  noted for inventory/consistency (and because the Keychain service name is security-adjacent).

---

## Verified-good (controls confirmed present)

These were checked and found sound — worth recording so a future reviewer doesn't re-litigate them:

- **SQL injection:** none found. Every query uses `rusqlite` `?N` bound params; `LIKE` user input is
  escaped via `escape_like` with `ESCAPE '\'` (`storage/db.rs`). No string-built SQL anywhere.
- **Memory safety:** no `unsafe` blocks in the Rust core (only a test *name* matches "unsafe").
- **TLS:** `reqwest` uses `rustls-tls` (`Cargo.toml`); no `danger_accept_invalid_certs`, no plaintext
  endpoints for cloud calls.
- **Secrets:** the Anthropic key lives only in the macOS Keychain (`secrets/keychain.rs`); it is never
  logged, never written to the settings DB, and is explicitly excluded from the config DTO
  (`get_config` returns `AppConfigDto`, which has no key field). An empty key clears the entry.
- **Redaction firewall placement:** `make_provider` (`summarize/mod.rs:42-69`) wraps **only** the
  cloud `AnthropicProvider` in `RedactingProvider`, at the single seam every feature routes through,
  and the decorator covers **both** `summarize` and `complete`. Local providers (`claude_code`,
  `ollama`) get raw text but never leave the device. (Caveat: the firewall is regex-only — emails /
  cards / phones, not names — as the code comments honestly state, and it does **not** cover the MCP
  egress of finding H1.)
- **Subprocess spawns:** all use argv arrays, never a shell string — `claude` (hermetic:
  `--disallowedTools`, front-matter validation, PATH recovered once and cached), `ps -axo comm=`
  (static), `osascript` (see L2), the ScreenCaptureKit sidecar, and `kill <pid>`. No shell
  interpolation of untrusted input.
- **Webview rendering:** Markdown → HTML goes through Angular's `[innerHTML]` sanitizer with no
  `bypassSecurityTrust`; other surfaces (chat, recipes, digests, threads, search snippets) render via
  escaped interpolation, explicitly avoiding `innerHTML`/`DomSanitizer`.
- **Tauri capabilities:** `capabilities/default.json` is minimal — `core:default`, `dialog:allow-open`,
  and a few window controls. **No `fs`/`shell` plugin is exposed to the webview.** The asset protocol
  scope is narrowly limited to `…/MeetNotes/audio/**`.
- **Filesystem writes to the vault:** atomic (hidden `.tmp` dotfile → `fsync` → `rename`), with
  filename sanitization against path separators + reserved chars, and `symlink_metadata` for existence
  checks (no surprise symlink following). Idempotent re-export avoids clobbering user edits.
- **Network bind:** the MCP server binds `127.0.0.1` only (not `0.0.0.0`) — not LAN-exposed.

---

## Recommended order of work

1. **H1** — gate + token + `Host` check on the MCP server. (Blocks shipping; also reconciles the docs.)
2. **M3** then **M2** — constrain the export paths, then add a strict CSP (the two compound).
3. **M1** — pin model checksums.
4. **L1–L3** — containment check, osascript hardening, MCP read-only open.

> **Disclosure note:** H1 describes an unauthenticated local data-exfiltration path that is not yet
> fixed. If this repository is or becomes public, consider landing the H1 remediation before
> publishing this file, or keep the audit in a private location until then.
