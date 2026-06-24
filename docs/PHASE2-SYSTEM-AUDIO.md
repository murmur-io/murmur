# Phase 2 — System-audio capture (design + handoff)

> **Status: NOT yet implemented.** This is the design for capturing the *other side*
> of a call (system audio) in addition to the mic. It is the single biggest remaining
> functional gap before MeetNotes is a real meeting recorder. It is documented here
> rather than coded blind because **system-audio capture cannot be verified in a
> headless build environment** — it needs an interactive macOS desktop, the Screen
> Recording permission, and live audio. Implement + verify it on a real Mac.

## Evidence (how Meetily does it — proven reference)
Inspecting `/Applications/meetily.app/Contents/MacOS/meetily`:
- `otool -L` links **ScreenCaptureKit**, AVFoundation, AVFAudio, AudioToolbox, CoreMedia, CoreAudio.
- `strings` shows `SCStream`, `SCStreamConfiguration`, `SCShareableContent`, `SCContentFilter`.
- Compiled-in Rust crates: **cpal 0.15** (mic) and **cidre** (Rust bindings to Apple frameworks incl. ScreenCaptureKit), plus the `objc2-*` family.

**Conclusion:** capture system audio via **ScreenCaptureKit (no virtual device)**, bound
from Rust via **`cidre`** — the exact approach Meetily ships. Copy that approach.

## Approach
- **System audio:** `SCStream` with audio capture (macOS 13+; prefer the 14.4+ audio
  path). From Rust use `cidre` (de-risked by Meetily). Alternatives: the
  `screencapturekit` crate or `objc2-screen-capture-kit`. Fallback if binding friction is
  high: a thin Swift sidecar over stdio.
- **Mic:** keep the existing `cpal` `Recorder` (Phase 0).
- **Mix:** resample both sources to 16 kHz mono → sum with a clipping guard → one WAV
  (reuse `audio/wav.rs`). Keep an optional dual-track mode behind a flag for future
  diarization.
- **Clock drift:** SCStream and cpal have independent clocks; align by timestamps,
  pad/drop to stay in sync over long meetings.

## Permissions
Screen Recording (TCC) is required for `SCStream` audio, plus Microphone
(`NSMicrophoneUsageDescription` already in `Info.plist`). Requesting `SCShareableContent`
triggers the Screen-Recording prompt; if denied, the system track is silently empty — so
detect authorization and route the user to System Settings → Privacy with onboarding copy.

## Interface (slots into `src-tauri/src/audio/`)
- `SystemAudioRecorder { start() -> Result<()>; stop() -> Result<Vec<f32>>; level() -> f32 }`
  mirroring the cpal `Recorder` (the `TODO(phase2)` hook in `audio/recorder.rs` marks where).
- `audio/mixer.rs`: combine mic + system sample streams → 16 kHz mono → WAV.
- `pipeline.rs`: on Stop, capture mic+system concurrently → mix → existing
  transcribe → summarize → export (downstream unchanged).

## Verification plan (must run on a real Mac)
1. Grant Screen Recording + Microphone permissions on first run.
2. Play known audio (e.g. a short video/call) while recording; speak into the mic.
3. Confirm the mixed WAV contains BOTH sources and the transcript reflects both sides.
4. Extend `scripts/e2e-core.sh` with a system-audio variant once capture works.

## Top risks + mitigations
1. `cidre`/SCStream async sample-buffer callback bridged into Rust → use Meetily as the
   reference; Swift sidecar fallback.
2. Screen-Recording permission UX (no prompt ⇒ silent empty track) → explicit auth check
   + onboarding.
3. Clock drift mic-vs-system on long meetings → timestamp-based alignment in the mixer.
