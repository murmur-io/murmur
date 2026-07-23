# Lock model — per-folder lock INVARIANTS (binding ruleset)

> Murmur's privacy promise is enforced here. These invariants are BINDING for ANY change that
> touches content reads, exports, encryption, the keychain, MCP, or the lock commands
> (`lock_folder`/`unlock_folder`/`unlock_meeting`/`relock_*`/`remove_lock`). Trust the code:
> anchors below are `file::symbol`; grep each symbol in the current tree before relying on it.

---

## The two encryption layers (don't conflate them)

1. **Whole-DB SQLCipher (DEK).** The entire SQLite file is SQLCipher-encrypted with the DB DEK,
   released from the keychain at launch (`secrets::keychain::get_or_create_db_dek`;
   opened via `storage/db.rs::Db::open_with_key`). Protects data-at-rest
   when the app is closed. The DB is readable for the whole running session once opened.
2. **Per-folder content-key (CK), wrapped by the master KEK.** A folder lock adds a SECOND layer:
   an AES-256-GCM content key (CK) that encrypts the actual note/transcript/timeline/audio of the
   folder's meetings. The CK is wrapped by the master KEK (`state.master_kek`,
   `secrets/keychain.rs::get_or_create_master_kek`), and the KEK is released only by a Touch ID prompt
   (`biometric.rs`) in a signed build. SQLCipher protects the file; CK/KEK protects sealed
   content even while the DB is open.

A locked folder's content must be unreadable even with the DB open and even to the app's own
read paths — until the session unlocks it.

## Seal = encrypt note + transcript segments + timeline + audio WAV, with VERIFY-BEFORE-DESTROY

`commands/lock.rs::lock_folder` seals, under the folder's CK:
- the **note** markdown → `content_blob` per provider row
  (`storage/seal_store.rs::Db::seal_note`), then
  the plaintext markdown column is blanked + the vault `.md` deleted — but ONLY after the blob is
  verified decryptable;
- the **transcript** segments → `text_blob` and the **timeline** → `data_blob`
  (`commands/mod.rs::seal_folder_extras`, `storage/seal_store.rs::Db::seal_timeline`), plaintext blanked
  after verify;
- the **audio WAV at rest** → `<file>.enc` via `crypto::encrypt_file` (verify-before-destroy
  inside), then the plaintext WAV is removed.

INVARIANT: any new seal/at-rest-encrypt path MUST prove the ciphertext decrypts back
byte-identical BEFORE blanking/deleting the plaintext (round-trip test pattern:
`storage/db_tests/lock_tests.rs::seal_transcript_timeline_round_trips_byte_identical`). Content is NEVER lost.

## Gate EVERY read — sealed-and-not-unlocked leaks NOTHING

- Detail / segments / timeline / audio commands check `meeting_is_unlocked(state, &meeting_id)?`
  (`commands/mod.rs::meeting_is_unlocked`; see `commands/meetings.rs::get_meeting_detail` and
  `commands/export.rs::export_audio`) and return a masked DTO when locked:
  `locked: true`, title → "🔒 Locked", no note, no segments, no timeline, `audio_path: None`
  (`commands/meetings.rs::get_meeting_detail`).
- MCP + graph + search route the session `unlocked` set through `visibility_clause`
  (`storage/db.rs::visibility_clause`) via `search_visible` / `list_meetings_visible` / `get_note_if_visible` /
  `meeting_is_visible` / `list_entities_visible`. A sealed-not-unlocked meeting is INVISIBLE
  there too (`src-tauri/src/mcp.rs`).

INVARIANT (hard rule): a NEW content read OR export path MUST be gated by `meeting_is_unlocked`
(commands) or `visibility_clause` (db/MCP). An ungated read is a leak and fails review.

## The `convertFileSrc` / asset-path trap (a real leak that was closed)

The masked DTO sets `audio_path: None` ON PURPOSE. The FE feeds `audio_path` straight into Tauri's
`convertFileSrc` (the `asset:` protocol, scoped to the audio dir), which serves the file to the
webview WITHOUT passing through the `export_audio` command (`commands/export.rs::export_audio`) or
`meeting_is_unlocked` — it is the ONE audio read path that bypasses the gate
(`commands/meetings.rs::get_meeting_detail`). Nulling the path in the masked DTO is what makes the gate cover
the asset protocol regardless of on-disk seal state, so a plaintext WAV that briefly survives in
the scoped dir (recorded into an already-sealed folder, or a crash window) can never be served to
a locked view. Do NOT hand the FE any on-disk path for a locked meeting; do NOT add a new
asset/`convertFileSrc` serve path that skips the gate.

## Unlock / relock / remove — reversible, biometric-gated

- `commands/lock.rs::unlock_meeting` resolves the meeting's folder → biometric
  `commands/lock.rs::unlock_folder`: KEK → unwrap CK → decrypt transcript+timeline back into
  plaintext columns, materialize a playable WAV (decrypt `.enc` → file) for the SESSION, and add
  the folder id to the session unlock set. The DB is not re-exported.
- `commands/lock.rs::relock_folder` / `commands/lock.rs::relock_all` (used by manual
  "Lock all" + screen-share auto-relock via `relock_all_inner`) re-blank the
  plaintext + drop the decrypted session WAV; the `.enc` + `*_blob` columns stay.
- `commands/lock.rs::remove_lock` PERMANENTLY removes the lock: KEK → unwrap CK → decrypt every
  note/transcript/timeline/audio back to plaintext, re-export the `.md`. Never lose audio
  (verify the matching decrypt branch in that symbol).

## Keychain / dev hatches / identity

- The keychain service is `com.meetnotes.app` (`secrets/keychain.rs::SERVICE`). The app identifier
  `com.meetnotes.app` is IMMUTABLE — changing it breaks macOS TCC/permission and keychain-ACL
  continuity for every existing user. Never change it.
- `MURMUR_DEV_DEK` and `MURMUR_DEV_KEK` (`secrets/keychain.rs`) are DEBUG-ONLY
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
