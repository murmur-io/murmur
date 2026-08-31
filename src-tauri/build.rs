use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // macOS final-link fix (Apple clang 21+): whisper's bundled ggml-metal
    // (`ggml-metal-device.m`) uses ObjC `@available`, which clang lowers to a call to
    // `__isPlatformVersionAtLeast`. That symbol lives in clang's compiler-rt
    // (`libclang_rt.osx.a`). rustc drives the final app link with `-nodefaultlibs`, so the
    // runtime is NOT auto-added and the binary fails with
    // "Undefined symbols: ___isPlatformVersionAtLeast". Link the archive explicitly, resolved
    // from the ACTIVE clang so it tracks CLT/Xcode updates. Only the whisper-linking app crate
    // needs it (the sibling brain crate uses mistralrs/candle, not whisper.cpp Metal).
    #[cfg(target_os = "macos")]
    link_clang_rt_osx();

    // ScreenCaptureKit sidecar (macOS 13+) — the system-audio FALLBACK path.
    build_swift_helper(
        "sysaudio/sysaudio.swift",
        "meetnotes-sysaudio",
        "SYSAUDIO_BIN",
        "13.0",
        &["ScreenCaptureKit", "AVFoundation"],
    );
    // Core Audio PROCESS TAP helper (macOS 14.4+) — the PREMIUM system-audio path.
    build_swift_helper(
        "audiocap/audiocap.swift",
        "meetnotes-audiocap",
        "AUDIOCAP_BIN",
        "14.4",
        &["AVFoundation", "CoreAudio", "Foundation"],
    );
    // VPIO AEC mic helper (echo cancellation; macOS 10.15+, app floor 13.4) — the AEC ASR feed.
    build_swift_helper(
        "aeccap/aeccap.swift",
        "meetnotes-aeccap",
        "AECCAP_BIN",
        "13.4",
        &["AVFoundation", "Foundation"],
    );
    // EventKit Calendar context helper (app floor 13.4; uses the macOS 14+ full-access API at
    // runtime, guarded). Surfaces local meeting context — title, attendees, agenda — zero-OAuth,
    // on-device. Crash-safe SEPARATE process: a missing permission / no events → a graceful JSON
    // envelope, never an app crash.
    build_swift_helper(
        "calendar/calendar.swift",
        "meetnotes-calendar",
        "CALENDAR_BIN",
        "13.4",
        &["EventKit", "Foundation"],
    );
    // WS2 (DEFERRED) — Apple Foundation Models reasoner sidecar. `afm/afm.swift` does NOT exist yet:
    // it needs the macOS 26 SDK (`import FoundationModels`, `@Generable`/`DynamicGenerationSchema`),
    // which this CLT-only Mac lacks. `build_swift_helper` EARLY-RETURNS when the source is absent
    // (see the `!src.exists()` guard), so this line is a HARMLESS NO-OP today and compiles nothing.
    // Once `afm/afm.swift` is written on a signed macOS-26 machine this stages `binaries/meetnotes-afm`
    // (arm64; the x86_64 slice fails FoundationModels → the single-arch fallback), and ONLY THEN may a
    // `bundle.resources` entry be added to `tauri.conf.json` (tauri_build validates the binary exists,
    // so that entry cannot land before the compile).
    build_swift_helper(
        "afm/afm.swift",
        "meetnotes-afm",
        "AFM_BIN",
        "26.0",
        &["FoundationModels"],
    );
    // Brain sidecar (`crates/murmur-brain` → `murmur-brain`): STAGE-IF-EXISTS the pre-built
    // binary into `binaries/murmur-brain` + emit `BRAIN_BIN` for the dev fallback. We DO NOT
    // shell `cargo build -p murmur-brain` here (nested-cargo-in-build.rs = re-entrancy / target-lock
    // hazard): release builds the child via `tauri.conf.json` `beforeBuildCommand`, and this just
    // copies it. If it is ABSENT (e.g. during `cargo test --lib`, which never builds the child) this
    // is a HARMLESS no-op — the resource entry is likewise release-only (see tauri.conf.json).
    stage_brain_sidecar();
    tauri_build::build();
}

/// Link Apple clang's compiler-rt archive (`libclang_rt.osx.a`) into the final app link so the
/// `__isPlatformVersionAtLeast` symbol emitted by whisper's ObjC `@available` checks resolves.
/// The path is resolved from the active clang (`--print-file-name`) so a CLT/Xcode version bump
/// is followed automatically. Applies only to the app crate's final link (`cargo:rustc-link-arg`),
/// so no dependency is recompiled. Best-effort: if the archive cannot be resolved, warn rather than
/// fail (a link error would still surface loudly, and non-macOS builds skip this entirely).
#[cfg(target_os = "macos")]
fn link_clang_rt_osx() {
    let cc = std::env::var("CC").unwrap_or_else(|_| "clang".to_string());
    if let Ok(output) = Command::new(&cc)
        .arg("--print-file-name=libclang_rt.osx.a")
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let resolved = Path::new(&path);
            if resolved.is_absolute() && resolved.exists() {
                println!("cargo:rustc-link-arg={path}");
                return;
            }
        }
    }
    println!(
        "cargo:warning=could not resolve libclang_rt.osx.a via {cc}; the app link may fail on \
         __isPlatformVersionAtLeast (whisper ggml-metal @available)"
    );
}

/// STAGE the `murmur-brain` child binary into the stable in-crate `binaries/murmur-brain`
/// path (the `bundle.resources` mount point), by profile:
///
/// - RELEASE bundle: the REAL UNIVERSAL binary is staged by `tauri.conf.json`'s `beforeBuildCommand`
///   (two per-arch `cargo build -p murmur-brain --release --target …` + a `lipo -create … -output
///   src-tauri/binaries/murmur-brain`), which runs ONCE *before* `tauri build` compiles the app —
///   i.e. before THIS build.rs runs. So on a real release build `binaries/murmur-brain` already
///   exists as the correct universal Mach-O when we get here, and we MUST NOT touch it. Overwriting
///   it with a stale HOST-ARCH-only `target/release/murmur-brain` (from a prior dev child build) or
///   with an empty placeholder is exactly BLOCKER-8: the universal DMG would ship a dead brain.
/// - DEV (`cargo build` / `npm run dev`, host-arch): stage the host-arch `target/{debug,release}`
///   child IF present, and emit `cargo:rustc-env=BRAIN_BIN=<built>` so the dev fallback in
///   `resolve_bin` can spawn it. Only refresh `binaries/…` from the host-arch build when the current
///   `binaries/…` is ABSENT or is the empty test placeholder — never clobber a real (universal or
///   larger) staged binary.
/// - `cargo test --lib` (runs build.rs, never `beforeBuildCommand`, never builds the child): if
///   nothing is present, write an EMPTY placeholder so `tauri_build`'s resource-existence check passes.
///   The placeholder is a TEST/DEV-ONLY sentinel; a real bundle ALWAYS gets the real universal binary
///   from the `beforeBuildCommand`-before-build.rs ordering above.
///
/// INVARIANT (BLOCKER-8): NEVER overwrite an already-present REAL `binaries/murmur-brain` with a
/// placeholder or a smaller/host-arch binary. Best-effort otherwise: a missing binary in a dev/test
/// build is a silent no-op so `cargo test --lib` is NEVER blocked.
fn stage_brain_sidecar() {
    const BIN: &str = "murmur-brain";
    println!("cargo:rerun-if-changed=build.rs");

    let bundled = Path::new("binaries").join(BIN);
    if let Some(parent) = bundled.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Is a REAL binary already staged? The empty placeholder we write below is 0 bytes; a real child
    // (host-arch dev build ~200+ MB, or the universal release binary) is far from empty. Treat any
    // non-empty `binaries/…` as already-staged and DO NOT clobber it (the release universal binary
    // staged by `beforeBuildCommand` lands here BEFORE build.rs runs — see the fn doc).
    let already_real = std::fs::metadata(&bundled)
        .map(|m| m.len() > 0)
        .unwrap_or(false);

    // Resolve the workspace target dir. `OUT_DIR` is `<target>/<profile>/build/murmur-*/out`;
    // climb to `<target>` then probe the profile subdirs. Honor `CARGO_TARGET_DIR` if set.
    let target_dir: Option<PathBuf> = std::env::var("CARGO_TARGET_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("OUT_DIR").ok().and_then(|out| {
                // out = <target>/<profile>/build/<pkg>-<hash>/out → 4 parents up = <target>.
                let mut p = PathBuf::from(out);
                for _ in 0..4 {
                    p = p.parent()?.to_path_buf();
                }
                Some(p)
            })
        });

    // Prefer release; also probe the universal-apple-darwin release the shipped bundle uses, then a
    // debug fallback for a dev child build. First existing wins. This ONLY finds a HOST-ARCH child in
    // a normal `cargo build -p murmur-brain` dev build (a target-triple build writes under
    // `target/<triple>/…`, which the release path stages via `beforeBuildCommand`, not here).
    // Require a NON-EMPTY file: a half-written / interrupted child build can leave a 0-byte
    // `target/<profile>/murmur-brain`. A bare `is_file()` would treat that empty stub as a real
    // child, then `fs::copy` it into `binaries/…` on EVERY build (since `already_real` — which also
    // gates on `len() > 0` — never flips true for a 0-byte copy), re-arming the dev watcher into an
    // infinite rebuild loop, and would emit a `BRAIN_BIN` pointing at an unspawnable empty binary.
    let built = target_dir.as_ref().and_then(|td| {
        [
            td.join("release").join(BIN),
            td.join("universal-apple-darwin").join("release").join(BIN),
            td.join("debug").join(BIN),
        ]
        .into_iter()
        .find(|p| {
            std::fs::metadata(p)
                .map(|m| m.is_file() && m.len() > 0)
                .unwrap_or(false)
        })
    });

    match built {
        Some(built) => {
            // A real host-arch child was built (dev `-p murmur-brain`). Expose the DEV fallback path so
            // `resolve_bin` can spawn it directly. Only refresh the staged `binaries/…` when the
            // current one is ABSENT or the empty placeholder — NEVER clobber an already-real (e.g.
            // universal, staged by `beforeBuildCommand`) binary.
            println!("cargo:rustc-env=BRAIN_BIN={}", built.display());
            println!("cargo:rerun-if-changed={}", built.display());
            if !already_real {
                if let Err(e) = std::fs::copy(&built, &bundled) {
                    println!(
                        "cargo:warning=could not stage {BIN} for bundling ({e}); a release bundle may lack it"
                    );
                }
            }
        }
        None => {
            // Not built yet (the COMMON case during `cargo test --lib`, which never builds the child).
            // `tauri_build` validates every `bundle.resources` path EXISTS — even in a test build — so
            // create an EMPTY placeholder ONLY if none is present. The placeholder is a TEST/DEV-ONLY
            // sentinel; a real release bundle ALWAYS gets the real UNIVERSAL binary from the
            // `beforeBuildCommand` `lipo` (which runs BEFORE this build.rs), so we must NOT overwrite it.
            // NO `BRAIN_BIN` is emitted here, so a dev run without a built child resolves no binary and
            // the reasoner degrades to the stub.
            if !bundled.exists() {
                let _ = std::fs::write(&bundled, b"");
            }
        }
    }
}

/// Compile a Swift helper as a UNIVERSAL (arm64 + x86_64) Mach-O so the distributed universal
/// `.app` runs on both arches, at deployment target `deploy_target`.
///
/// Two outputs from the same compile:
///   1. `$OUT_DIR/<bin>` — exposed via the `<env_var>` env var for the DEV fallback path.
///   2. `binaries/<bin>` (a stable in-crate path) — bundled into `Contents/Resources` via
///      `tauri.conf.json` `bundle.resources`, then resolved at RUNTIME. This is the ONLY path
///      that works in a shipped, notarized build.
///
/// Dev builds the HOST SLICE ONLY and never lipos: a dev run executes the `OUT_DIR` binary on this
/// machine, so the x86_64 slice is pure build cost (measured ~0.6 s per slice, ~2.4 s per build-script
/// run across the four helpers). It still falls back to a universal compile if the single-arch one
/// fails. Release is strict and unchanged: every present/bundled helper source must freshly compile
/// as arm64+x86_64 or the build aborts; a stale staged helper must never make a nominally universal
/// DMG pass — and release ALWAYS refreshes `binaries/<bin>`, so a single-arch dev artifact left
/// there cannot survive into a bundle.
fn build_swift_helper(
    src_rel: &str,
    bin: &str,
    env_var: &str,
    deploy_target: &str,
    frameworks: &[&str],
) {
    let src = Path::new(src_rel);
    println!("cargo:rerun-if-changed=build.rs");
    // NEVER declare `rerun-if-changed` on a path that does not exist. Cargo reports a missing
    // watched path as `StaleItem::MissingFile`, which is PERMANENT staleness: the build script is
    // re-run on EVERY cargo invocation, and because it can emit `rustc-env`/`rustc-link-arg` the
    // whole crate is recompiled and relinked with it. `afm/afm.swift` is deliberately absent (it
    // needs the macOS 26 SDK), and declaring it cost a measured 18.6 s on EVERY `cargo test`,
    // `cargo clippy`, `cargo build` and dev-watcher rebuild in this repo — a no-op build went from
    // 18.6 s to 0.33 s once the declaration moved below this guard. The trade-off is deliberate:
    // creating a helper source later needs a `build.rs` touch (still watched, one line above) to
    // be picked up, which is unavoidable anyway since a new helper also needs its own
    // `build_swift_helper` call. Regression oracle: the incremental-no-op check in `scripts/ci.sh`.
    if !src.exists() {
        return;
    }
    println!("cargo:rerun-if-changed={src_rel}");
    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        return;
    };
    let out_bin = Path::new(&out_dir).join(bin);
    let is_release = std::env::var("PROFILE").as_deref() == Ok("release");

    if is_release {
        if !compile_universal(src, &out_dir, &out_bin, deploy_target, frameworks)
            || !is_universal_macho(&out_bin)
        {
            panic!(
                "release helper {bin} was not freshly built as universal arm64+x86_64; refusing stale/single-arch bundle"
            );
        }
    } else if !compile_single(src, &out_bin, deploy_target, frameworks)
        && !compile_universal(src, &out_dir, &out_bin, deploy_target, frameworks)
    {
        return; // warnings already emitted; this helper is unavailable in dev
    }

    // Dev fallback path.
    println!("cargo:rustc-env={env_var}={}", out_bin.display());

    // Stage a copy at the stable in-crate path that `bundle.resources` embeds — and that
    // `tauri_build` validates exists, even in dev. RELEASE refreshes it every build; DEV only
    // CREATES it when ABSENT. Rewriting it each build would retrigger Tauri's dev file-watcher
    // (resources are registered `rerun-if-changed`) into an infinite rebuild loop. In dev the
    // helper actually executes via the `OUT_DIR` env fallback above; this staged copy only
    // satisfies the resource check.
    let bundled = Path::new("binaries").join(bin);
    if is_release || !bundled.exists() {
        if let Some(parent) = bundled.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::copy(&out_bin, &bundled) {
            if is_release {
                panic!("could not stage universal release helper {bin}: {e}");
            }
            println!("cargo:warning=could not stage {bin} for dev bundling ({e})");
        }
        if is_release && !is_universal_macho(&bundled) {
            panic!("staged release helper {bin} is not universal arm64+x86_64");
        }
    }
}

fn is_universal_macho(path: &Path) -> bool {
    let Ok(output) = Command::new("lipo").arg("-archs").arg(path).output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let Ok(arches) = String::from_utf8(output.stdout) else {
        return false;
    };
    let mut has_arm64 = false;
    let mut has_x86_64 = false;
    for arch in arches.split_whitespace() {
        has_arm64 |= arch == "arm64";
        has_x86_64 |= arch == "x86_64";
    }
    has_arm64 && has_x86_64
}

fn framework_args(frameworks: &[&str]) -> Vec<String> {
    let mut v = Vec::with_capacity(frameworks.len() * 2);
    for f in frameworks {
        v.push("-framework".to_string());
        v.push((*f).to_string());
    }
    v
}

/// Compile both arches and `lipo` them into one universal Mach-O. Returns false (so the caller
/// falls back to a single-arch compile) if any slice or the lipo step fails.
fn compile_universal(
    src: &Path,
    out_dir: &str,
    out_bin: &Path,
    deploy_target: &str,
    frameworks: &[&str],
) -> bool {
    let stem = out_bin
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("helper");
    let mut slices: Vec<PathBuf> = Vec::new();
    for arch in ["arm64", "x86_64"] {
        let slice = Path::new(out_dir).join(format!("{stem}-{arch}"));
        let ok = Command::new("swiftc")
            .arg("-O")
            .args(["-target", &format!("{arch}-apple-macos{deploy_target}")])
            .arg("-o")
            .arg(&slice)
            .arg(src)
            .args(framework_args(frameworks))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return false;
        }
        slices.push(slice);
    }
    let mut cmd = Command::new("lipo");
    cmd.arg("-create");
    for slice in &slices {
        cmd.arg(slice);
    }
    cmd.arg("-output").arg(out_bin);
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

/// Host-arch-only fallback (dev-friendly, non-fatal). Still pins the deployment target so even a
/// fallback build carries the correct `minos` (not the build machine's).
fn compile_single(src: &Path, out_bin: &Path, deploy_target: &str, frameworks: &[&str]) -> bool {
    let host_arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    };
    match Command::new("swiftc")
        .arg("-O")
        .args([
            "-target",
            &format!("{host_arch}-apple-macos{deploy_target}"),
        ])
        .arg("-o")
        .arg(out_bin)
        .arg(src)
        .args(framework_args(frameworks))
        .status()
    {
        Ok(s) if s.success() => true,
        Ok(s) => {
            println!(
                "cargo:warning=swift helper {} compile failed ({s}); that capture path unavailable",
                out_bin.display()
            );
            false
        }
        Err(e) => {
            println!("cargo:warning=swiftc not found ({e}); swift capture helpers unavailable");
            false
        }
    }
}
