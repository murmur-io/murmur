# Learnings — main-loop (the orchestrator)

Lessons for the top-level agent that plans, dispatches sub-agents/workflows, runs git + deploys, and
owns the "is it actually done" verdict. Per-agent implementation lessons live in the sibling files;
this file is the CROSS-CUTTING orchestration/git/deploy/crypto-process loop.

## Recurring patterns
<!-- Curated, binding. Keep ≤ ~20 bullets. -->

- **Verify the build YOURSELF before trusting a workflow's "PASS" — or before committing.** A
  workflow's verify phase can run on a mid-edit or half-finished tree and still report a
  structure-level PASS while `cargo test --lib` / `npx ng build` is actually RED. A green *unit* run
  proves nothing at a seam. Run the real gates on the FINAL tree with your own hands, every time.
- **A multi-agent split/refactor can leave the tree half-done — grep the RESULT, don't trust the
  report.** A build-phase agent that dies mid-response ("API Error: Connection closed") leaves new
  sub-components CREATED but NOT wired and the old markup NOT removed → the tree won't build, and a
  later phase's "done" report describes intent, not reality. Independently confirm: are the new
  components actually rendered? are the stale refs/imports gone? does it compile?
- **Shared working tree + concurrent sessions → reconcile STASH-SAFE, never destructively.**
  `git stash push -u` → `git pull --ff-only` → `git stash pop` catches the local branch up to origin
  without losing any session's uncommitted WIP (the stash is RETAINED on a pop conflict — nothing is
  lost). NEVER `reset --hard` / `clean -fd` a shared tree. Committing on a feature branch then
  `git checkout <trunk>` REVERTS the branch-committed files in the working tree (the dev app silently
  loses the feature) — restore them with `git checkout origin/<trunk> -- <files>` or the stash-safe
  pull. Stage ONLY your feature's files by explicit path; leave others' analytics/learnings/docs WIP
  untouched.
- **Audit before you conclude "did I destroy anything?"** — `git reflog` (any `reset`/`clean`?),
  `git status` (is the WIP still there?), and `git diff origin/<trunk> -- <file>` (already-merged dup
  vs genuine new WIP) answer it with evidence, not vibes.
- **No backticks in `git commit -m "…"`.** The shell runs `` `word` `` as command substitution and
  silently deletes those words from the message. Write the message backtick-free, or use `-F file` /
  a single-quoted heredoc.
- **Shut down background review/verify agents the moment you've consumed their verdict.** Idle
  agents emit `idle_notification` every few seconds, each re-firing your turn (token + attention
  drain). `SendMessage({to, message:{type:"shutdown_request", reason}})`.
- **Deploy → POLL until the new artifact is genuinely live before saying "test now".** A green build
  ≠ deployed; the old container keeps serving until the new one is up. Poll the concrete changed
  thing (a new static file 200s, a response header flips) — a background `until`-loop, then verify.
- **Cross-check crypto interop BYTE-EXACT before shipping.** For any cross-language crypto (Rust↔JS
  viewer), emit a vector from one side and derive/decrypt it on the other with the SAME primitives +
  params; a green unit test on ONE side is not interop proof. Prefer a fix that needs NO protocol/DB
  change (derive a value both sides already hold) over plumbing a new field through every layer.
- **A "shipped" feature can be structurally present but functionally DEAD** — a UI field on top of an
  unimplemented backend/viewer path silently produces broken output (undecryptable password links).
  Test the FULL user path end-to-end, or gate the UI off until the path works.
- **When a task turns out bigger/broken than asked, surface the honest scope + a decision, don't
  quietly half-build it.** (Password links were fundamentally incomplete, not a one-line bug — the
  right move was to say so + offer build-properly / disable / warn, and only then build.)
- **Present a research-backed design for a UI-critical rebuild BEFORE building it.** After a redesign
  missed, a Research→Design workflow + a short "approve this direction?" gate stopped a second miss.

## Run journal
<!-- Append-only, newest first. -->

### [2026-07-05 landing/API deploy] GitHub Pages + Railway custom-domain flow
- **Pattern:** The public launch path split across two hosts and two repos: landing lives in
  `murmur-io/murmur` GitHub Pages on `murmurnotes.io`, while the zero-knowledge relay lives in
  `murmur-io/murmur-server` Railway on `api.murmurnotes.io`. Both can look "configured" before their
  certificates are actually live: Railway needed the `_railway-verify.api` TXT plus CNAME, and GitHub
  Pages needed `landing/CNAME` in the workflow artifact plus a remove/re-add of the custom domain
  before certificate issuance moved from "does not exist yet" to issued.
- **Caught by:** operator (public DNS, Railway domain status, GitHub Pages API, `curl -I`, and
  `openssl s_client`).
- **Lesson:** Treat DNS, provider-domain status, and browser HTTPS as separate gates. For API, require
  `api CNAME -> k9sfnbwk.up.railway.app`, `_railway-verify.api` TXT, Railway `verified=true`, cert
  `VALID`, and `https://api.murmurnotes.io/healthz` 200. For landing, require apex GitHub A records,
  `www CNAME -> murmur-io.github.io`, `landing/CNAME`, Pages deploy green, Pages `https_enforced=true`,
  and cert SAN covering both `murmurnotes.io` and `www.murmurnotes.io`. Full runbook:
  `.codex/learnings/landing-api-deploy.md`.
- **Status:** success-pattern

### [2026-07-05 sharing session — #194/#195/#196 + murmur-server #4/#5] password links + Touch ID + detail redesign
- **Pattern:** (a) A `build-detail-redesign` workflow's Split phase agent DIED mid-response → the tree
  was left with panel components created-but-unwired + a hard syntax error in `share-panel` (backticks
  inside an HTML comment terminating the template literal); the workflow later "completed" and its
  verify verdict was on a non-building tree. (b) Committing the redesign on a branch then
  `git checkout murmur` REVERTED the whole redesign in the working tree (panels gone, dev app broke).
  (c) Password-protected link shares had been "shipped" for weeks but NEVER worked — the viewer's
  `argon2idBytes` was a throwing stub AND the Argon2id salt had no protocol field (random salt lost on
  seal). (d) A `git commit -m` with backticks mangled the message on #196.
- **Caught by:** operator (me, running `ng build`/`cargo test`/grep myself) + adversarial-verifier
  (redesign) + lock-security-reviewer (Touch-ID MK-at-rest, caught a logout clear-before-drop
  ordering) + CryptoReview (salt soundness) + the user (live "działa"/"dalej nie działa").
- **Lesson:** Verify the build yourself on the final tree; grep the result of a multi-agent refactor;
  reconcile a shared tree stash-safe + restore reverted files from `origin`; cross-check crypto
  byte-exact (Rust emit vector → Node/hash-wasm derive+decrypt) before deploy; poll the deploy until
  `/s/argon2.js` 200 + CSP flipped before "test now"; no backticks in `git commit -m`; shut down idle
  reviewer agents.
- **Status:** journal

### [2026-07-05 api.murmurnotes.io] Railway custom-domain cert stuck → defer, don't chase
- **Pattern:** A Railway custom domain (CNAME, DNS-only) stuck at `certificateStatus: VALIDATING_OWNERSHIP`
  for 40+ min TWICE (DNS correct + PROPAGATED) — a Railway/Let's-Encrypt flakiness. Chasing it (delete +
  re-add) changed the CNAME target and cost more DNS edits for zero gain.
- **Caught by:** operator (polling the Railway GraphQL `customDomain.status`).
- **Lesson:** A branded domain is polish; if the cert won't validate, DEFER (the `*.up.railway.app`
  URL works), delete the stuck domain so it's not "pending", and document Plan B (Cloudflare proxy +
  SSL Full, but disable Rocket-Loader/Email-Obfuscation or they break the strict-CSP viewer).
- **Status:** superseded by `2026-07-05 landing/API deploy` (final fix was CNAME + TXT +
  wait/poll; cert became `CERTIFICATE_STATUS_TYPE_VALID`)
