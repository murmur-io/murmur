<!-- Dreamed 2026-07-19 via /dreaming. Prototypes are vibe-prototypes (fake data, not production). -->
# Dream: Off the Record — the meeting you can keep to yourself

Four divergence lenses ran in parallel (SCAMPER on shipped features / cross-domain analogy raid /
only-Murmur-could / enemy-move + fresh-Apple-platform) → ~55 ideas. Filtered against the prior dream
journal so we don't re-dream **Receipts** (live contradiction/quote-back), **Re-Truth** (vault
self-heal), **The Whisper**, **CKEditor-adversarial**. One ridge won **independently in three of the
four lenses** (SCAMPER "off-the-record gesture" · only-Murmur "Seal-to-Confess" · enemy "Off-the-Record
Zone"). That convergence is the signal.

## The spark  (survivors — one line each)

1. **Off the Record** *(the winner — prototyped)* — What if, mid-recording, one gesture (or the spoken
   phrase *"Murmur, off the record"*, confirmed by your voiceprint) seals everything you say next into
   a **Touch-ID compartment** that never enters the shared note and — because ASR + seal both run
   on-device — **never leaves this Mac**? Your negotiation floor, your private read of the room, your
   intel: captured, useful to *you* live, and physically un-cloudable. A cloud bot streamed it to a
   server the instant it was said — "off the record" already left the building.
2. **The Provenance Gutter** *(git-blame for what was said)* — What if every line of a note carried a
   quiet gutter: who said it **out loud**, the wall-clock second, a tiny waveform, **play the real
   audio**, voiceprint confidence — the whole moat (voice + far-side + on-device + owned files) in one
   UI primitive. Every line becomes provable.
3. **Belief Autopsy** *(the bitemporal killer)* — After a project fails: *"walk me back — when did we
   first believe the thing that turned out false, and who introduced it?"* The brain reconstructs the
   exact meeting and speaker where the doomed assumption entered the record. Organizational memory as a
   forensic tool; only possible over one canonical bitemporal store keyed to voice events.

## The wide spread  (the graveyard — terse; the graveyard is the point)

**SCAMPER** · voice-instead-of-text note editing ("cut this, tighten it" spoken) · per-speaker owned
`.md` (each attendee's own reconstructed note) · **off-the-record gesture** ✅ · argue-with-the-graph
(delete a retrieval edge, re-answer) · git-blame for spoken decisions ✅ (→ Provenance Gutter) ·
spaced-repetition for decisions · always-listening ambient desk-brain · self-destruct/decay notes ·
MCP exposes *you* (your voice-verified track record as a tool) · Connections links *claims* not notes ·
delete the record screen (note just exists after) · delete the note (query-only meeting layer) · the
note interviews you to fill gaps.

**Cross-domain** · fog-of-war "Unspoken Map" (lit = actually said, grey = circled-but-never-voiced) ·
racing ghost-replay of your own recurring pitch · **voice dead-drop** (note sealed until your spoken
passphrase) · immune-memory antibody cards (broken-promise pattern flags on contact) · DJ speaker-stems
(solo/mute/crossfade Me vs Others) · EDM "drop detector" (prosody marks the real inflection) ·
compartments/need-to-know clearance on Ask · contradiction highlight reel (≈Receipts) · double-entry
reconcile spoken-vs-written commitments · **sleep consolidation pass** (Mac dreams overnight, promotes
durable facts, zero egress) · save-scum decision save-states (reload brain's as-known-then context) ·
director's commentary track (speak *over* a past meeting) · scouting-report dossier from far-side
words · ER triage chart on meeting entry · burn-after-reading spoken asides · trading order-book of
open commitments.

**Only-Murmur-could** · broken-promise ledger (≈Receipts consistency) · contradiction radar (≈Receipts)
· **Seal-to-Confess** ✅ (Touch-ID gut-reaction mid-call) · deniable meeting (no-bot transcript, subpoena
= ciphertext) · tone-drift timeline (is my boss souring on me) · verbatim quote vault ("prove they said
it") · therapy/medical/legal privilege un-cloudable mode · **belief autopsy** ✅ · the silent party (who
never voiced the objection) · sealed compartment brain-handoff on job change · voiceprint impostor alarm
· unsaid-aloud filter (spoken = load-bearing) · retroactive dual-consent unseal · dead-man's seal /
heir-voiceprint vault.

**Enemy + fresh-platform** · Silent Guest (detect the *other* side's cloud bot, flag it) · **Off-the-Record
Zone** ✅ · the Leak Ledger (signed on-device "0 bytes left this device" proof) · voiceprint-locked folder
· poison-pill self-sealing share · amnesia switch (surgical forget-person, verify-before-destroy) ·
deniable second brain (decoy fingerprint) · Apple **SpeechAnalyzer** as a zero-download ASR engine ·
Murmur as a **Siri verb** (App Intents + Foundation Models tool-calling over the gated brain) · **Live
Activity** talk-balance + one-tap off-record on the Dynamic-Island pill · Vision-framework whiteboard →
`[[wikilinks]]` · Focus-mode-aware auto-arm+lock · Journal-Suggestions "reflect on today's meetings" ·
Handoff a live transcript Mac→iPhone · Foundation-Models `@Generable` guided-generation notes.

## Why these survived  (atom + emotion)

- **Off the Record** — stands on **THE LOCK + VOICE + ON-DEVICE + FAR-SIDE CAPTURE** at once (four
  atoms in one gesture). Emotion: **relief with a shiver of power** — permission to think out loud in a
  live call without it ever existing "out there." Daily-use hook, not a once-a-quarter forensic tool.
  Won 3/4 lenses. The Leak Ledger folds in as its proof surface ("0 bytes ever left this device").
- **Provenance Gutter** — the single densest expression of the whole moat; emotion: **certainty**
  ("every line is provable, click and hear the real voice"). A cloud notetaker has one mixed stream,
  no clean far-side per-speaker audio, no local voiceprint → cannot manufacture line-level spoken
  provenance.
- **Belief Autopsy** — rides the **rarest** atom (bitemporal brain). Emotion: **sober revelation**. A
  question every team wants answered that literally no other tool can compute.

## Prototype(s)

`docs/dreams/prototypes/off-the-record/index.html` — self-contained HTML+CSS+vanilla-JS, Murmur design
tokens, no build/npm. A live **Customer Call — Acme Corp** recording surface (far-side captured, "no bot
joined"). Scripted call auto-streams Me/Others with voiceprint-provenance; three lines are the ones
you'd never want in a shared note (your floor price, "45 is real — push to 47 and stop", "their CTO is
leaving in Q3 — do NOT reference it"). Toggle **Go off the record** (or the spoken-phrase chip) flips
the whole stage into an orchid **SEALING** cast; those lines fall out of the public note into a
**Touch-ID sealed-asides** compartment on the right. A "Where every word goes" panel counts public vs
sealed; a **Leak Ledger** reads *"0 bytes of your sealed asides ever left this device"* with an
on-device ed25519 signature. Click **Touch ID to reveal** → fingerprint → the sealed line decrypts in
place. Fakes: the ASR, the voiceprint, the crypto, the biometric. Screenshots in `./shots/`
(01 full · 02 mid-seal · 03 Touch-ID modal over the sealed stage · 04 revealed secret).

## What it'd really take  (real seams + honest sizing + limits)

Grounded in the shipped lock model (`.claude/rules/lock-model.md`) — this is *the seal pattern applied
to a live sub-span of one meeting*, not a new crypto primitive.
- **Backend (Rust, M–L):** a new "sealed span" concept — mark `[startS,endS)` of a recording as
  off-record; those transcript segments seal under a per-folder (or a dedicated "asides") CK via the
  existing `seal_note`/`seal_timeline` **verify-before-destroy** path, and are **excluded from the
  note-gen input + the redaction-firewall egress** and from Shared-Brain sync. New Tauri command(s)
  (`mark_off_record_span` / `seal_asides` / `reveal_aside`) in `commands.rs` + `lib.rs` handler; gate
  every read of an aside through `meeting_is_unlocked`. **Must** route through
  `lock-security-reviewer`.
- **Voice trigger (M):** phrase-spotting "off the record / back on record" in the live ASR path
  (`transcribe/live.rs`) + a voiceprint check that it's the owner. A **push-to-seal** button/hotkey is
  the S-effort v1; the spoken phrase is the delight but needs live keyword-spotting.
- **FE (Angular, M):** the recording surface already got the drawer redesign (PR #377) — add the
  on-record/off-record mode, the orchid "sealing" state, the sealed-asides drawer + Touch-ID reveal,
  the leak-ledger chip. Signals-first, `IpcService`, new DTOs in `models.ts`.
- **Limits — needs a signed build on a real Mac:** Touch ID / KEK release, true on-device seal, and the
  "far side never knew" far-side-capture premise only *actually* verify signed (dev degrades biometrics
  to `Ok(true)`). The Leak-Ledger signature is real crypto but its trust story is a signed-build claim.
- **The Siri-verb / SpeechAnalyzer / Live-Activity platform ideas are separate, bigger swings** (new
  Apple frameworks; some are iOS/macOS-26-only) — parked as their own future dreams.

## Verdict  (accepted / rejected / iterate — filled after the user disposes)

_pending — awaiting the user's call._
