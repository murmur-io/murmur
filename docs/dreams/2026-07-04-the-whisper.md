<!-- Dreamed 2026-07-04 via /dreaming. Prototypes are vibe-prototypes (fake data, not production). -->
# Dream: The Whisper — your memory in the room

Four dreamers were dispatched in parallel, each jammed in a different lens (SCAMPER /
cross-domain analogy raid / "only-Murmur impossibility" / adversary + platform-scout).
~50 raw sparks came back. Three survived. One got a clickable prototype.

## The spark (survivors — one line each)

1. **The Whisper** — *what if, mid-call, Murmur privately whispers to YOU alone — "he's
   contradicting what he told you on Mar 3" — pulled live from the bot-free far-side audio
   against your on-device cross-meeting memory, invisible to the other party.* ⭐ prototyped.
2. **Under Oath** — *what if every quoted line in the .md is a voiceprint-signed affidavit —
   line-level "git blame" showing who actually SPOKE it, exportable as a tamper-evident
   sworn record.*
3. **The Ghost** — *what if the graph's nodes are VOICES, not names — the same unknown
   voiceprint clusters across "different" calls under three different names, and Murmur
   quietly flags it.*

## The wide spread (the graveyard is the point)

**SCAMPER lens** — pre-brief note that exists BEFORE the meeting (already ~half-built as
`pre_meeting_brief`); voice-biometric folder unlock (speak the passphrase); voiceprint-signed
affidavit per quote →**#2**; no record button, always-on rolling buffer you "keep" after the
fact; far-side builds a private dossier of the counterparty from their own words; live
consistency auditor →**#1**; lock as a time-capsule / dead-man's switch; entity-graph nodes are
voices →**#3**; note "git blame" per sentence →**#2**; the vault interviews you (hands you the
next question to ask someone); you record a voiceprinted stand-in of yourself for a meeting you skip.

**Cross-domain raid** — espionage dead-drop (decrypts when the target's voice is heard live);
medical differential (every meeting where this person argued the opposite) →**#1**; biology
sharp-wave-ripple **nightly memory consolidation** ("the brain sleeps and gets smarter by
morning"); immune "antibodies" to a person's commitment-that-always-slips; courtroom chain-of-
custody sworn record →**#2**; legal privilege folder = structurally undiscoverable until a
Touch-ID waiver; DJ **stems / solo one voiceprint** and re-export a note from only their track;
roguelike **fog-of-war brain map** lit only by what you've spoken aloud; save-state/permadeath
CRM snapshot; film montage supercut of one person on one topic in their real voice; geology
"core sample" through one relationship (tone drift over months); poker **tell** from far-side
prosody; cooking fermentation (a raw note re-links itself while you ignore it); espionage burn-
notice one-time voice-gated share.

**Only-Murmur impossibility** — spoken-under-oath seal →**#2**; folder sealed to a VOICE not a
finger; live private contradiction whisper →**#1**; dead-man's switch meeting; a portable .md
that decrypts only when THEIR voice reads the opening line; same unknown voice across renamed
identities →**#3**; retroactive self-rewrite (rename a project → brain rethreads every past
.md's [[wikilinks]]); **Privacy Receipt** (attest zero bytes left); zero-knowledge "we both
heard him commit to X" between two Murmur users; on-device "tell" model of one counterparty;
born-unforgeable note (voiceprint-signed hash at write-time); "when did each person first lie
to me?" over years, physically un-leakable.

**Adversary + platform scout** — court-grade provenance vs hearsay →**#2**; "forget this person"
as a verify-before-destroy *shred* mechanic; hotword 30s off-the-record (drop pre-persist);
**counter-surveillance** (detect via ScreenCaptureKit when a rival note-bot joined and is
recording YOU); **Consent Passport** (the far side's own voice saying "yes, record",
voiceprinted + sealed); duress decoy Touch-ID vault; share-the-answer-not-the-file (zero-
knowledge Q&A your model answers, transcript never egresses). Platform [B]: Apple **Foundation
Models** on-device SummarizerProvider (no model download, WWDC26 session 339); **multimodal**
capture of the slide they SHARED into the note; **App Intents → Siri** over the sealed store;
dual-engine transcript (Apple SpeechTranscriber ✕ whisper.cpp cross-check flags low-confidence
spans).

## Why these survived

- **#1 The Whisper** — stands on **far-side capture (#6) + the brain (#5) + on-device (#2)**.
  It's the one idea that made three of four dreamers converge independently. A cloud notetaker
  without a bot literally *cannot hear the far side*, and can't hold your private cross-meeting
  memory locally in real time. The feeling: **a private earpiece from your own past** — uncanny,
  a little forbidden, exactly the "lean-forward" a negotiator/founder/lawyer feels.
- **#2 Under Oath** — stands on **voice-verified provenance (#1) + the lock (#3) + owned files
  (#4)**. This is our *central* moat per [[deep-analysis-v3-2026-07-03]] (voice-verified,
  biometric-locked provenance). The feeling: **receipts nobody can gaslight away.**
- **#3 The Ghost** — stands on **on-device voiceprint (#1) + the brain (#5)**. Cross-meeting
  voiceprint clustering of *anonymous* speakers is biometric identity only we hold locally. The
  feeling: **catching a ghost you'd never consciously have noticed.**

Everything else died so these could breathe — the point of a wide spread. Many (nightly
consolidation, Privacy Receipt, Consent Passport, counter-surveillance, Apple Foundation Models
provider) are strong enough to be re-dreamed or fast-followed later.

## Prototype

`docs/dreams/prototypes/the-whisper/` — a self-contained autoplay HTML vibe-prototype (no build,
no deps, Murmur tokens over the "Sonora" demo world). It fakes a live call: Marcus (far side)
soft-pedals three commitments; the on-device co-pilot rail privately raises **Contradiction /
Recall / Context** cards, each with the past quote, a `[[wikilink]]` source, a voiceprint-verified
badge (`🎙 Marcus · 98%`), and a `▶ 2s` clip — while the footer ledger reads **"0 bytes left this
Mac."** A `👁 What Marcus sees` peek proves the other side just sees a normal chat. Screenshot:
`docs/dreams/prototypes/the-whisper/preview.png`.

## What it'd really take (honest)

**#1 The Whisper** — **L**. Real seams: a new async command streaming live far-side segments
into an on-device retrieval pass over the brain (FTS+vectors+facts) with a *contradiction*
detector; `ipc.service.ts` `onWhisper` event stream → a signal store; a `WhisperCard` DTO in
`models.ts`; an FE rail on the record screen. Hard parts are real: low-latency on-device retrieval
+ a good-enough contradiction signal without cloud, and it only *truly* verifies on a **signed
build on a real Mac** (live ScreenCaptureKit far-side + voiceprint) — headless can't prove the
latency or the match. Privacy: never-persisted, zero-egress by construction (that's the whole point).

**#2 Under Oath** — **M**. Provenance already flows partly (segments carry speaker + timestamps;
voiceprint identity exists, dormant). Needs: per-utterance voiceprint-confidence stamped into the
note model + a hover/`git-blame` FE affordance + a "sworn export" that hashes+signs. Lock-security
review required (new at-rest artifact).

**#3 The Ghost** — **M/L**. Needs anonymous-voiceprint clustering across meetings (the voiceprint
layer exists but is 1:1-oriented) + a graph node kind for "voice" + the flag UI. Real-Mac verify.

## Verdict

_(to be filled after the user disposes — accepted / rejected / iterate)_
