# Learnings — main-loop (the orchestrator)

Lessons for the top-level agent that plans, dispatches sub-agents/workflows, runs git + deploys, and
owns the "is it actually done" verdict. Per-agent implementation lessons live in the sibling files;
this file is the CROSS-CUTTING orchestration/git/deploy/crypto-process loop.

## Recurring patterns
<!-- Curated, binding. Keep ≤ ~20 bullets. -->

- **A hypothesis you cannot falsify is not a diagnosis — build the instrument first.** Three of six
  palette fixes rested on "`:modal` throws in the shipping WKWebView". Nobody could check, because
  nothing here could run JS in that engine. `scripts/wkwebview-probe` now can, and the first thing
  it did was DISPROVE that hypothesis in ten seconds (`:modal` does not throw; the cause was the
  wire contract). When a report will not reproduce in your tools, the next move is to get a tool
  that can see it — not a fourth variant of the same guess.
- **When a fix does not hold, change the LAYER you are looking at — do not iterate inside it.** Six
  rounds went to the Add-tile palette's positioning (teleport, non-transform layout, native
  `<dialog>`, fallbacks) and the palette was never the bug: the tile payload shipped snake_case
  while the FE read camelCase, so a tile threw while rendering and took the board with it. Two
  failed fixes in the same layer is the signal to move DOWN a layer — component → payload →
  contract — not to try a fourth variant of the same idea.
- **A user-reported failure against green gates means the ORACLE is wrong, not the code.** The
  correct first move is to fix the measurement, not to patch the implementation. On PR #566 I
  shipped three speculative fixes because my e2e ran in a newer engine than the app does; the bug
  was invisible to the suite by construction. If the report and the suite disagree, the suite is
  testing something other than the product — say so, and go make it testable.
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
- **EXACTLY ONE `cargo` process machine-wide, ever — concurrency is what freezes the Mac.** This
  repo's test binary statically links the always-compiled ML tree (candle/mistralrs/whisper); ONE
  such `cargo test --lib` link is a ~15-20 GB transient. Several builder/verifier agents (each
  running its own cargo on the shared 187 GB `CARGO_TARGET_DIR`) + your own runs = multiple giant
  links at once → macOS memory-compressor pins all 16 cores → the whole UI (even a new browser tab)
  freezes, with swap still 0 (it's compression, not pageout). After `pkill cargo/rustc` the machine
  is instantly 95% free — proving it's transient build peaks, not a leak. RULES: never run local
  cargo while a subagent might; serialize builders (build → verify while builder idle → merge →
  next); cap `CARGO_BUILD_JOBS=2 … -j2`; iterate with TARGETED filters (`cargo test --lib links` =
  full compile, subset run) not the full 1900-test suite; **let CI (remote, 0 local RAM) be the real
  full-gate.** Also: N divergent worktrees on ONE shared target THRASH (fingerprint miss → full ML
  rebuild each switch) — prune stale worktrees, keep local cargo in as few trees as possible. Deeper
  fix (needs a profile change): the default `debug=2` full debuginfo on every dependency is the
  amplifier — see the build-perf research memory / `[profile.dev.package."*"] debug=false`.
- **A multi-PR program under the RAM constraint = SEQUENTIAL build, CI-gated, not parallel-cargo.**
  The proven cadence (ran a 12-PR program clean): build ONE PR in a worktree → verify (adversarial +
  lock-security where the change touches seal/gate; verifiers are read-only or single-targeted-test,
  never a 2nd concurrent cargo) → push → **CI (GitHub, remote = 0 local RAM) runs the full ci.sh
  gate** → merge → remove the worktree → next. Overlap the NEXT builder's local compile with the
  PREVIOUS PR's remote CI (still ONE local cargo). Rebase each PR onto fresh trunk before push
  (merge-skew). Do NOT reach for a parallel-cargo Workflow — concurrent cargo freezes the Mac; a
  read-only MAPPING fan-out (no cargo) is the only safe parallelism.
- **Under session churn (model/effort switches, `/loop`), background tasks + subagents DIE silently.**
  Verifiers/builders go idle without delivering; a re-verify run is lost. RECOVERY: trust the
  FILESYSTEM, not chat — `git -C <wt> status/diff` (is the work there? is the tree byte-identical to
  the builder's report?), `.claude/tmp/<task>/*.json` verdict artifacts, and any scratch files a dead
  verifier left (its probe tests can be salvaged + run yourself to settle a verdict — that's how 2
  real leaks were caught after verifiers stalled). Respawn a FRESH agent rather than nudging a wedged
  one repeatedly. And SHUT DOWN each verify/build agent the moment you've consumed its verdict
  (idle-notification spam re-fires your turn every few seconds).
- **When a verifier FAILs a lock/seal change, apply its EXACT prescribed fix, then re-prove RED via
  Edit (not git-checkout) + request a NARROW re-review of just the delta.** Both real bugs this
  program (Accept deadlock, sealed-title leak) were caught by the lock/adversarial gate, fixed to the
  reviewer's prescription (mirror the proven sibling), regression-pinned RED→GREEN, and re-confirmed
  on the delta only — don't re-run the whole review.
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
  `.claude/learnings/landing-api-deploy.md`.
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

### [2026-07-10 brain-l4-live] Never run the MUTATING adversarial verifier concurrently with the READ-ONLY lock auditor
- **Pattern:** both verifiers dispatched in parallel on the same uncommitted diff; the adversarial's mutation-probe (removing a purge site, then restoring byte-identical) overlapped the lock reviewer's audit window — the lock reviewer saw the mutated tree, reported a "blocking leak" for code that was present before and after, and burned a FAIL verdict + an investigation on a concurrency artifact.
- **Caught by:** the lock reviewer's own tree-state warning (diff hunk count changed mid-audit) + a post-hoc grep/test on the settled tree.
- **Lesson:** patch-and-restore verifiers get EXCLUSIVE tree access. Run adversarial first, lock-security after (or vice versa) — never both at once on a dirty tree. If parallelism matters, give the mutating verifier a worktree.
- **Status:** journal

### [2026-08-03 model-picker] The harness does not run clippy — 20 verify rounds passed a lint error CI caught in a minute
- **Pattern:** `canonical_checks["rust-lib"]` is `cargo test --lib`, and nothing in the harness runs
  `clippy -D warnings`. Inserting a `#[test]` fn immediately before an existing one split that item
  from its attribute, so the pre-existing `#[test]` landed on the NEW function: it registered twice
  (cargo prints the same test name twice, "2 passed") while the old test silently lost its
  attribute. `cargo test --lib` cannot see this — `duplicate_macro_attributes` is a warning there.
  Twenty verify rounds, three PASS reviewers and ~2700 green tests all missed it; CI failed on the
  first run. The same insertion mistake happened THREE times in one session (`gateway.rs`,
  `summarize/mod.rs`, `gateway.rs` again).
- **Caught by:** GitHub Actions `rust lane` (`clippy --all-targets -- -D warnings`); once locally by
  a `function is never used` warning after the attribute was stolen from a live test.
- **Lesson:** After inserting any item before an existing `fn`, immediately check the NEXT item still
  owns its attributes and doc comment. Before pushing Rust, run
  `cargo clippy --lib --tests -- -D warnings` (NOT `--all-targets`, which the hook blocks and which
  thrashes the profile) — the harness will not do it for you. A test-count DROP after removing a
  duplicated attribute is not a lost test; the duplicate was registering one test twice.
- **Status:** journal — the durable fix is to add a `rust-clippy` canonical check to the harness
  (control-plane change, own PR).

### [2026-08-03 model-picker] A green test proves nothing until you have seen it go red
- **Pattern:** two assertions written as evidence were VACUOUS and passed with the bug restored.
  (a) `the_ledger_reports_exactly_what_the_wire_sends` compared `effective_model_requested` against a
  re-derivation of the same fallback — after the factory was routed through that function, the test
  called it on both sides: a tautology dressed as a test. (b) An e2e assertion that "the new engine's
  catalog still loads" switched to `claude_code`, which was ALREADY the selected engine, so its
  catalog had loaded before the code under test ever ran. Both read as sensible evidence.
- **Caught by:** deliberate RED checks — reverting the fix and confirming the intended assertion (and
  only it) fails.
- **Lesson:** RED-check every load-bearing assertion, and check WHAT failed, not just THAT something
  failed. Two smells: an assertion whose two sides call the same function, and a fixture that is
  valid under both branches of the rule being tested (a short model id cannot tell an argv rule from
  a JSON-body rule; the DEFAULT engine cannot test "the new engine's catalog loads").
- **Status:** journal

### [2026-08-03 model-picker] Harness proof gaps have no termination condition
- **Pattern:** after all three reviewers returned PASS with zero MAJOR findings, four consecutive
  rounds returned `NEEDS_EVIDENCE` with the proof-gap count going 8 → 6 → 7 → 7. Closing a gap
  creates a new claim ("this test proves X"), which the next round questions one link further out;
  round N+1 restated claims closed in round N-1. `commit` refuses anything but PASS, so the loop has
  no exit. Deterministic checks were green in every one of the ~20 rounds. Measured earlier on a
  216-review corpus: the generalist reviewer PASSes 26% of the time and ALL 106 non-PASS verdicts
  occurred with every deterministic gate green.
- **Caught by:** counting gaps per round instead of reading them one round at a time.
- **Lesson:** Distinguish "this is broken" (MAJOR — always fix; three of this session's eight MAJORs
  were regressions introduced by earlier fixes in the same task) from "this is not proven" (a gap —
  bounded value, unbounded supply). Track the gap COUNT across rounds; if it is not falling, the loop
  is not converging and the work should land through Track A with the residual gaps written into the
  PR body. `CLAUDE.md` is explicit that CI is the only merge authority.
- **Status:** journal — durable fix is a harness stop condition (all reviewers PASS + zero MAJOR ⇒
  gaps become advisory), own PR.

### [2026-08-03 model-picker] A literal control byte makes a .ts file binary, and reviewers get a blob
- **Pattern:** a regex character class was authored with the actual bytes (`\x00`, `\x1f`, `\x7f`)
  instead of escape text. Git classifies a file with a NUL in the first 8000 bytes as binary, so a
  production module deciding whether a stored model id is kept or cleared reached three reviewers as
  `GIT binary patch / literal 4955` — unreadable. `ng lint`, `ng build` and 434 e2e tests were green
  on it, because the code is identical either way.
- **Caught by:** the lock-security reviewer, who refused to audit what it could not read.
- **Lesson:** Write control characters as escapes (`\u0000`), never as the byte. The Write tool
  transmits typed escapes literally, so build such strings programmatically and verify afterwards
  (`[b for b in path.read_bytes() if b < 9 or 13 < b < 32 or b == 127]` must be empty).
- **Status:** journal

### [2026-08-03 model-picker] A stacked PR gets no CI, and retargeting alone does not start it
- **Pattern:** `.github/workflows/ci.yml` triggers on `pull_request: branches: [murmur]`. A PR based
  on another feature branch therefore runs NOTHING — it shows "no checks reported", which reads like
  "CI hasn't finished" rather than "CI never started". Retargeting the base to `murmur` does not fire
  a run either: `pull_request` defaults to `opened`/`synchronize`/`reopened`, and a base change is
  `edited`.
- **Caught by:** checking `gh pr checks` instead of assuming, then reading the workflow trigger.
- **Lesson:** Either base a stacked PR on `murmur` from the start (accepting that its diff shows the
  parent's commits until the parent merges — say so at the top of the body), or close+reopen after
  retargeting to fire `reopened`. Merge the parent with a MERGE commit, not a squash: squashing
  creates a new commit carrying content the child already has as its own commits.
- **Status:** journal

### [2026-08-03 deps] A dependabot PR gets no Actions secrets, so a secret-dependent lane makes it unmergeable forever
- **Pattern:** #526/#527/#528 all failed the rust lane identically while changing no Rust at all. The
  lane clones the PRIVATE sibling `murmur-server` for the `murmur-protocol` path dependency using
  `secrets.MURMUR_SERVER_DEPLOY_KEY`, and dependabot pull requests read a SEPARATE Dependabot secret
  store — repository Actions secrets are withheld. The step failed before any dependency was
  exercised, so the red said nothing about the bumps and no rerun could ever make it green.
- **Caught by:** noticing the SAME failure on three unrelated ecosystems, then reading the failing
  step instead of the PR titles.
- **Lesson:** Identical CI failure across PRs that share no content is an environment fact, not N
  defects — read the step, not the diff. Two ways out: mirror the secret into Settings -> Secrets ->
  **Dependabot** (a different tab from Actions; values cannot be read back, so it needs a FRESH
  read-only deploy key), or re-create the bumps on an ordinary branch, which does receive secrets,
  and close the originals as superseded. Never merge such a PR with `--admin` to "unblock" it: the
  bumps then land with zero verification, which is the opposite of what the red was hiding.
- **Status:** journal — resolved here by removing `.github/dependabot.yml` at the operator's request
  (security ALERTS are a repo setting and survive; only the weekly version-bump PRs stop).

### [2026-08-03 deps] Never write an action SHA you did not copy from somewhere
- **Pattern:** while re-creating the dependabot bumps by hand, the first attempt pinned
  `actions/setup-node` and `taiki-e/install-action` to SHAs that were INVENTED — plausible-looking
  40-hex strings that correspond to no commit. A pinned action resolving to nothing breaks CI for
  everyone, and the diff looks entirely normal.
- **Caught by:** self-review before commit; `gh pr diff <n>` then showed the real SHAs.
- **Lesson:** A SHA, a hash, a version, a line number and a file path are all values to be COPIED,
  never composed from memory. When re-creating someone else's change, take the literal `+` line from
  their diff (`gh pr diff <n>`), do not retype it.
- **Status:** journal

### [2026-08-03 cleanup] `git worktree remove` on a live harness task strands the task state
- **Pattern:** three abandoned task worktrees held 8.1 GB. Removing them with
  `git worktree remove --force` succeeded, and `agent-harness clean --abandon` then refused every
  one: "v2 task lost its worktree before a durable clean intent". The harness records the intent to
  clean BEFORE the bytes disappear, so removing the worktree first leaves the task permanently
  non-terminal, and a non-terminal task is what blocks primary-checkout branch surgery.
- **Caught by:** the harness refusing; recovery was `git worktree add --force <same path> <branch>`
  to recreate an empty worktree, then `clean --abandon`, which then archived and closed all three.
- **Lesson:** Reclaiming space from task worktrees is `scripts/h clean <id> --abandon`,
  never `git worktree remove`. `clean` refuses while ignored build bytes are unarchived — delete
  `target/`, `.angular/`, `dist/`, `test-results/` and any duplicated `src-tauri/binaries` first
  (all regenerable), then let it run. 8.1 GB -> 28 MB this way.
- **Status:** journal

### [2026-08-03 cleanup] `git diff` does not carry untracked files — listing them is not preserving them
- **Pattern:** archiving three doomed worktrees produced patches from `git diff HEAD` plus a
  MANIFEST of untracked paths. One of those paths was `src-tauri/src/connectors/fetch.rs` — a whole
  new source file that existed nowhere else. The manifest named it; the archive did not contain it.
  Deleting the worktree at that point would have destroyed it while the archive looked complete.
- **Caught by:** reading the manifest instead of trusting the line count of the patch.
- **Lesson:** When rescuing a worktree, COPY the untracked files, do not enumerate them
  (`git ls-files --others --exclude-standard` gives the list; the copy is a separate step). Verify
  the archive by listing what is actually in it, not by trusting that the step ran.
- **Status:** journal
