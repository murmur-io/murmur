<!-- Generated 2026-07-04 via /research (murmur-researcher fan-out: 4 angles). Pricing/funding/version/platform-policy = point-in-time; every mobile capture/keychain/perf claim needs a real device + signed build to confirm. -->
# Research: Murmur on iPhone / Android — is it possible and realistic?

## TL;DR / Verdict

**Possible? Partly. Realistic as a *port*? No. Realistic as a re-scoped *companion*? Yes.**

Three independent walls, in order of decisiveness:

1. **The crown value-prop cannot legally/technically run on a phone.** Murmur's differentiator — botless capture of the *far side* of a call (Zoom/Meet/Teams) via macOS **ScreenCaptureKit** + Core-Audio process-tap — has **no equivalent on iOS** (a sandboxed app gets only its own-app audio + mic; call audio is reserved to Apple's Phone/FaceTime) and is **blocked on Android** (`AudioPlaybackCapture` excludes `USAGE_VOICE_COMMUNICATION`, exactly what conferencing/telephony uses). The *entire* mobile market routes around this with server-side bots, conference bridges, or dedicated hardware — proof it's unsolvable in a sandboxed app. On a phone, Murmur is **mic-only** (in-person capture).
2. **The heavy on-device ML stack does not lift onto mobile intact.** `mistralrs` (the brain) has no mobile target and its Metal path is dead on iOS; `candle` runs **CPU-only on iOS** (no Metal) and has no Metal on Android; `sherpa-onnx` bundles a macOS onnxruntime; `rusqlite bundled-sqlcipher-vendored-openssl` is a known-thorny cross-compile. A **minimal non-ML Rust core** (Tauri shell + `cpal` mic + `rusqlite` + a cloud/native-per-OS provider) *is* buildable.
3. **The store/vault/server assumptions don't hold.** iOS filesystem is sandboxed → no arbitrary Obsidian `.md` write (needs a security-scoped bookmark to a user-picked folder); the MCP localhost server can't live in the background on a phone; the murmur-server spec explicitly makes full vault **sync a non-goal**, so a companion's sync is net-new architecture.

**Recommendation: don't port. If mobile is pursued, build a `companion` — iOS-first — that is honest about being (a) the best *in-person* (mic-only, on-device) recorder and (b) a remote/viewer for the Mac, where the Mac stays the inference engine and canonical store.** Frame far-side capture as a permanent **macOS-native superpower**, not a cross-platform feature. Smallest first step: a throwaway `tauri ios init` build that (1) links a *minimal* feature set (`cargo build --target aarch64-apple-ios` with the ML tree `#[cfg]`-excluded), (2) records the mic, and (3) reads the user's Obsidian iCloud vault via a document-picker bookmark. That single spike de-risks the two load-bearing unknowns for near-zero cost.

---

## What we already have (from the repo, code-grounded)

**Portable today (pure Rust, zero OS pillar):**
- `src-tauri/src/crypto.rs` — AES-256-GCM envelope (`nonce(12)||ct||tag(16)`), `random_key`, `encrypt`/`decrypt`, `encrypt_file` with verify-before-destroy. Built on `aes-gcm` + `getrandom` only → the **whole per-folder CK/KEK crypto logic ports to iOS/Android**; only KEK *storage* + the *biometric gate* are OS-specific.
- SQLite/SQLCipher store — SQLCipher runs on iOS/Android, so the canonical store design ports (modulo the vendored-OpenSSL cross-compile).
- `cpal` mic capture — ports cleanly (iOS CoreAudio, Android Oboe/AAudio). **This is the only capture path that survives on a phone.**

**macOS-only pillars (the hard part) — all already `cfg(target_os="macos")`-gated, so they won't *break* a mobile build, but leave functional holes:**
- **Four native Swift sidecars** bundled as Resources (`tauri.conf.json:53-58`): `meetnotes-sysaudio` (ScreenCaptureKit), `meetnotes-audiocap` (Core-Audio tap, 14.4+), `meetnotes-aeccap` (AEC), `meetnotes-calendar` (EventKit). System audio is captured by **spawning a subprocess** (`audio/system.rs` uses `std::process::Command`, resolves the helper from `Contents/Resources`, `system.rs:8-71`) — **iOS App Store apps cannot fork/exec**, so this path is dead on iOS regardless of the API gap.
- `audio/system.rs::is_available()` / `select_helper()` — the single macOS seam that decides "can we hear the far side." There is **no portable output-capture abstraction beneath it** (`system.rs:51-71`).
- ASR/brain are Metal: `whisper-rs { features=["metal"] }`, `mistralrs 0.8.1 { features=["metal"] }`, `candle-* 0.10.2 { features=["metal","accelerate"] }`, `sherpa-onnx 1.13` ("STATIC onnxruntime built for macOS 13.4+"). **Default whisper model = `small` (~470 MB)**, `model.rs:42-46`; Polish forces the multilingual `small` build (`model.rs:47-52`). *(Correction to one sub-agent's claim: the default is `small`, not `large-v3` — this is good news for mobile, since `small` is real-time on A17/A18.)*
- `secrets/keychain.rs` — data-protection Keychain + `SecAccessControl(kSecAccessControlUserPresence)` biometric-gated KEK, `#[cfg(target_os="macos")]` (`keychain.rs:112-134`, `MacKekStore`/`write_biometric`). `objc2-local-authentication` (LAContext) already in the graph. Non-macOS falls back to a plain keyring.
- `mcp.rs` — a long-lived `tiny_http` server on `127.0.0.1:8765` looping on a background thread (`mcp.rs:16,68-93`). **Mobile OSes kill background sockets** → the MCP surface effectively can't exist on a phone.
- `screenshare.rs` (CGWindowList auto-relock) + `audio/output.rs` — macOS AppKit/CoreGraphics, N/A on mobile.
- `export/obsidian.rs::write_note(vault_dir, …)` writes `.md` to an **arbitrary filesystem path** (`config.vault_path`, `obsidian.rs:106-166`). On iOS the sandbox forbids arbitrary-path writes → needs a security-scoped bookmark.
- No mobile scaffolding exists: `gen/schemas/` holds only `macOS-schema.json` + `desktop-schema.json`; `tauri.conf.json` has no iOS/Android target; `identifier=com.meetnotes.app`, `minimumSystemVersion=13.4`.

**Sync foundation:** the murmur-server spec (`docs/superpowers/specs/2026-07-04-murmur-server-spec.md:20,304`) is **share-scoped only** — *"Full vault backup/sync is a written non-goal"*, *"no multi-device mirroring"*. It stores ciphertext blobs of explicitly-shared notes, not a device mirror. **A companion's sync is net-new architecture beyond that spec.**

---

## Findings (per angle; each claim → URL or file:line + confidence)

### A. Tauri 2 mobile + our Rust/ML stack (technical foundation)
- **Tauri 2 mobile is production-stable but rough** (high). iOS/Android first-class since Oct 2024; current line 2.9.x (Dec 2025). A Tauri iOS app = thin Swift/WKWebView shell + the Rust core as a static lib via FFI; default mobile plugins include **biometric auth**. Official caveat: *"not all desktop features/plugins are ported."* Our **zoneless Angular 18 renders unchanged** in WKWebView/Android WebView — with the known CSP `style-src` nonce trap (angular-zoneless T4), already solved via `dangerousDisableAssetCspModification`. [tauri-20, tauri mobile-alpha, develop/plugins]
- **`cpal` mic ports cleanly** (high). **`whisper-rs`/whisper.cpp buildable** — official iOS+Android examples; **Metal runs on iOS GPU** (whisper.cpp #3531 is a live iOS-Metal workload); Android = CPU/NEON/Vulkan, no Metal. Open risk: the `whisper-rs 0.16` binding `build.rs` handling iOS/Android triples + Metal-shader embedding — unverified, needs a spike (med).
- **`candle` = CPU-only on iOS** (high). candle itself + a worked Phi-3-on-iOS example confirm "no Metal on iOS"; `accelerate` is Apple-only. Our `candle metal+accelerate` features must become per-target → e5 embedder + NER run CPU-only/slower.
- **`mistralrs` = the hard blocker** (high it's unsupported). No iOS/Android target; Metal path dead on iOS. Must be `#[cfg]`-excluded on mobile — tolerable, since `reason::active_reasoner` already degrades to `StubReasoner` on model absence and cloud providers carry reasoning.
- **`sherpa-onnx`**: upstream C++ supports Android/iOS, but our crate bundles a **macOS** onnxruntime → mobile needs per-target ORT; cheapest is to `#[cfg]`-exclude diarization on mobile (med-high).
- **`rusqlite bundled-sqlcipher-vendored-openssl`** = the known-thorny cross-compile (high friction). Vendored `openssl-sys` fails for `aarch64-linux-android`/`-apple-ios` in upstream issues; Tauri's own mobile notes call OpenSSL cross-compile out. Mitigable (CommonCrypto on iOS, system/BoringSSL on Android) but a real build-system change. `sqlite-vec` (pure C) cross-compiles fine.
- **Binary size** (high): statically linking whisper.cpp + candle + ORT + vendored OpenSSL + SQLCipher = a heavy binary before any model; models are downloaded-not-bundled, but a phone build should ship `small`/`base-q4`, not large-v3.

### B. Audio-capture barriers (the make-or-break)
- **iOS: no path to system/far-side audio for a 3rd-party app** (high). ReplayKit delivers only `audioApp` (own app) + `audioMic`; DRM/protected media and VoIP downlink are never delivered. iOS 18.1 native call recording works **only** for Phone/FaceTime, not an API for others. App-Store "call recorders" (TapeACall) use a **3-way conference bridge**, not capture. → **iOS = mic-only.** [Apple RPBroadcastSampleHandler, Apple ReplayKit, Sonix iOS-18 guide, TapeACall]
- **Android: `AudioPlaybackCapture` exists but excludes exactly our audio** (high on the exclusion, med on per-app attribution). It captures only `USAGE_MEDIA/GAME/UNKNOWN`; **`USAGE_VOICE_COMMUNICATION` (Zoom/Meet/Teams/telephony) is not capturable**. Apps can also opt out (`ALLOW_CAPTURE_BY_SYSTEM`), most-restrictive wins. Requires the MediaProjection consent dialog + a `FOREGROUND_SERVICE_MEDIA_PROJECTION`. Play banned Accessibility-API call recording on **11 May 2022**. [Android av-capture, AudioPlaybackCaptureConfiguration, 9to5Google/Register]
- **The market's tell** (high): Otter/Fireflies/Fathom/tl;dv use **server-side joining bots** for the far side; their mobile apps are **mic-only** in-person. Plaud/Limitless are **hardware**; even they need speakerphone for call far-side. Four independent workaround categories = strong evidence no sandboxed phone app can tap call audio.
- **Background recording** (high): iOS needs the `audio` background mode (+ App-Store justification), yields to call interruptions; Android needs a `microphone`-typed foreground service (+ `FOREGROUND_SERVICE_MICROPHONE` on 14). Fine for a mic-only recorder, adds friction.

### C. On-device inference on a phone + mobile prior art
- **On-device ASR is solved and fast on iPhone** (high). Argmax iPhone-17 benchmarks: large-v3-turbo ~10× real-time (17 Pro GPU) / ~4× (16 Pro ANE); our default `small` is ~5× smaller → **easily real-time on any A17/A18**, base/tiny for older. **WhisperKit** (Argmax, CoreML/ANE, 99+ languages incl. **Polish**) is the iOS-preferred runtime; **Apple SpeechAnalyzer (iOS 26)** is faster but **has no Polish** → WhisperKit/whisper.cpp is mandatory for Murmur's multilingual promise. Android: whisper.cpp on CPU/Vulkan, slower/fragmented (med). [Argmax, WhisperKit ICML-2025, macrumors, vocai]
- **On-device LLM is real but a *different* engine** (high). iOS → **Apple Foundation Models (iOS 26)**: free on-device ~3B, Swift API, tuned for summarization/extraction + guided generation + tool-calling. Android → **LiteRT-LM / MediaPipe** (Gemma 3n). Map `SummarizerProvider` → a per-platform native provider; **don't port mistralrs**. The single-meeting note is feasible on-device; the **full cross-vault brain is better left on the desktop's larger model** (a 7–8B on a phone is marginal — heat/OOM is the limit). [Apple FoundationModels, Google AI Edge, buildmvpfast]
- **Battery/thermal** (med-high): on-device ASR ~7.5% battery / 45 min, minimal throttling; the risk is *continuous* ASR **+** LLM together → run the LLM once at the end / on desktop, not continuously.
- **Competitor mobile shape** (high): Otter/Fireflies/Granola/Plaud are all **cloud** on mobile; Granola is iPhone-only, no Android; the only on-device mobile prior art is **Superwhisper** (dictation, not meeting notes). **No meeting-notes competitor ships on-device on a phone** → a local-first mobile companion is genuinely differentiated *if* it clears the capture wall.

### D. Product paths + sync/E2EE + Obsidian/keychain fit
- **Secure storage**: iOS Keychain uses the **same Security.framework `SecItem*` + `SecAccessControl` + Secure Enclave** as macOS → `keychain.rs` is close (widen `cfg(target_os="macos")` to include `"ios"`; LAContext via the already-present `objc2-local-authentication`). Android = a **native Kotlin rewrite** (Keystore + BiometricPrompt + StrongBox). [talsec, developer.android keystore]
- **Obsidian mobile vault is reachable only via the picker** (high/med): iOS vaults live in `~/Library/Mobile Documents/iCloud~md~obsidian/Documents/<vault>/` or the app container; a 3rd-party app needs a **document-picker + security-scoped bookmark**. Crucial upside: **if the user runs Obsidian Sync / an iCloud vault, desktop-written notes already appear on the phone with zero new infra** → the desktop→phone note path is *free*. [Obsidian forum, medium sync guide]
- **Sync is net-new**: a companion needs phone→desktop audio (best: **LAN** via mDNS + a Keychain/Keystore-paired token + `crypto.rs` AES-GCM transport → desktop ingest → existing pipeline → note back into the vault). Server-relay sync is the spec's **non-goal** and an L++ surface — defer it. [server spec :20,304]

---

## Fit with Murmur's constraints

| Constraint | Verdict on mobile |
| --- | --- |
| **Local-first / privacy** | *Preserved if done right, strained if not.* On-device ASR (WhisperKit) + phone→desktop audio over **LAN** keeps audio on the user's devices. A **cloud-relay** mobile app would break the hard constraint and drop to Otter/Granola parity — **reject that shape**. A joining bot is off-identity (cloud egress + kills the botless moat). |
| **Obsidian-native / owned files** | *Strengthened on the read leg* (piggyback the user's existing Obsidian iCloud/Sync vault). iOS sandbox forces a bookmark picker; the files stay the user's. |
| **SQLite canonical** | *Preserved only if the phone is a thin reader/capturer.* Desktop stays source of truth; do **not** let the companion grow its own authoritative DB (would create the "three diverging copies" the constraint forbids). |
| **Provider seam + redaction firewall** | *Intact and load-bearing.* The phone has no viable heavy local brain → Ask/summarize routes to the desktop engine or a cloud provider **through the redaction firewall**. |
| **macOS-first** | *This is the escape hatch and the honest framing.* The far-side moat isn't "hard to port" — it's **architecturally impossible** to port. Keep the meeting-capture workhorse on the Mac by design. |
| **CI honesty bar** | Every capture/keychain/biometric/perf claim needs a **real device + signed build + recorded evidence** — the "needs a real Mac" bar, doubled. Adds an iOS/Android toolchain today's macOS `scripts/ci.sh` doesn't cover. |

---

## Options and tradeoffs

| Option | Effort | Risk | What it unlocks | Verdict |
| --- | --- | --- | --- | --- |
| **A. Viewer-only** — Tauri-iOS shell reading the user's Obsidian iCloud vault + local `.md` search | **S–M** | Low | Read notes on the phone | **Redundant with Obsidian mobile** unless it carries **Ask** — at which point it's a companion. Fallback only. |
| **B. Companion** — phone = mic capture + note viewer + Ask; desktop = engine + canonical store. Notes desktop→phone via existing vault sync (free); audio phone→desktop via LAN | **L** | Med-high | The mobile *expression* of the real product, zero macOS-pillar dependency, lands in a real market gap (no on-device mobile competitor) | **RECOMMENDED.** Only variant that's realistic *and* identity-preserving. |
| **C. Full port** — rebuild capture natively per-OS, ship on-device ASR + brain on the phone | **XL** | High | A standalone mobile app that is a *worse* recorder than the desktop (iOS still can't get far-side audio; Android needs a Kotlin rewrite; brain marginal) | **Reject.** Mostly fantasy. |
| **D. Cloud-relay** — phone streams audio to cloud for transcribe+summary | **S–M** | Med | Fastest to ship, matches Otter/Granola | **Reject.** Breaks local-first; surrenders the only mobile differentiation. |

---

## Recommendation and first step

**Build the companion (Option B), iOS-first; explicitly reject the full port and the cloud-relay; keep viewer-only as the fallback.** Sequence the cheap half first so value ships before the hard module:

1. **Smallest verifiable spike (de-risks the load-bearing unknowns at once, needs a signed dev build on a real iPhone):** a throwaway `tauri ios init` project that
   - **links** — `cargo build --target aarch64-apple-ios` on a **minimal** feature set (`tauri` mobile + `cpal` + `rusqlite bundled-sqlcipher-vendored-openssl` + `reqwest rustls`), with `mistralrs`/`sherpa-onnx`/`candle`/`whisper-rs` **`#[cfg]`-excluded** — this either links (non-ML core viable) or fails on OpenSSL/SQLCipher (the highest-probability early blocker), for ~a day. **Do not** try to link `mistralrs` first — known-negative, burns a day.
   - **records the mic** to a WAV, and
   - **reads a `.md`** from the user's Obsidian iCloud vault via a document-picker security-scoped bookmark.
   If all three pass → companion is real. If (mic) or (vault) fail → fall back to viewer-only.
2. **Verify the capture wall before betting** (converts the one med-confidence, load-bearing claim to high): on a real Android 14 phone, a minimal `AudioPlaybackCapture` test recording during a live Google Meet **and** a cellular call — confirm both buffers are silent (they will be) while a YouTube playback *is* captured. iOS needs no spike — the absence of an output-tap API is documented.
3. Then ship **desktop→phone notes = free** (document "point Murmur-mobile at your Obsidian vault"; no engine work), and only after that build the **LAN audio handoff** (the genuinely new module: mDNS pairing + `crypto.rs` transport + a desktop ingest command reusing the existing pipeline). **Defer server-relay sync** (spec non-goal, L++).

Positioning throughout: mobile Murmur is **the best in-person, on-device, private recorder + a Mac companion** — never pitched as a virtual-meeting/call recorder. Far-side capture is a **macOS-native superpower**, permanently.

---

## Open questions / what could not be verified (all need a real device + signed build)

- **Does Tauri-iOS actually build *our* tree?** The heavy always-compiled Metal deps almost certainly need a **mobile-only Cargo feature set that excludes brain/embedder/ASR/diarization**. Unverified — the spike's `cargo build --target aarch64-apple-ios` settles it.
- **`whisper-rs 0.16` binding cross-compiling** for iOS/Android with correct Metal-shader handling (the C library supports both; the binding is the risk).
- **The Android capture claim** (`USAGE_VOICE_COMMUNICATION` exclusion for live Zoom/Meet) is med-confidence from convention + user reports, not a decompiled manifest — the Android probe closes it.
- **iOS App Store approval** for a "meeting recorder" using the Audio background mode — Apple scrutinizes this; not proven acceptable.
- **Reliable *write* into the Obsidian iCloud vault** via security-scoped bookmark (read path is well-attested; write + Obsidian's whole-vault assumption is the unknown for any future phone→vault write).
- **Polish note quality** from Apple Foundation Models / small on-device LLMs — unmeasured; a 3B may under-serve inflected-Polish summarization vs the desktop's model.
- All perf/battery/thermal numbers are vendor/blog-reported (2025–2026), point-in-time; none validated on Murmur specifically.

---

## Sources

**External (URLs fetched by the sub-agents):**
- Tauri mobile: https://v2.tauri.app/blog/tauri-20/ · https://v2.tauri.app/blog/tauri-mobile-alpha/ · https://v2.tauri.app/develop/plugins/develop-mobile/
- ML on mobile: https://github.com/huggingface/candle (iOS CPU-only) · https://www.strathweb.com/2024/05/running-microsoft-phi-3-model-in-an-ios-app-with-rust/ · https://github.com/ggml-org/whisper.cpp + issue #3531 · https://github.com/EricLBuehler/mistral.rs · https://github.com/k2-fsa/sherpa-onnx · https://github.com/rustaudio/cpal
- Cross-compile pain: https://github.com/sfackler/rust-openssl/issues/1331 · https://github.com/sfackler/rust-openssl/issues/1530
- iOS capture/policy: https://developer.apple.com/documentation/replaykit/rpbroadcastsamplehandler · https://developer.apple.com/documentation/ReplayKit · https://sonix.ai/resources/transcribe-iphone-phone-calls/ · https://www.tapeacall.com/blog/3-way-conference-calling-on-iphone-everything-you-need-to-know · https://developer.apple.com/app-store/review/guidelines/
- Android capture/policy: https://developer.android.com/media/platform/av-capture · https://developer.android.com/reference/android/media/AudioPlaybackCaptureConfiguration · https://android-developers.googleblog.com/2019/07/capturing-audio-in-android-q.html · https://9to5google.com/2022/04/21/google-will-block-all-third-party-call-recording-apps-on-play-store/
- On-device inference: https://www.argmaxinc.com/blog/iphone-17-on-device-inference-benchmarks · https://arxiv.org/html/2507.10860v1 · https://github.com/argmaxinc/WhisperKit · https://developer.apple.com/documentation/FoundationModels · https://ai.google.dev/edge/mediapipe/solutions/genai/llm_inference/android · https://www.macrumors.com/2025/06/18/apple-transcription-api-faster-than-whisper/
- Competitors/mobile: https://otter.ai/blog/ai-notetaker-for-in-person-meetings · https://tldv.io/blog/mobile-meeting-recordings/ · https://www.plaud.ai/products/plaud-note-ai-voice-recorder · https://superwhisper.com/ios
- Keychain/Obsidian/sync: https://docs.talsec.app/appsec-articles/articles/ios-keychain-vs.-android-keystore · https://developer.android.com/privacy-and-security/keystore · https://forum.obsidian.md/t/full-file-system-access-for-the-ios-app-open-existing-vault-folder/28266 · https://medium.com/@payamqorbanpour/sync-obsidian-vaults-data-on-ios-and-macos-42b5170615e8

**Repo (code-grounded):**
- `src-tauri/Cargo.toml` (whisper-rs/mistralrs/candle metal, sherpa macOS ORT, rusqlite bundled-sqlcipher-vendored-openssl, keyring apple-native, objc2*, target-cfg gating)
- `src-tauri/src/audio/system.rs:8-71` (subprocess-spawned SCK sidecar; `is_available`/`select_helper` — the macOS far-side seam) · `audio/tap.rs:35-57` (Core-Audio tap, 14.4+) · `sysaudio/sysaudio.swift:53-57` (`SCContentFilter` + `capturesAudio`)
- `src-tauri/src/transcribe/model.rs:42-52` (default `small`, Polish→multilingual) · `transcribe/whisper.rs:28-61` (Fast/Accurate profiles, Metal load)
- `src-tauri/src/crypto.rs:28-135` (portable AES-GCM) · `secrets/keychain.rs:112-134,400-453` (macOS-gated Keychain + SecAccessControl KEK) · `biometric.rs` (LAContext)
- `src-tauri/src/mcp.rs:16,68-93` (persistent localhost server) · `export/obsidian.rs:106-166,422` (arbitrary-path vault write) · `settings/config.rs:73,489` (vault_path)
- `src-tauri/src/screenshare.rs` + `audio/output.rs` (macOS-only, cfg-gated)
- `src-tauri/tauri.conf.json:53-58` (four native sidecars; no iOS/Android target) · `gen/schemas/` (desktop/macOS only)
- `docs/superpowers/specs/2026-07-04-murmur-server-spec.md:20,304` (vault sync = written non-goal)
