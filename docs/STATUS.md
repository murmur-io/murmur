# Murmur — status

> **Updated 2026-08-28 · v2.0.0.**
>
> **The authoritative feature list is [`README.md` → Status](../README.md#%EF%B8%8F-status).** This
> file exists because several agent prompts and the `research` skill point at it, and because it used
> to be the single worst piece of documentation in the repo: until this rewrite it still said
> *"MeetNotes — Status. Updated 2026-06-24 … Phase 0 (skeleton), Phase 1 (3 AI providers) … 34
> tests"*, two months and a product rename out of date, while presenting itself as *"Authoritative
> current state."* Several agent definitions had already learned to distrust it by name.
>
> So: **this file does not maintain a second copy of the feature list.** It answers the two questions
> a status doc should answer and that the README deliberately does not — *how do I check?* and *what
> can a headless machine not prove?*

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
