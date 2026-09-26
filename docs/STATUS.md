# Murmur — status

> **Updated 2026-08-28 · v2.0.0.**
>
> **This file is the authoritative shipped-feature list** (it moved here from the root `README.md`,
> which is now a short front door that links to the landing page and its docs). It exists because
> several agent prompts and the `research` skill point at it, and because it used to be the single
> worst piece of documentation in the repo: until the 2026-08 rewrite it still said *"MeetNotes —
> Status. Updated 2026-06-24 … Phase 0 (skeleton), Phase 1 (3 AI providers) … 34 tests"*, two months
> and a product rename out of date, while presenting itself as *"Authoritative current state."*
>
> It answers three questions: *what shipped?*, *how do I check?* and *what can a headless machine not
> prove?* User-facing copy lives on the landing page (`landing/index.html`) and in its docs
> (`landing/docs.html`); this file is the engineering record behind them.

## What has shipped

Murmur ships as a signed, notarized macOS app; the manifests and [GitHub releases](https://github.com/murmur-io/murmur/releases) are the version source of truth.

- The full record → transcribe → summarize pipeline, dual-stream capture, the conversation-first record
  screen with in-meeting `@brain` threads, and the floating recorder bar.
- The on-device brain (agentic tool-use loop + hybrid FTS/semantic/entity-graph retrieval), the `/brain`
  knowledge hub, Ask-Your-Vault, and the knowledge graph.
- **Universal document ingest** — drop in a PDF (scanned pages fall back to on-device Apple Vision OCR),
  a Word / PowerPoint / Excel file, an HTML page, Markdown/text, or an image, and it's extracted,
  chunked, and vector-indexed into the same brain, gated by the same per-folder lock.
- **Receipts** — every claim in a generated note that aligns to what was actually said carries a
  receipt chip that jumps you to the exact transcript segment (audio second, speaker, ASR-confidence)
  it came from; unsupported lines earn none, and a sealed meeting leaks no timing or speaker.
- **A self-building link graph** — notes, meetings, *and* imported documents are first-class
  `[[link]]` targets you pick/link/open from any surface; backlinks resolve by id, and the full-brain
  graph renders as a living neural map.
- **Workspaces** — one hierarchy for everything. The separate Meetings and Notes folder trees are
  gone; there is a single tree of **Workspaces › folders › your recordings and notes**, in one sidebar
  that collapses to a rail. Locking a workspace seals everything inside it.
- **Dashboards** — compose a board from your notes, recordings, documents, people, promise ledgers
  and reminders, plus pinned **living answers** (a question whose answer the app keeps up to date, and
  withholds the moment its sources stop being readable). Read a board through **Brief / Overview /
  Commitments / Sources / People** lenses, or ask it directly, grounded only in what's on it.
- **Imports** — Settings → Imports pulls in a **Notion export**, an **Obsidian vault**, or **Apple
  Notes**. Entirely offline: no API token, no account, no network call. Every import is a dry run
  first, reporting what it would write (new vs. already imported) before anything is written. Apple
  Notes asks macOS for permission on the first run.
- **Ask remembers** — vault, note and meeting conversations persist, each surface with its own history
  browser. A conversation disappears the instant any folder it drew on stops being readable.
- **One model picker** across every AI surface, always accepting a free-text model id — so a model
  released after this build is still selectable.
- **Notes** as a full standalone product — editor, the shared workspace/folder lock lifecycle, AI
  auto-organize, and the 19-action AI command menu.
- **Shared Brain** — free, opt-in, end-to-end-encrypted org sharing of notes and meetings, multi-org
  aware, auto-refreshing, with its own MCP tool, and **per-document permissions** (**View only** /
  **Can edit**, set by the document's author).
- **Tasks** — shared work inside a Shared Brain org: assignees, due dates, subtasks, and the same
  per-document permissions. Tasks belong to an org, so — like Shared Brain and link sharing — they
  require a signed-in account. Everything else in this list works with no account at all.
- The per-workspace / per-folder Touch ID lock model (two encryption layers, gated reads,
  verify-before-destroy seals), the content-free egress ledger, and the read-only MCP server.
- Per-note and per-meeting expiring E2EE link sharing.

## Honest gaps — not shipped, or only partially proven

The user-facing version of this list is the [Known limitations](https://murmurnotes.io/docs.html#known-limitations) docs page; keep the two in step.

- A retrieval router module exists in code but is explicitly shadow-mode — not yet wired into live
  dispatch. Treat it as internal plumbing, not a user-facing capability.
- The on-device reranker seam is wired but currently measured to add no retrieval-quality lift yet.
- Cloud transcription is **not** a shipped path — all transcription today is on-device
  (`whisper.cpp` / optional Parakeet); a cloud-ASR option exists only as a research note, not code.
- Live ScreenCaptureKit / Core Audio tap capture, the Touch ID prompt, and screen-share auto-relock can
  only be *fully* exercised on a signed build on a real Mac, and are documented as such rather than
  claimed from unit tests alone.
- A live, two-account, signed-build round-trip of Shared Brain sharing is still validated manually per
  release rather than by an automated headless test.

## How to check, rather than believe

Nothing here is worth trusting over the code. Three commands and one file:

```bash
.agents/h/mirror-check                    # 0.02 s — the cheapest check in the repo, run it always
( cd src-tauri && cargo test --lib )      # the Rust unit suite
npx ng lint && npx ng build               # the frontend gates
bash scripts/ci.sh                        # the full gate: clippy -D warnings + tests + lint + build + headless E2E
```

GitHub Actions running `scripts/ci.sh` is the **only** merge authority. A claim that is not covered
by one of those, or by a named oracle below, is a claim about intent.

Never pin a test count in documentation. The suite grows continuously, and every count written down
here has been wrong within a fortnight — which is exactly how this file came to claim "34 tests".

## What a headless machine cannot prove

These are real capabilities with real code behind them, and they still only *truly* verify on a
Developer-ID-signed build on a physical Mac. They are listed as user/runtime-gated rather than
claimed from unit tests:

| Capability | Why it can't be proven in CI |
| --- | --- |
| Touch ID unlock, lock-at-rest | The Keychain's `SecAccessControl` user-presence prompt needs a signed binary with a stable signature and a real GUI session. Debug builds can bypass it with the `MURMUR_DEV_KEK` hatch — convenient for iteration, not a security guarantee. |
| System-audio capture | A Core Audio process tap (macOS 14.4+) or the ScreenCaptureKit sidecar needs the Screen Recording permission and a desktop session. The graceful no-permission degrade IS unit-tested; the capture itself is not. |
| Screen-share auto-relock | Requires an actual screen-sharing session to detect. |
| Notarization + Gatekeeper | Needs the Apple account and the notary service. |
| A two-account Shared Brain round-trip | Validated manually per release; there is no automated headless test that runs two signed clients against the server. |
| Packaged-WebKit rendering | `ng serve` sends no `Content-Security-Policy` header at all, which is the only reason the 0.5.0 style-loss bug never reproduced in development. `e2e/render/csp-style-src.spec.ts` now supplies the header and reproduces it in both engines; `scripts/wkwebview-probe` executes JS inside the real shipping engine when a UI failure will not reproduce in Playwright. |

## The oracles

A bug class that reached a user is not closed until a deterministic check for it exists. The shipped
classes and the check that owns each:

| Bug class | Oracle |
| --- | --- |
| Seal destroys content | `src-tauri/src/storage/db_tests/lock_tests.rs::seal_transcript_timeline_round_trips_byte_identical` |
| Sealed content leaks through a read path | `src-tauri/src/commands/tests/lock_read_gate_tests.rs` |
| macOS FFI abort at launch | `scripts/harness-runtime-smoke.py` |
| Packaged-WebKit CSP style loss | `e2e/render/csp-style-src.spec.ts` (with a control that asserts the blocking really happens, so the guard cannot go vacuous) |
| An IPC DTO serialized in snake_case against a camelCase frontend | `src-tauri/src/commands/tests/dashboard_cmd_tests.rs` — the camelCase wire oracle |
| Developer vocabulary reaching a user-visible string | `scripts/check-vocabulary.mjs` |
| A product screenshot carrying real data | the privacy gate in `scripts/screenshots/capture.mjs`, which refuses the shot rather than writing it |

## Related documents

`docs/` mixes current reference with historical planning notes. See [`docs/README.md`](README.md) for
which is which before citing any of it.
