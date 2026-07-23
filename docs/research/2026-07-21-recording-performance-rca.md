# Recording performance/RAM RCA

Status: integrated implementation; the complete canonical CI gate is green as of 2026-07-23. The
installed DMG and release data were not touched. Independent exact-tree reviews and a future signed
build recording soak remain separate from the headless acceptance evidence below.

## Executive result

The regression is real and has several additive causes. It is not explained by one frontend leak.

1. The production recorder retained the complete native-rate microphone stream in a growing
   `Vec<f32>`. Four hours of mono f32 audio is about 2.58 GiB at 48 kHz, 5.15 GiB at 96 kHz, and
   10.30 GiB at 192 kHz, before Stop-time copies.
2. Stop materialized additional full-duration native and resampled buffers. A long/high-rate
   meeting could therefore transiently hold two or more duration-sized copies.
3. The live-bullets path could resolve the local GGUF Brain during Record even when Brain Live was
   off. The separate helper then added roughly the model's multi-gigabyte fixed residency and
   competed with Whisper for unified memory/Metal bandwidth.
4. Brain-v3 repair, audit, memory, embedding, NER, reranking, and brief jobs did not all share one
   recording-priority admission boundary. They could load another local model while live ASR was
   resident or while batch ASR was starting.
5. The system-audio helpers passed some multichannel ScreenCaptureKit layouts through generic
   conversion rather than explicitly averaging every channel to mono. That made conversion work
   layout-dependent and could produce incorrect 9-channel output.
6. The development workflow could leave a Rust library-test process alive after an agent timeout
   and could start several Cargo checks concurrently. That explains the separate screenshot with
   `meetnotes_lib-<hash>` at roughly 1118% CPU.
7. Helper crash handling had two unsafe gaps: a cross-launch `ps` snapshot followed by `kill(pid)`
   could hit a reused PID, while the Brain's idle timer deliberately did nothing during a wedged
   generation. Legacy audio recovery could also rename/read a scratch inode that a survivor still
   had open.

The Activity Monitor shape — about 14.38 GiB in Murmur plus 3.59 GiB in the Brain helper — is
consistent with duration-sized audio, Stop copies, in-process Whisper/Candle residency, Metal
allocator high-water, and a hidden GGUF child co-residing. It does not require a single 18 GiB leak.

The history narrows the regression claim: the duration-sized mic `Vec<f32>` is older than Brain v3
(it exists back to the original Phase-0 recorder, and the later crash-spill work mirrored it without
retiring the RAM copy). Brain-v3/L4 then added live-bullets/reaction paths and the killable local GGUF
helper. In other words, Brain v3 amplified a latent unbounded recorder until ordinary meetings crossed
the machine-pressure threshold; it was not the sole origin of every retained byte.

## The two screenshots mean different things

### `Murmur` + `meetnotes-brain`

This is production-app pressure. `meetnotes-brain` is the killable local GGUF helper. Starting it
while Brain Live is disabled is a bug; keeping it beside live Whisper also increases memory-bandwidth
contention even if its RSS is flat.

The helper is renamed to `murmur-brain`. The old name remains only in legacy orphan detection and
measurement compatibility; it is never resolved or spawned by a new build.

### `meetnotes_lib-<hash>`

This is not a Murmur runtime process and must not be renamed. Cargo names the Rust library
`meetnotes_lib`; `cargo test --lib` produces a hashed test executable with that name. The observed
34 threads and >1000% CPU are the test harness/native ML tree running in parallel, most likely left
alive or duplicated by the agent workflow. The workflow repair serializes Cargo, caps build/test
parallelism, gives one wall deadline, and terminates the entire spawned process group on timeout,
signal, or runner exit.

A repo-wide resource lane uses one Git-common-dir `flock`, defaults to `CARGO_BUILD_JOBS=2`,
`RUST_TEST_THREADS=1`, `RAYON_NUM_THREADS=2`, `OMP_NUM_THREADS=1`, and
`VECLIB_MAXIMUM_THREADS=1`, gives the lane wait plus child one aggregate deadline, and retains the
lock while its exact process group is terminated and reaped. Both standalone Claude and Codex hooks
deny direct Cargo/rustc, Tauri/dev/build/full-CI commands, including common shell/npm indirection,
and point agents to the supervised runner. Its supervisor, hook, config-audit, harness, evaluator,
and remote-policy selftests are part of the canonical gate rather than an optional local check.

## RAM math and the repaired capture contract

Old microphone allocation, excluding Vec capacity growth and copies:

```text
bytes = sample_rate * 4 bytes * duration_seconds

48,000 * 4 * 14,400 = 2,764,800,000 bytes = 2.58 GiB
96,000 * 4 * 14,400 = 5,529,600,000 bytes = 5.15 GiB
192,000 * 4 * 14,400 = 11,059,200,000 bytes = 10.30 GiB
```

The repaired contract is independent of meeting duration and device rate:

- the realtime callback writes into one preallocated 32 MiB atomic ring, retaining the bounded
  14-second live-caption history while older verified frames are recycled;
- it allocates no buffer, performs no file I/O, and takes no mutex;
- a non-realtime spool copies bounded absolute windows into a create-new raw f32 file;
- the spool fsyncs and commits an exact `(generation, inode, frames, bytes, SHA-256 prefix)`
  checkpoint in SQLCipher before advancing the ring base;
- the realtime callback never waits for the checkpoint thread; exact readers reject only when a
  verified base advance actually recycled their requested prefix;
- disk/device failure latches a typed terminal capture fault and automatically finalizes the exact
  durable prefix; there is no growing emergency-memory fallback;
- Stop, retry, mixing, archive generation, and ASR read bounded windows rather than collecting the
  whole meeting;
- classic RIFF is used only while its 32-bit size fields fit; longer/high-rate masters use RF64 with
  an exact `ds64` header.

This moves native-rate growth from RAM to private scratch disk; it does not make the bytes vanish.
The nonterminal workspace is identity-checked at mode `0700`, and every Rust-created plaintext
recording artifact is forced to mode `0600` independently of the process umask.
At the four-hour ceiling the raw mono f32 source needs the same approximate 2.58/5.15/10.30 GiB at
48/96/192 kHz. A write/fsync failure, including disk-full, therefore latches a typed fault and
finalizes only the verified durable prefix. It must never fall back to accumulating the remainder
in memory.

Crash recovery claims the expired affine generation lease. It accepts only the committed prefix of
the same regular single-link inode and matching hash. An uncommitted tail is truncated only after
the committed prefix has been authenticated; ambiguity preserves the row and file and marks the
meeting recoverable/Error rather than guessing or deleting.

## Model-residency contract

Recording owns an affine session token with explicit phases:

```text
Starting -> Live -> Draining -> Postprocess -> Finished
```

Starting installs priority before capture side effects. Existing local work must quiesce, the
killable Brain child is reaped, and e5/NER/reasoner caches are evicted again after the drain. While
Live, unscoped background model loads fail closed. Live ASR resolves to exactly one Whisper runtime;
the optional Parakeet path is not allowed to load beside the Whisper handle still required by wake
and manual capture. Stop removes the live capture owner, drains its exact model generation, and only
then admits batch ASR/postprocess. The final boundary releases e5, NER, the host reasoner cache, and
the Brain child before background jobs are reopened.

Every actual e5, NER, reranker, local reasoner, local Ollama, and sidecar forward must enter the same
admission seam. Leases cover native inference, not deterministic DB work or cloud/network waits.
Background outputs carry a monotonic epoch; the final write is atomically rejected if Record began
after dispatch.

Recording priority intentionally wins over optional local Brain reactions while live Whisper owns
the resident lane. A cold recording with Brain Live off must never launch or refresh
`murmur-brain`.

### Helper lifetime and crash-recovery contract

Cross-launch process observation is detection-only. A `ps` row followed by `kill(pid)` cannot bind
the signal to the observed process generation on Darwin; even `(pid,pidversion)` is not durable
across reboot without a boot-session binding. Murmur therefore never signals or authorizes scratch
deletion from that snapshot. Any live/orphan/ambiguous helper defers recovery and a new Start before
capture side effects.

New helpers instead own their own exact lifetime proof. `murmur-brain` watches the sole
parent-owned stdin pipe with Darwin `kqueue` (`EVFILT_READ | EV_CLEAR`) and exits only on `EV_EOF`:
protocol bytes remain owned by the NDJSON reader, while sole-writer close terminates the exact child
even during synchronous wedged inference. The watcher is installed and fd 0 is verified as a
pipe/socket before Tokio or the multi-GB model is loaded.
The three Swift capture helpers use the same inherited-pipe principle, while normal Stop keeps the
writer open through TERM, WAV finalization, and `wait`; the pipe is only a crash/parent-death path.
Parent-loss stop work is serialized with each helper's signal/rebuild queue, and an independent
five-second wall-clock `_exit(6)` bounds a wedged finalizer. Rust adopts a verified WAV only after
exit 0 or the ready-phase I/O-fault exit 5; pre-ready exit 3, hard-bound exit 6, signals, and unknown
status are not finalization proof even if the file happens to parse. The older four-hour helper wall
cap remains defense in depth.

Startup recovery first publishes SQLCipher ownership and reads only sidecar/scratch metadata in a
non-consuming preflight, then performs helper detection before any claim, rename, audio-content
read, reconcile, sweep, or heavy salvage. If a legacy helper may still hold the scratch inode open,
every durable marker and artifact is left untouched for a later clean launch. This prevents both
consuming a still-growing WAV and running salvage ASR beside a surviving Brain/capture process.

### Live-caption CPU contract

The live loop no longer performs a 14-second copy and Whisper decode on every timer tick. With the
optional Silero gate available it first snapshots only the exact unseen native-rate span plus two
seconds of overlap, outside the recorder mutex. An absolute source-frame cursor proves continuity;
trim contention, a cursor gap, or VAD failure is uncertainty and therefore fails open to ASR rather
than being mistaken for silence. Only a speech/hangover verdict materializes the full bounded tail.

The first tick after model load scans the full retained history so speech captured during model
startup is not skipped. Stop changes the affine phase and wakes the loop through a condition
variable; the loop checks that phase both before copying the full tail and immediately before native
ASR. This removes the former uninterruptible 3/6/9-second sleep and prevents a stale decode from
holding the Draining barrier.

### Background index/model consistency

Embedder selection now pins one exact real model handle for every sub-batch. Selection and vector
partition invalidation are one guarded operation; chunks/FTS remain canonical while only model-bound
vectors are rebuilt. Recording/background work cannot silently fall back to a stub embedding and
persist it as real output. NER model instances are process-global per model directory and are
evicted at recording boundaries instead of being independently loaded by concurrent features.

Org background sync uses membership plus a strictly-newer sequence claim in the same transaction as
each live/tombstone/terminal mutation. Leaving an Org deletes membership and purges its local
plaintext/chunks/vectors atomically; a late worker or local owner refresh cannot resurrect content.
A fresh scoped lock-security review passed this client invariant. Two older protocol/server issues —
tombstones that do not receive a new server sequence and the generic empty-AAD compatibility
fallback — were confirmed pre-existing and are tracked separately rather than being disguised as
part of this performance patch.

## File encryption and long recordings: explicit follow-up

Whole-file `read`/`write` in the existing folder-lock audio path can recreate an O(duration) peak
during seal/unseal. That is real compatibility debt, but it is not the cause of the observed
recording-time slope and it is **not changed by this product patch**.

A versioned, segmented AES-256-GCM prototype was built in a separate worktree. Repeated lock review
found subtle publish, rollback, vault-export, permanent-unseal, and exceptional-cleanup hazards.
The prototype was therefore rejected and is not present in this integration. The shipped one-shot
AES-GCM format remains authenticated but O(file size) in memory. Bounded lock/unlock needs its own
focused design with durable cleanup intent and a clean lock-security verdict; no hand-written
GHASH/CTR compatibility shortcut is acceptable.

## Repeatable measurement workflow

Use the signed app for the acceptance number; a dev process includes build/debug overhead.

```bash
INTERVAL=5 bash scripts/measure-recording-ram.sh /tmp/murmur-recording.log
```

The script records RSS, Activity-Monitor-style physical footprint, CPU, thread count, compressor
size, and swap for Murmur plus both current and legacy Brain helper names. It refuses ambiguous
multiple PIDs unless `MURMUR_PID`/`BRAIN_PID` is supplied. It logs no audio, transcript, titles,
paths, or other content.

Protocol:

1. Cold-launch one signed Murmur process and sample 60 seconds idle.
2. Disable Brain Live and record at least 30 minutes; mark the Record timestamp.
3. Stop and sample through the full note pipeline plus another five minutes.
4. Repeat three Record/Stop cycles in the same process.
5. Repeat once with Brain Live enabled, treating its explicit feature cost separately.
6. Inspect footprint slope, compressor/swap, child launch times, and Stop-correlated peaks together.

Optional thermal evidence, run separately by the operator because it requires sudo:

```bash
sudo powermetrics --samplers gpu_power,thermal -i 1000 -n 120
```

## Acceptance bar

- Brain Live off: a cold Record never launches or refreshes `murmur-brain`.
- Healthy meeting capture keeps the microphone ring at 32 MiB at 48/96/192/384 kHz; the standby
  wake listener uses a separate 8 MiB short-window ring.
- Main-process footprint has no duration-proportional audio slope after fixed model/allocator
  high-water is reached.
- Stop has no duration-proportional native-audio copy peak.
- Live and batch Whisper never overlap; background e5/NER/GGUF work cannot enter during capture.
- A capture/spool fault produces one typed event and a note from the exact durable prefix.
- Crash recovery never deletes an ambiguous artifact and never consumes bytes past the SQLCipher
  checkpoint.
- Four-hour 192 kHz master generation selects RF64 and stays bounded in RAM.
- Three same-process cycles return to a stable post-pipeline floor rather than raising the floor on
  every cycle.
- Folder seal/unseal is measured separately and remains a documented O(file size) follow-up; it is
  not used as evidence that the recording-time fix is complete.

## Verification status

Static review and runtime/build evidence remain intentionally separate. The final supervised
`scripts/ci.sh` run completed with `✅ CI: all gates green` and included:

- config audit (120 checks), hook guard (160 assertions), harness/eval/remote-policy selftests;
- both optimized Swift SRC helper selftests and typechecking;
- Clippy with warnings denied, `2259` Rust library tests passing (`15` ignored), and all `7`
  `murmur-brain` tests passing;
- `cargo audit`, `cargo deny`, both Rust builds, Angular lint and production build;
- all `264` mocked-Tauri Playwright tests on Chromium and WebKit with one worker;
- real headless Whisper/provider/Obsidian core E2E and mic+system dual-stream mix E2E.

The macOS parent-pipe close regression additionally passed 25 consecutive real-pipe repetitions.
The Stop/failed-save/picker regression passed 18 repeated cross-engine cases plus the complete
12-case focused suite before the full UI gate. Server counterpart checks passed workspace format,
Clippy, protocol tests, route/integration tests, quota rollback, and concurrent quota tests.

RED-before-GREEN was reproduced against the immutable pre-fix `f18dccb` snapshot with the same
`bounded_real_append_and_durable_reuse_at_192_khz_keeps_fixed_memory` acceptance: after 20,000
durable-spill cycles the old `Mutex<Vec<f32>>` retained a 67,108,864-byte allocation and failed the
32 MiB bound. The repaired fixed ring passed the identically named test at the same 20,000-cycle,
192 kHz contract boundary.

The workflow acceptance bar is therefore exercised: one cross-worktree lane, bounded inherited
thread caps, one aggregate deadline, and TERM→KILL→reap behavior for the owned process group. The
remaining release-level evidence is deliberately narrower: independent adversarial and lock reviews
of the frozen integration, followed later by a signed/notarized build Record/Stop soak with physical
footprint sampling. No signed-app capture or claim of production soak numbers is made here.
