<!-- Generated 2026-07-26 via /research + a 13-agent Workflow (8 angles → adversarial refutation of the 4 decisive claims → completeness critic).
     NOTHING here is measured on Windows hardware. Every external claim is documentation / vendor-page / issue-tracker evidence; every repo claim is
     file:line read in this tree at commit 031e225. Pricing, eligibility rules and platform market share are point-in-time. -->

# Research: Murmur on Windows — feasibility, cost and port architecture

Supersedes the one-paragraph sketch in `docs/research/2026-07-25-competitive-gaps-6angle-brainstorm.md` §"Angle 3", whose
headline ("Windows = feasible, size **L**, no architectural blocker, cost dominated by build/sign/CI **ops**") is **wrong in
both halves** and is corrected below.

---

## TL;DR / Verdict

**Technically feasible with no dead end — but it is an XL project (34–50 engineer-weeks), the cost is in the CODE not the ops,
and the binding constraint is not feasibility at all: it is that there is no measured demand to multiply.**

Three things flipped versus the 25.07 brief:

1. **The ops tax is small; the code tax is large.** Signing is ~$120–309/yr and fully automatable from a hosted runner; GitHub
   Actions is *free* for this repo (public). What is expensive is a layer nobody had censused: **~219 POSIX file-identity
   assertions** (`dev`/`ino`/`nlink`/`O_NOFOLLOW`/`0o600`) across **~12,800 LOC in six lock-model-load-bearing files**, whose
   `std::os::unix` imports are **ungated** — so the crate does not even *parse* for `x86_64-pc-windows-msvc`. Two of those
   primitives (`renameatx_np(RENAME_SWAP)`, `fclonefileat`) have **no Windows equivalent at all**, so the Obsidian
   marker-cleanup atomic publish and the legacy-scratch exclusive claim need a **new crash-safety design** that
   `lock-security-reviewer` must re-derive from scratch. That is not a port, it is a re-proof.

2. **The compiler-visible census is the wrong census.** The dangerous class is code with **no `cfg` at all** that compiles on
   Windows and then silently lies: 14 macOS-only binaries shelled out via `Command::new`; the default note-generation provider
   (`claude_code`) resolving its CLI through a `$SHELL -lic` PATH probe split on `':'` (100% non-functional on Windows); its
   supply-chain `vet_binary` check wrapped in `#[cfg(unix)]` → a **silent no-op**; `is_any_screen_captured() -> false` while
   `relockOnScreenshare` **defaults to ON in the UI** → a privacy toggle that lies; `active_name_redactor()` failing **open** to
   `NoopNameRedactor` on the cloud-egress path; `device_platform() -> "macos"` hardcoded into the sharing wire format.

3. **The capture moat inverts.** On macOS, botless far-side capture costs us four signed Swift sidecars and a TCC prompt — that
   difficulty *is* the moat. On Windows it is one flag (`AUDCLNT_STREAMFLAGS_LOOPBACK`), no consent, no helper — so it is
   **commodity**, and the local/private/free/OSS niche is already held by **Meetily** (MIT, Rust+Tauri, mac+Windows, 26.7k
   stars) while the positioning rival **Granola shipped Windows ~a year ago**.

**Recommendation: do not port now.** Instead spend ~1 eng-week making Windows demand *countable*, and land the ~60% of the
seam work that has positive expected value **even if Windows is cancelled forever** — because that work also closes four
fail-open bugs that exist on macOS today.

---

## What we already have (code-grounded, `file:line` read in this tree)

**The port surface, measured three ways — each larger than the last:**

| Census | Count | Why it matters |
| --- | --- | --- |
| `cfg(target_os)` blocks | **79** (120 mentions: 120× `macos`, 3× `linux`) | the *visible* surface, and the one the 25.07 brief scoped from |
| files using `std::os::unix` / `std::os::fd` | **13**, of which **6 are UNGATED** | the crate does not compile for `*-windows-msvc` |
| POSIX file-identity call sites | **~219** across ~12,800 LOC in 6 files | `crypto.rs`=50, `audio/source.rs`=85, `audio/spill.rs`=31, `export/obsidian.rs`=38, `instance_lock.rs`=15 |
| uncfg'd macOS-binary shell-outs | **14** across 11 files | compiles clean, fails **silently** at runtime |

The six ungated `std::os::unix` imports: `crypto.rs:33`, `export/obsidian.rs:4`, `instance_lock.rs:11-12`,
`audio/spill.rs:35`, `audio/source.rs:9`, `pipeline.rs:346`. `instance_lock.rs:25-26` additionally declares
`extern "C" { fn flock(…) }` **unconditionally** — a link error on Windows.

**Critically, the existing `cfg(not(target_os = "macos"))` fallbacks were written for other *Unix*, not for Windows.** The
comment at `crypto.rs:42-45` says so verbatim: *"Murmur is macOS-first; **other Unix test targets** retain the pre/post-open
identity checks and omit the platform-specific flag."* On those branches `NOFOLLOW_FLAG` is simply `0` — i.e. the symlink
hardening silently degrades. `export/obsidian.rs:199` and `:886` are hard-error stubs
(`"anchored marker cleanup requires macOS file APIs"`).

**The build system is macOS-shaped too** (my own finding, not in any angle): `src-tauri/build.rs` calls `build_swift_helper()`
**unconditionally** for all four Swift helpers, and in `PROFILE=release` it `panic!`s if the universal Mach-O was not freshly
built (`build.rs:248-251`). `tauri_build::build()` then validates that every `bundle.resources` entry exists
(`tauri.conf.json:53-58` lists five `binaries/…`). A Windows build fails in `build.rs` before reaching any application code.

**What is genuinely already portable** — and it is more than expected:
- `commands/lock.rs` (1,217 lines) and `storage/seal_store.rs` (1,066 lines): **zero** `cfg(target_os)`, **zero**
  `std::os::unix`. The lock *logic* ports byte-for-byte; only the file-identity *proofs* underneath it do not.
- `src-tauri/src/e2ee/` and `src-tauri/src/share/`: **zero** `target_os`, **zero** `std::os::unix`. The server's
  `normalize_platform` (`../murmur-server/…/routes/auth.rs:52-58`) accepts any non-empty string ≤64 chars, so **no
  `murmur-protocol` wire change and no dual-repo coordination is needed**. The only defect is the hardcoded
  `device_platform() -> "macos"` literal at `share/mod.rs:252-255` — ~2 days, not the weeks a "spans two repos" framing implies.
- The DSP layer — `audio/{mixer,merge,align,aec_offline}.rs` + the `sonora` AEC3 crate — is pure Rust (~1.3 kLOC of tested logic).
- The runtime-selection pattern already exists and is the right shape to copy: `reason::active_reasoner`,
  `embed::active_embedder`, `summarize::redact::active_name_redactor` → `Box<dyn T>`, plus the `KekStore` trait +
  generic `resolve_kek<S: KekStore>`.

---

## Findings by angle

Each decisive claim was handed to an independent agent instructed to **refute** it. All four came back at least partially
refuted — the refutations are where the real numbers are.

### 1. System-audio capture — the crown jewel survives, but not for free

**Survives refutation:**
- WASAPI **render-endpoint** loopback captures the far side with **no consent prompt and no bundled helper**, Windows 10
  1703+ (~100% of installs). The documented consent prompt lives on `ActivateAudioInterfaceAsync` — the *process*-loopback path
  — not on endpoint loopback. [MS Learn: Loopback Recording]
- **Teams specifically is confirmed working** by Microsoft's own bug report: device loopback "can successfully capture the
  output audio from the Microsoft Teams app" while **process loopback returns all zeros**. [Windows-classic-samples#414]
- There is **no Android-style communications-category exclusion**. The `USAGE_VOICE_COMMUNICATION` analogy that killed the
  mobile port does not transfer.
- The clock domains genuinely match: `GetBuffer`'s `pu64QPCPosition` is `10,000,000·t/f` over raw QPC ticks, and Rust std's
  Windows `Instant` is literally `QueryPerformanceCounter`.

**Refuted / corrected:**
- **"`merge_streams` works unchanged and better" is a category error.** `audio/merge.rs:41-47` defines
  `StreamInput { segments, started_at: Instant, speaker }` — **one** anchor per stream, one **constant** offset. It consumes no
  per-packet timestamps, so per-packet QPC buys nothing without rewriting `merge.rs`. Worse, its module doc (`:5-19`) requires
  the "others" WAV to be a **continuous real-time recording** — and endpoint loopback **emits no packets while nothing renders**
  [PortAudio#935]. Dropped silence makes stream-time→wall-time a piecewise warp that no single constant anchor can correct;
  error grows **unbounded with meeting length**. It also breaks `align.rs:78 estimate_stream_offset` (constant-lag NCC, bails on
  `spread > MAX_SPREAD_S`) and `align.rs:132 archive_delays`. Because it is **driver-dependent** (Realtek "Speakers" fine,
  "Headphones" not), *it will pass a demo and fail in the field*.
- **cpal is not sufficient.** cpal 0.16 *does* already do endpoint loopback (`host/wasapi/device.rs:455-472`) — correcting the
  25.07 brief — but it hardcodes the `eConsole` role (`:907-911`) while conferencing apps use `eCommunications`; it **discards
  the WASAPI buffer flags entirely** (`stream.rs:407,421`), swallowing `AUDCLNT_BUFFERFLAGS_SILENT` (contents undefined),
  `DATA_DISCONTINUITY` (the timeline just broke) and `TIMESTAMP_ERROR` (the QPC value is invalid); it waits `INFINITE` with a
  literal `// TODO: allow setting a timeout`, so during silence the thread blocks with nothing to delta; and it has **no
  mid-stream `AUDCLNT_E_DEVICE_INVALIDATED` recovery** — plugging in a Bluetooth headset mid-call (A2DP→HFP) permanently kills
  the "others" stream. → needs the **`wasapi` crate (MIT)**, a *new dependency requiring approval*.
- **No exclude-self.** Both macOS backends filter our own audio out — `sysaudio.swift:58-60` (`excludingApplications:`),
  `audiocap.swift:967-978` (`excludePids = [getppid(), getpid()]`). **WASAPI endpoint loopback has no exclusion mechanism
  whatsoever**, and the app *does* render audio (`detail.component.ts:304` `convertFileSrc` playback). Playing back a prior
  meeting while recording would transcribe it as the far side.
- **No AEC story.** `audio/output.rs` (118 lines) classifies the output device to gate echo risk; `audio/aec.rs` (945 lines)
  drives the VPIO helper. On speakers, Windows loopback captures the far side **and** the mic re-captures it acoustically →
  double transcription into two streams, which `merge.rs` has no defence against.
- **ARM64 is unproven, not "low risk":** a Microsoft moderator (2026-01-06) states loopback "is not explicitly documented as
  supported on ARM64… behavioral parity with x64 cannot be assumed" — on exactly the Copilot+ hardware a paid meeting app targets.

**Honest size: 5–8 eng-weeks for the capture layer + 6–10 for de-Unix'ing `source.rs`/`spill.rs` (content-loss risk).**

### 2. ML stack off Metal — the cheapest angle, and the earlier fear was misplaced

**Refuted (in our favour):** the "Windows must drop the default model / ship N installers" fear is wrong on our architecture.
- Vulkan is a **feature flag on an existing dep** (`whisper-rs/vulkan` = `_gpu` + `dep:libc`) → **zero new crates, no approval
  needed**, and the manifest already has the `[target.'cfg(target_os = "macos")'.dependencies]` block needed to split
  `metal` from `vulkan`. ~1–2 eng-weeks, not the dominant cost.
- The default model is **RAM-gated, never GPU-gated** (`transcribe/model.rs:105-122`; turbo needs ≥12 GiB). Live captions are
  **already pinned to `small`** (`live_model_pin`, `config.rs:593-595`); turbo is **batch-only**, batch runs post-Stop and is
  **VAD-segmented** so only speech seconds decode (`pipeline.rs:1321-1366`), and the two streams transcribe **sequentially on
  one `Transcriber`** (`pipeline.rs:1778-1793`). **Missing GPU buys extra offline latency, not a quality regression.**
- Amusing detail: on Windows the default *already* resolves to `small` by accident — `total_ram_bytes()` shells out to
  `sysctl -n hw.memsize` (`model.rs:157-167`), fails, returns `None`, fail-small. One `GlobalMemoryStatusEx` fixes it.

**Real cost sits elsewhere:** the brain sidecar is **outright broken on Windows today** —
`reason/sidecar.rs:133-138` `make_stdin_nonblocking` returns `AppError::Unavailable` on non-unix; the killer shells out to a
`kill` binary that does not exist (`sidecar.rs:450`); the parent watchdog is a **kqueue** (`murmur-brain/src/main.rs:104-105`)
needing a Job Object replacement; `available_ram_bytes()` shells to `vm_stat` with a documented **fail-open**. And
**`mistralrs 0.8.1 has no `vulkan` feature at all** (accelerate/cuda/cudnn/flash-attn/metal/mkl/nccl/ring) → the on-device brain
is CPU-only on Windows unless we swap the sidecar engine to `llama-cpp-2`. `sherpa-onnx-sys 1.13` has **no aarch64-windows
arm** → diarization must be cfg-excluded on ARM64. **8–12 eng-weeks.**

### 3. Secrets & the lock model — the invariant survives, the *property* is honestly weaker

**Survives:** `KeyCredential::RequestSignAsync` is deterministic (RSASSA-PKCS1-v1_5-SHA256, no PSS salt — verified against
Microsoft's own Hello sample using `RSASignaturePadding.Pkcs1`, corroborated by WebAuthn COSE `-257/RS256` and by KeePassXC
shipping this exact construction since PR #7384 with no signature-instability reports). So
`wrapping_key = HKDF-SHA256(ikm = signature, salt = challenge)` reproduces, and the cryptographic gate is real — unlike
`UserConsentVerifier`, which is a boolean (KeePassXC #10042 shipped a real Escape-key bypass).

**Refuted:** *"unobtainable without a live Windows Hello gesture"* is **false for our app shape**. Tauri v2 emits only WiX MSI /
NSIS setup.exe — **no MSIX, no package identity** — and per Microsoft's own Q&A, an unpackaged Win32 app's KeyCredential is
**scoped to the Windows user account, not the app**. Any process running as the user can `OpenAsync("murmur_kek")`, replay our
challenge, and derive the identical key after a gesture the user cannot attribute to Murmur. macOS binds the KEK item to
Murmur's **code signature** via the keychain ACL — that binding **has no Windows equivalent**.

Two preconditions the researcher missed, both blocking:
- `RequestSignAsync` is a "Request"-pattern WinRT method Microsoft documents as **unsupported in desktop apps** (CoreWindow
  dependency), and unlike `UserConsentVerifier` there is **no `IKeyCredentialManagerInterop`**. Observed failure: the Hello
  dialog renders *behind* the app window unfocused so the sensor never fires (cppwinrt#999, closed unresolved). KeePassXC's
  production workaround is `FindWindowA("Credential Dialog Xaml Host")` + `SetForegroundWindow()` retried 3× — **an
  undocumented window-class race in the unlock path of a load-bearing security gate.**
- **Destructive Hello PIN reset** — the routine consumer lock-screen "I forgot my PIN" flow — deletes "any keys or certificates
  added to their Windows Hello container"; so do TPM clear and some BIOS updates. Non-destructive reset needs the *enterprise*
  Microsoft PIN Reset Service. Our `resolve_kek` correctly refuses to mint over sealed data, and macOS's multi-generation
  `kSecMatchLimitAll` recovery has **no Windows analogue** → a **mandatory bip39 recovery phrase is a shipping precondition**,
  not polish. (`bip39` + `argon2` are already deps.)

Also: today's `cfg(not(target_os = "macos"))` arm stores the master KEK as a **plaintext hex string with no presence check at
all** (`secrets/keychain.rs:185-204`) — and would not even run, because `keyring` is pinned `features = ["apple-native"]`
(`Cargo.toml:99`).

**The honest marketing line:** *"protected by Windows Hello (user-presence)"* — **never** "same protection as Touch ID".

### 4. Build, signing, CI — cheap, but with one eligibility landmine

**Refuted (in our favour on cost, against us on the plan):**
- Azure Artifact/Trusted Signing (~$120/yr) is probably **unavailable to us**: Microsoft employees state verbatim on MS Q&A a
  **three-years-of-verifiable-history** requirement absent from the quickstart, and 2026-03 reports say the operative
  restriction is **US/Canada organizations only, with individual onboarding paused**. Creating a Polish sp. z o.o. does *not*
  fix this — it fails for three years.
- **But the "physical USB token on a Windows box" fallback is a myth.** SSL.com sells an **Individual Validated** publicly-trusted
  code-signing cert, "no business registration required", **$129/yr**, and its FIPS-140-2 eSigner cloud HSM signs `exe`/`msi`
  **from `runs-on: ubuntu-latest`** with a TOTP secret. Total ~**$309/yr fully automated**. `jsign` (Apache-2.0) independently
  covers eight cloud-KMS backends. Certum (Polish CA) has an open-source cloud cert at **$58** — but SimplySign automation needs
  a phone-app login per container restart, so it is cheap-but-semi-manual.
- **GitHub Actions is free for this repo** (public), and `windows-latest` is the cheapest paid tier anyway ($0.010/min vs macOS
  $0.062/min).

**Irreducible:** builds must happen **on Windows** (MSI cannot be cross-built; the whisper-rs/CMake + sherpa static-ORT + candle
tree makes `cargo-xwin` a research bet), and **SmartScreen cold start** — weeks and hundreds of clean installs, no consumer
submission path, and per Microsoft's current docs **EV no longer bypasses it**. Our macOS users never see this.
**Planning posture: SSL.com IV + eSigner as baseline, Artifact Signing as optional upside. 4–7 eng-weeks of ops work.**

### 5. Frontend / WebView2 — the cheapest angle, for a reason nobody had noticed

**The main window is NOT transparent and uses NO native vibrancy.** `tauri.conf.json` has neither `transparent` nor
`windowEffects`; the entire Liquid Glass shell is **in-page** `backdrop-filter` over a CSS aurora (`--aurora-field` in
`src/design-tokens/glass.css`, painted on `body::before`). In-page glass over an opaque window carries to Chromium/WebView2
essentially unchanged — and Chromium's `backdrop-filter` is the better-optimised implementation, so the Windows UI is plausibly
**faster** (relevant given our known WebKit typing-lag problem, PR #423). *Unmeasured — do not ship it as a marketing line.*

Real work: the frameless `transparent(true)` + `Effect::HudWindow` **recorder bar** (`lib.rs:1086-1102`) has no Windows
equivalent and hits tauri#15512 (transparent window kills CSS `backdrop-filter` on Windows) so even the in-page fallback fails
there; **Windows 11 Snap Layouts** on a custom titlebar is an open upstream gap (tauri#4531) — 2 days if we accept native
decorations, 1.5–2.5 wk if we hand-roll `WM_NCHITTEST`; ~20 hardcoded traffic-light clearance constants; **46 hardcoded ⌘/⇧
glyphs across 17 files with zero platform detection anywhere in `src/`**. The **T4 CSP nonce fix carries verbatim** (CSP3
§6.7.3.2 is engine-agnostic and the nonce injection lives in platform-independent `tauri-utils`) — but per our own T4 rule it
still needs **one packaged render-test of a ROUTED CONTENT view**, which is exactly the 0.5.0 mistake. **5–8 eng-weeks**
(4–5 with native decorations).

### 6. Feature parity beyond audio — one surprise win, two hard losses

- **OCR is parity-or-better.** `Windows.Media.Ocr` with `Language.OCR~~~pl-PL` is a first-class, free, in-OS, offline recognizer
  — while our own code already hedges on whether Vision supports `"pl"` at our 13.4 floor (`extract/ocr.rs:66-83`). Keeps the
  zero-model-download and zero-egress promises. No new crate.
- **PDF** needs `pdfium-render` + a bundled **3.7 MB `pdfium.dll`** — a new supply-chain item and a sixth signed binary.
  (`lopdf` is DLL-free but has no scanned-page rendering, losing the OCR fallback.)
- **Calendar is BLOCKED.** `AppointmentManager.RequestStoreAsync` needs the `appointmentsSystem` **restricted capability** →
  MSIX package identity + Microsoft Store approval. Windows ships without calendar, or takes a Graph connector that is
  `EgressClass::External` and contradicts constraint #1.
- **Apple Reminders is BLOCKED** for the same reason (Microsoft To Do = Graph = cloud) — `commands/reminders.rs:69-120`, an
  entire user-facing feature with its own voice-action dispatch path and injection-escaping tests. **Two** OS-integration
  casualties, not one.
- **No `NSProcessInfo.thermalState` equivalent exists in user mode** → the thermal governor (`thermal.rs:146`) is permanently
  absent on Windows.
- **No NTFS equivalent of APFS `fclonefileat`** → `spill.rs`'s atomic legacy-scratch claim must be re-implemented as a
  non-atomic copy and its content-loss argument re-derived.

### 7. The fail-open class the census misses (completeness critic)

This is the single most valuable output of the whole run. Sorted by danger:

| # | Fail-open | Where | Consequence |
| --- | --- | --- | --- |
| 1 | `claude_code` — the **default provider** — resolves its CLI via `$SHELL -lic printf '%s' "$PATH"` split on `':'`, and treats a value as a path only if it `contains('/')` | `summarize/claude_code.rs:562-598, 610-626` | **Default note generation is 100% non-functional on Windows.** No `$SHELL`, `;` separator, `\` paths, and the CLI installs as `claude.cmd` which Rust refuses to spawn with args post-CVE-2024-24576 |
| 2 | `vet_binary`'s uid-owner + world-writable checks are inside `#[cfg(unix)] { … }` | `claude_code.rs:628-666` | A security control that passed adversarial review on macOS **silently vanishes** on the path that sends transcripts to the cloud |
| 3 | `is_any_screen_captured() -> false` on non-macOS, while `relockOnScreenshare` **defaults to `true`** in the FE | `screenshare.rs:340-344`; `settings.store.ts:1353` | The watcher runs, never fires, never tells the user — **the UI asserts protection that does not exist** |
| 4 | `active_name_redactor()` returns `NoopNameRedactor` (byte-identical passthrough) whenever the NER model is absent **or init errors** — error path only `warn`s, and a test **pins the no-op as contract** | `summarize/redact.rs:87-102, 1339-1350` | This degradation is **intentional and documented** on macOS ("a NER miss leaks no more than the no-op"). On Windows the *probability* rises sharply — candle's mDeBERTa must run without `metal`/`accelerate`, and cfg-ing candle out to dodge the Apple-only feature flags would make it permanent — so the degradation must become **visible in the egress-consent UI**, not silent |
| 5 | `device_platform() -> "macos"` hardcoded | `share/mod.rs:252-255` | Every Windows user appears as a Mac in their own device manager → **"revoke all other devices" becomes actively dangerous** |
| 6 | 14 uncfg'd macOS binaries: `open`, `osascript`×3, `sysctl`×3, `ps`, `shasum`, `/bin/date`, `/usr/bin/touch`, `/bin/kill`×4, `vm_stat`×2, `sw_vers`, `git`, `$SHELL` | 11 files incl. `update.rs:202`, `commands/audio.rs:499`, `perf.rs:975`, `model.rs:158,922,929`, `obsidian.rs:1495`, `sidecar.rs:185` | Each compiles clean and fails at runtime. Two should be **deleted not ported** (`shasum` → the `sha2` crate already in-graph; `/bin/date`+`touch` → `std::fs`) |
| 7 | The **stdio MCP connector** requires `Path::is_absolute()` then spawns directly | `connectors/mcp_client.rs:270-300` | Windows MCP servers are `npx`/`uvx` `.cmd` shims — unusable, and fixing it re-opens the argument-injection review the absolute-path rule was closing |
| 8 | `sanitize_title` has **no length cap**; nothing normalises line endings | `export/obsidian.rs:944-969, 1198` | MAX_PATH 260 blowout in a OneDrive vault fails the atomic export **on the crash-safety path**; `core.autocrlf=true` makes every note look externally edited → duplicate ` (1)`, ` (2)` files |
| 9 | Only **2 of 2,330 Rust tests** are macOS-gated | — | Effectively the whole suite must compile and pass on Windows; every `#[cfg(not(windows))]` on a lock/crypto test is a coverage hole to justify |
| 10 | `secrets/keychain.rs` backs **five** secret classes, not one — DB DEK, master KEK, **E2EE account MK**, MCP bearer token, BYO connector creds | `keychain.rs:71,150,166,340,360,391,412,1469,1529,1554,1577` | The Hello design covers only the KEK. The other four need a second, **non-interactive** store (DPAPI/Credential Manager) — a second threat model and a second lock-security review. Honest statement: on Windows any same-user process can read the DEK and decrypt the whole library |
| 11 | The repo's **binding** dev process does not run on Windows: `scripts/agent-harness` (`#!/bin/sh`+python3), `agent-resource-run` (`fcntl.flock`, POSIX process groups), 5 `.sh` hooks | `agent-resource-run:228,268,367`; `.claude/hooks/*` | Windows-native debugging — the only place several findings can be settled — produces **no hash-bound attestation**, violating "the implementer never owns the verdict" for exactly the riskiest work |

### 8. Market — the constraint that dominates everything above

- **The TAM headline is ~3× overstated.** Obsidian ships desktop installers as GitHub assets, so the download counts give the
  only hard platform data for our *actual* population: **60–68% macOS** across six consecutive releases (v1.12.7 = 6,558,102
  `.dmg` vs 4,302,940 `.exe`). Including Linux: mac 56.2% / Win 36.9% / Linux 6.9%. Mac-only → Mac+Windows is **+66% reach, not
  +250%**. The macOS skew increases monotonically as you narrow: global desktop 22% → US 36% → professional devs 40% → Obsidian
  ~65%.
- **Murmur has ~23 lifetime `.dmg` downloads across 25 releases** (max 4 on any single release), 7 stars, 0 forks, 0 watchers,
  0 externally-filed issues — and **not one mention of Windows** in the 18 most recent issues/PRs. A port multiplies a base
  indistinguishable from zero: 23 × 1.66 ≈ 38.
- **There is no instrument that could ever fire a "Windows demand" trigger**: `landing/index.html` has zero analytics, the site
  is static GitHub Pages, and the app is deliberately telemetry-free.
- **The niche is occupied.** Meetily (MIT, Rust+Tauri — *our stack*, mac+Windows, no bot, Whisper+Ollama local, Markdown export,
  free forever, **26.7k stars**); Vibe 6.9k; Buzz 20.4k. Granola already ships Windows with a free tier. On Windows our
  differentiator **cannot** be "local and private" — Meetily says exactly that — it has to be the vault/graph/lock/E2EE-share
  stack, which is far harder to explain in a README diff.

---

## Fit with Murmur's non-negotiable constraints

| Constraint | Verdict on Windows |
| --- | --- |
| **Local-first / privacy** | *Mostly improved on capture, degraded on integrations.* Loopback is fully local with no TCC fragility. But calendar and Reminders can only be had via cloud Graph connectors, and the redaction firewall's fail-open path becomes **more** likely (candle NER without `metal`/`accelerate`). Windows must make the redactor **fail closed** or make the degradation loud in the egress-consent UI. |
| **Obsidian-native / owned files** | *Preserved but riskier.* Windows vaults are far more likely to sit in **OneDrive** than macOS vaults are in iCloud — and whether `GetFileInformationByHandleEx(FileIdInfo)` stays stable across OneDrive sync/rehydrate is **the single highest-leverage unknown in the whole port**, because the entire identity-assertion design for `export/obsidian.rs` anchors on it. |
| **SQLite canonical** | *Preserved.* But `bundled-sqlcipher-vendored-openssl` on MSVC is unverified (needs Perl/NASM; rusqlite #966 open), and byte-compatibility of a Mac-written DB with a Windows-written one (page size, KDF iterations, HMAC) is untested. |
| **Provider seam + redaction firewall** | *Intact as a seam, broken as an implementation.* The default provider does not run at all; its supply-chain vetting silently no-ops; the redactor fails open. All three need fixing **before** a Windows build touches a cloud provider. |
| **The lock model is load-bearing** | *Survives cryptographically, weakens as a property.* No per-app ACL binding on any secret; no reliable screen-share detection; `renameatx_np`/`fclonefileat` need new crash-safety designs. Every one of the ~219 identity assertions needs an independent `lock-security-reviewer` verdict — this is the bulk of the cost. |
| **macOS-first** | *A deliberate departure that dilutes the moat.* Touch-ID-bound secrets, Vision OCR, ScreenCaptureKit/CoreAudio exclude-self fidelity, EventKit and Reminders all degrade or vanish. And "botless capture" stops being a moat the day a Windows build exists. |
| **Honesty bar / DoD** | *Doubles permanently.* Every capture/permission/FFI claim now needs a real Mac **and** a real Windows box with a live meeting app. `scripts/e2e-core.sh` — the only true end-to-end pipeline proof — is `say`-based (macOS TTS) and cannot run on Windows without a checked-in fixture WAV. |

---

## Options and tradeoffs

| Option | Effort | Risk | What it unlocks | Verdict |
| --- | --- | --- | --- | --- |
| **A. Do nothing** | 0 | — | nothing | Leaves the port un-scoped and the `sanitize_title` length cap (#8) unfixed on macOS too. **Weak.** |
| **B. Seam + fail-open audit ONLY, no Windows** | **S–M / 3–5 eng-weeks** | low | `SystemAudioCapture` + `PlatformServices` traits; the 14 shell-outs deleted or wrapped; the redactor's degradation made visible; `device_platform()` de-hardcoded; filename length/CRLF hardening | **RECOMMENDED.** ~60% of the port's seam work, **all landable and testable on the macOS trunk with no Windows machine**. Note honestly: most of its value is *optionality on a future port* plus code hygiene — only #8 fixes something live on macOS. It is cheap and reversible, not urgent. |
| **C. Windows spike (1 week, one laptop)** | **S / 1 eng-week + ~$1k** | low | settles the 6 questions that swing the estimate by 3× | Do this **only if** the trigger in the recommendation fires. |
| **D. Full Windows v1** | **XL / 34–50 eng-weeks** + a permanent **~30–40% tax** on every future feature touching a seam + ~1.5× per release | high | +66% reach on Obsidian desktop | **Not now.** The arithmetic — 34–50 eng-weeks against ~23 lifetime downloads, entering as a 7-star unknown against a 26.7k-star incumbent on our own stack — is the decision, not any technical blocker. |
| **E. Linux-first beachhead** (the 25.07 recommendation) | M | low | forces `#[cfg]`/trait discipline cheaply | **Superseded.** Its premise was that the seam work needs a second OS to force it — Option B gets the same discipline with no second OS, and Linux has an even smaller Obsidian share (6.9%). |

---

## Recommendation and first step

**Do not port to Windows now. Do Option B, and make the trigger countable.**

Three concrete steps, in order:

1. **Run a fail-open audit as a merge gate (~3 days).** Grep every `Command::new`, every `#[cfg(unix)]` guard and every
   `#[cfg(not(target_os = "macos"))]` fallback, and classify each **fail-open vs fail-closed**. Until this exists, *every effort
   number in this document is a floor of unknown depth*.

   **Accuracy note (verified myself, not taken from the agents):** findings #2, #3 and #5 are **latent, not live** — on macOS
   `cfg(unix)` is true, `is_any_screen_captured()` uses the real CoreGraphics probe, and `device_platform() -> "macos"` is
   correct. That is exactly the point: they activate only on a non-macOS build, which is why a
   `cargo check --target x86_64-pc-windows-msvc` census cannot surface them and why the audit must be a *semantic* grep, not a
   compile. The one that is arguably live on macOS today is #8's missing length cap in `sanitize_title` (APFS also caps a
   filename at 255 bytes, and note stems are `YYYY-MM-DD HHmm <LLM-generated title>`).

2. **Land the seam work on the macOS trunk (~3–5 eng-weeks, harness-verified, PR by PR).** `SystemAudioCapture` with the current
   Mac helper as impl #1; a `PlatformServices` trait covering shell-outs, secret storage, capture detection and file identity;
   `FileIdentity` as an explicit abstraction over the ~219 assertions rather than raw `dev`/`ino`. Every PR is testable on macOS.
   This is the only portion with positive expected value if Windows never happens.

3. **Make Windows demand countable (~0.5–1 eng-week).** A standing monthly read of
   `api.github.com/repos/murmur-io/murmur/releases` `download_count` deltas costs **zero dependencies and zero visitor
   tracking** — this research proved it is a usable signal. Optionally a one-click "I'd use this on Windows" on the landing page
   (owner decision: a privacy-first product measuring its visitors is defensible but not automatic). Then **write the trigger
   down**, e.g.:

   > Revisit Windows when any ONE of: (a) ≥25 unsolicited Windows requests; (b) one org that will pay contingent on Windows;
   > (c) macOS downloads exceed ~500/month, i.e. the base is large enough that +66% is a real number.

**If the trigger fires, the first Windows action is a 1-week spike on one physical laptop** (≈$1,000, plus ideally one
Snapdragon X device) answering exactly six questions, each of which swings the estimate by more than it costs to answer:
(i) does endpoint loopback deliver packets during 60 s of silence, and what is the accumulated mic↔loopback desync over a
60-minute Teams call; (ii) does the stream survive a Bluetooth headset connect mid-call; (iii) does loopback on **speakers**
double-transcribe the far side in our dual-stream merge; (iv) does `whisper-rs features=["vulkan"]` actually register the
backend under the forced `BUILD_SHARED_LIBS=OFF`, checked by reading `ggml_backend_reg_count()` — a green `cargo build` proves
nothing; (v) is `KeyCredential::RequestSignAsync` byte-identical across two process launches, and is it available to an
unpackaged app on a **local** (non-MSA) account; (vi) does `GetFileInformationByHandleEx(FileIdInfo)` stay stable across a
OneDrive sync/rehydrate cycle.

---

## Open questions / what could not be verified

**Nothing in this document was measured on Windows hardware.** Beyond the six spike questions above:

- Does WASAPI endpoint loopback trip the Windows 11 "let desktop apps access your microphone" toggle or the mic-in-use privacy
  indicator? (20-minute test; decides onboarding UX.)
- Do Teams/Zoom/Meet render the far side to the `eCommunications`-role endpoint or `eConsole`? Each also has its own in-app
  speaker picker. Decides whether we capture one endpoint, both, or need per-session enumeration.
- Is there **any** user-mode Windows signal for "a screen share is active" good enough to keep `relockOnScreenshare` honest
  (`SetWinEventHook` owner matching, `GraphicsCaptureSession`, the Win11 capture indicator)? If not, the toggle must be greyed
  out on Windows with explicit copy — a shipping decision.
- Does `bundled-sqlcipher-vendored-openssl` build on `windows-latest`, and is a Mac-written DB byte-compatible with a
  Windows-written one?
- Does `std::fs::rename`/`MoveFileExW` reliably replace a `.md` that Obsidian currently holds open? (Obsidian's file-watcher
  share mode is unknown; if it omits `FILE_SHARE_DELETE`, every vault write during an open session fails.)
- Does Tauri's `assetProtocol` glob scope match Windows **backslash** paths against forward-slash patterns, and does NTFS
  case-insensitivity let `.../audio/foo.ENC` bypass the `.enc` **deny** rule? Both are lock-model-relevant failures a green build
  will not show.
- Can SSL.com issue an IV certificate to a **Polish** resident, and does eSigner sign a Tauri NSIS `.exe` / WiX `.msi` intact
  from a Linux runner without corrupting the installer?
- What is the real SmartScreen ramp for an app at Murmur's install volume? Microsoft's "several weeks and hundreds of clean
  installs" could be months for a niche app.

---

## Sources

**External (fetched by the sub-agents):**
- Capture: https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording ·
  https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudiocaptureclient-getbuffer ·
  https://learn.microsoft.com/en-us/windows/win32/api/audioclientactivationparams/ns-audioclientactivationparams-audioclient_process_loopback_params ·
  https://github.com/microsoft/Windows-classic-samples/issues/414 (Teams silence under **process** loopback) ·
  https://github.com/PortAudio/portaudio/issues/935 (silence stall) · cpal 0.16.0 `src/host/wasapi/{device,stream}.rs` ·
  OBS `win-wasapi.cpp` (ClearBuffer, reconnect thread)
- ML: https://github.com/ggml-org/whisper.cpp issue #3750, discussion #2996 · whisper-rs 0.16 / whisper-rs-sys 0.15 `build.rs` ·
  https://docs.rs/mistralrs (feature list — no `vulkan`) · https://github.com/utilityai/llama-cpp-rs · rusqlite #966
- Secrets: https://learn.microsoft.com/en-us/uwp/api/windows.security.credentials.keycredential ·
  Microsoft Hello sample (`RSASignaturePadding.Pkcs1`) · KeePassXC PR #7384, issue #10042 · microsoft/cppwinrt#999 ·
  MS Learn on destructive Hello PIN reset
- Signing/CI: https://learn.microsoft.com/en-us/azure/trusted-signing/ + MS Q&A threads (3-year history; US/CA-only; individual
  onboarding paused) · https://www.ssl.com/ (IV cert, eSigner + GitHub Actions guide) · https://ebourg.github.io/jsign/ ·
  https://www.certum.eu/ (open-source cloud signing)
- Frontend: tauri-apps/tauri#15512 (transparent window kills `backdrop-filter` on Windows), #4531 (Snap Layouts) ·
  MicrosoftEdge/WebView2Feedback#1469, #5392
- Features: https://learn.microsoft.com/en-us/uwp/api/windows.media.ocr · `Language.OCR~~~pl-PL` FOD ·
  https://github.com/ajrcarey/pdfium-render · https://github.com/bblanchon/pdfium-binaries · `appointmentsSystem` restricted capability
- Market: https://api.github.com/repos/obsidianmd/obsidian-releases/releases (installer platform split) ·
  https://api.github.com/repos/murmur-io/murmur (7 stars, 0 forks) + `/releases` (~23 lifetime downloads) ·
  https://gs.statcounter.com/os-market-share/desktop/worldwide (June 2026) · https://survey.stackoverflow.co/2024/technology/ ·
  https://www.granola.ai/ + /pricing + /security · https://github.com/Zackriya-Solutions/meetily ·
  https://github.com/thewh1teagle/vibe · https://github.com/chidiwilliams/buzz

**Repo (`file:line` read at commit `031e225`):**
- Ungated POSIX: `crypto.rs:33,42-45` · `export/obsidian.rs:4,199,886,944-969,1116-1139,1198,1495` · `instance_lock.rs:11-12,25-26` ·
  `audio/spill.rs:35,1790` · `audio/source.rs:9,48-51` · `pipeline.rs:346`
- Build system: `src-tauri/build.rs:14-65,225-256` (unconditional `build_swift_helper`, release `panic!`) ·
  `tauri.conf.json:53-63` (five bundled resources; `assetProtocol` scope hardcoded to `$HOME/Library/Application Support/…`)
- Capture: `audio/system.rs:20,29-49,52-71,115,348` · `audio/tap.rs:1-10,42` · `audio/merge.rs:5-19,41-47,63-79` ·
  `audio/align.rs:78,132` · `audio/output.rs:1-30` · `audio/aec.rs:1-9,220,440` · `sysaudio/sysaudio.swift:58-60` ·
  `audiocap/audiocap.swift:967-978`
- ML/brain: `reason/sidecar.rs:98-138,161-186,424-451` · `crates/murmur-brain/src/main.rs:96-110,202-205,251-283` ·
  `transcribe/model.rs:75-77,84,101-122,157-167` · `settings/config.rs:593-595,1697` · `pipeline.rs:1321-1366,1778-1793`
- Secrets/lock: `secrets/keychain.rs:71,150,166,177-204,280-320,340,360,391,412,1469,1529,1554,1577` · `Cargo.toml:99` ·
  `commands/lock.rs` (zero `cfg`) · `storage/seal_store.rs` (zero `cfg`) · `screenshare.rs:340-344`
- Fail-open class: `summarize/claude_code.rs:562-598,610-626,628-666` · `summarize/redact.rs:87-102,1339-1350` ·
  `share/mod.rs:252-255` · `connectors/mcp_client.rs:270-300` · `commands/reminders.rs:69-120,178-205` ·
  `src/app/features/settings/settings.store.ts:1353` · `src/app/features/detail/detail/detail.component.ts:304`
- FE/shell: `src-tauri/src/lib.rs:1086-1102` · `src/styles.css:166-215,476-500` · `src/design-tokens/glass.css`
- Process: `scripts/ci.sh` · `scripts/e2e-core.sh:1-11` · `scripts/agent-resource-run:228,268,367` · `.claude/hooks/*.sh`
- Server side: `../murmur-server/crates/murmur-protocol/src/dto.rs:148-166` ·
  `../murmur-server/crates/murmur-server/src/routes/auth.rs:52-58`
