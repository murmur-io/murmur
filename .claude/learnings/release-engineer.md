# Learnings — release-engineer

## Recurring patterns
<!-- Curated, binding. Prepended to every dispatch. Keep ≤ ~20 bullets. -->

- **NOTARIZATION IS MANDATORY** — never ship signed-only. `xcrun notarytool submit <dmg>
  --keychain-profile murmur --wait` → `stapler staple` → `spctl` says *Notarized Developer ID*.
  A signed-but-un-notarized DMG is Gatekeeper-blocked on macOS 15 (v0.3.0/0.3.1 shipped un-notarized
  and blocked the user — never again).
- **Sign INSIDE-OUT, NEVER `--deep`.** `codesign --deep` skips the nested `Contents/Resources/`
  audio helpers → notarization comes back `Invalid`. Sign every `meetnotes-*` helper FIRST (glob
  them — a missed one like `meetnotes-calendar` = Invalid; read the `notarytool log` to find it),
  THEN seal the `.app` without `--deep`, THEN the DMG. Use `scripts/macos-sign-notarize.sh`.
  (`codesign --deep` is also blocked by `.claude/hooks/block-bash.sh`.)
- **Sign by IDENTITY HASH, not name** — the cert CN has a Polish `ń` ("Gawroński") so name matching
  fails "no identity found": `HASH=$(security find-identity … | grep 'Developer ID Application' |
  head -1 | awk '{print $2}')`.
- **NEVER run `security`/keychain CLI from the agent shell** — it can't surface the auth dialog →
  hangs → retries queue → runaway procs (the 2026-06-27 11-`security`-procs loop). Keychain ops
  needing auth (`unlock`, `add`, `notarytool store-credentials`) are run BY THE USER via `!`.
  (`pkill security` is fine; also enforced by `block-bash.sh`.) A locked login keychain also breaks
  `git`/`gh` push ("could not read Username") — the user unlocks, you retry.
- **PR-merge, never direct-push the trunk.** `git push origin murmur` is refused by
  `block-bash.sh`; land via `gh pr create --base murmur` → `gh pr merge`. `gh` active account =
  `JakubGawr`; commits authored ONLY by `JakubGawr <63911380+JakubGawr@users.noreply.github.com>`, NO AI co-author trailers.
- **Version bump touches four files in sync** — `package.json`, `src-tauri/tauri.conf.json`,
  `src-tauri/Cargo.toml`, then `cargo update -p murmur --precise <ver>` (Cargo.lock).
- **`MURMUR_DEV_DEK` vs release-keychain DEK collide on a shared DB** → the release build can't open
  a dev-keyed DB. Testing a release on a dev machine: restore the `.pre-encrypt.bak` first.
- **Startup must never hard-crash** on a keychain/DB failure — graceful dialog + clean exit; never
  reintroduce `init().expect()`/`.unwrap()` on the keychain-or-DB path. `com.meetnotes.app` immutable.

## Run journal
<!-- Append-only, newest first. -->

### [2026-07-02 seed] Distilled from CLAUDE.md release rules + memory
- **Pattern:** the 2026-06-27 notarization mess + the sign/keychain/push gotchas.
- **Caught by:** operator (seeding the loop).
- **Lesson:** the bullets above; full runbook = `.claude/skills/release-murmur/SKILL.md`.
- **Status:** distilled (2026-07-02)
