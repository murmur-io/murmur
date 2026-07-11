# Lock model — per-folder lock INVARIANTS (binding rules-for-Codex)

> Murmur's privacy promise is enforced here. These invariants are BINDING for ANY change that
> touches content reads, exports, encryption, the keychain, MCP, or the lock commands
> (`lock_folder`/`unlock_folder`/`unlock_meeting`/`relock_*`/`remove_lock`). Trust the code:
> every symbol below is cited `file:line` — confirm before relying on it.

---

## The two encryption layers (don't conflate them)

1. **Whole-DB SQLCipher (DEK).** The entire SQLite file is SQLCipher-encrypted with the DB DEK,
   released from the keychain at launch (`secrets::keychain::get_or_create_db_dek`,
   `secrets/keychain.rs:21`; opened via `Db::open_with_key`, `db.rs:91`). Protects data-at-rest
   when the app is closed. The DB is readable for the whole running session once opened.
2. **Per-folder content-key (CK), wrapped by the master KEK.** A folder lock adds a SECOND layer:
   an AES-256-GCM content key (CK) that encrypts the actual note/transcript/timeline/audio of the
   folder's meetings. The CK is wrapped by the master KEK (`state.master_kek`,
   `secrets/keychain.rs:47`), and the KEK is released only by a Touch ID prompt
   (`biometric.rs`) in a signed build. SQLCipher protects the file; CK/KEK protects sealed
   content even while the DB is open.

A locked folder's content must be unreadable even with the DB open and even to the app's own
read paths — until the session unlocks it.

## Seal = encrypt note + transcript segments + timeline + audio WAV, with VERIFY-BEFORE-DESTROY

`lock_folder` (`commands.rs:1731`) seals, under the folder's CK:
- the **note** markdown → `content_blob` per provider row (`db.seal_note`, `db.rs:1000`), then
  the plaintext markdown column is blanked + the vault `.md` deleted — but ONLY after the blob is
  verified decryptable (`commands.rs:1751`/`1768`);
- the **transcript** segments → `text_blob` and the **timeline** → `data_blob`
  (`seal_folder_extras` `commands.rs:2087`, `db.seal_timeline` `db.rs:1204`), plaintext blanked
  after verify;
- the **audio WAV at rest** → `<file>.enc` via `crypto::encrypt_file` (verify-before-destroy
  inside, `crypto.rs:50`), then the plaintext WAV is removed (`commands.rs:2123`).

INVARIANT: any new seal/at-rest-encrypt path MUST prove the ciphertext decrypts back
byte-identical BEFORE blanking/deleting the plaintext (round-trip test pattern:
`seal_transcript_timeline_round_trips_byte_identical`, `db.rs:2672`). Content is NEVER lost.

## Gate EVERY read — sealed-and-not-unlocked leaks NOTHING

- Detail / segments / timeline / audio commands check `meeting_is_unlocked(state, &meeting_id)?`
  (`commands.rs:2249`; sites at `478`, `1386`, `1443`) and return a masked DTO when locked:
  `locked: true`, title → "🔒 Locked", no note, no segments, no timeline, `audio_path: None`
  (`commands.rs:1431`/`1467`).
- MCP + graph + search route the session `unlocked` set through `visibility_clause`
  (`db.rs:1269`) via `search_visible` / `list_meetings_visible` / `get_note_if_visible` /
  `meeting_is_visible` / `list_entities_visible`. A sealed-not-unlocked meeting is INVISIBLE
  there too (`mcp.rs:18`/`206`).

INVARIANT (hard rule): a NEW content read OR export path MUST be gated by `meeting_is_unlocked`
(commands) or `visibility_clause` (db/MCP). An ungated read is a leak and fails review.

## The `convertFileSrc` / asset-path trap (a real leak that was closed)

The masked DTO sets `audio_path: None` ON PURPOSE. The FE feeds `audio_path` straight into Tauri's
`convertFileSrc` (the `asset:` protocol, scoped to the audio dir), which serves the file to the
webview WITHOUT passing through the `export_audio` command (`commands.rs:470`) or
`meeting_is_unlocked` — it is the ONE audio read path that bypasses the gate
(`commands.rs:1435-1442`, `1468`). Nulling the path in the masked DTO is what makes the gate cover
the asset protocol regardless of on-disk seal state, so a plaintext WAV that briefly survives in
the scoped dir (recorded into an already-sealed folder, or a crash window) can never be served to
a locked view. Do NOT hand the FE any on-disk path for a locked meeting; do NOT add a new
asset/`convertFileSrc` serve path that skips the gate.

## Unlock / relock / remove — reversible, biometric-gated

- `unlock_meeting` (`commands.rs:2055`) resolves the meeting's folder → biometric
  `unlock_folder` (`commands.rs:1793`): KEK → unwrap CK → decrypt transcript+timeline back into
  plaintext columns, materialize a playable WAV (decrypt `.enc` → file) for the SESSION, and add
  the folder id to the session unlock set. The DB is not re-exported.
- `relock_folder` (`commands.rs:1890`) / `relock_all` (`commands.rs:1910`, used by manual
  "Lock all" + screen-share auto-relock via `relock_all_inner` `commands.rs:1915`) re-blank the
  plaintext + drop the decrypted session WAV; the `.enc` + `*_blob` columns stay.
- `remove_lock` (`commands.rs:1950`) PERMANENTLY removes the lock: KEK → unwrap CK → decrypt every
  note/transcript/timeline/audio back to plaintext, re-export the `.md`. Never lose audio
  (`commands.rs:2034`).

## Keychain / dev hatches / identity

- The keychain service is `com.meetnotes.app` (`secrets/keychain.rs:5`). The app identifier
  `com.meetnotes.app` is IMMUTABLE — changing it breaks macOS TCC/permission and keychain-ACL
  continuity for every existing user. Never change it.
- `MURMUR_DEV_DEK` (`keychain.rs:26`) and `MURMUR_DEV_KEK` (`keychain.rs:51`) are DEBUG-ONLY
  escape hatches (fixed 64-hex keys) that avoid per-rebuilt-binary keychain re-prompts in dev.
  They MUST NOT be reachable in release builds and MUST NOT be logged. Touch ID + lock-at-rest +
  screen-share auto-relock only TRULY verify on a signed build (stable signature) — a dev/unsigned
  build degrades biometrics to `Ok(true)` (`biometric.rs:7`).

## No PII in logs (lock paths included)

Lock/unlock/seal code logs IDs, stages, counts — never note/transcript text, titles, keys, or
DEK/KEK/CK material. A debug log is not allowed to become the leak the seal prevents.

## The two hard rules (if you remember nothing else)

1. A new content read or export path **MUST be gated** (`meeting_is_unlocked` / `visibility_clause`).
2. Any new seal **MUST verify-before-destroy** (prove decryptable before blanking plaintext) and
   be reversible by the matching unseal.

Lock-touching changes are gated by the `lock-security-reviewer` agent — it is the required review
before merge, and it audits exactly these invariants.
