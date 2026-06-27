use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    build_sysaudio_sidecar();
    tauri_build::build();
}

/// Compile the ScreenCaptureKit system-audio sidecar (macOS 13+) as a UNIVERSAL
/// (arm64 + x86_64) Mach-O so the distributed universal `.app` runs on both arches.
///
/// Two outputs are produced from the same compile:
///   1. `$OUT_DIR/meetnotes-sysaudio` — exposed via the `SYSAUDIO_BIN` env var (read with
///      `option_env!`) for the DEV fallback path (`audio::system::sidecar_path`).
///   2. `binaries/meetnotes-sysaudio` (a stable in-crate path) — bundled into
///      `Contents/Resources` via `tauri.conf.json` `bundle.resources`, then resolved at
///      RUNTIME. This is the ONLY path that works in a shipped, notarized build; relying on
///      (1) alone was the latent "records mic-only in production" regression.
///
/// Best-effort: missing `swiftc`/SDK → `cargo:warning` and the app records mic-only; a missing
/// x86_64 slice → fall back to a host-arch-only binary (dev still works; a real universal
/// release build on a Mac with full SDKs gets both slices). It never fails `cargo build`.
fn build_sysaudio_sidecar() {
    let src = Path::new("sysaudio/sysaudio.swift");
    println!("cargo:rerun-if-changed=sysaudio/sysaudio.swift");
    println!("cargo:rerun-if-changed=build.rs");
    if !src.exists() {
        return;
    }
    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        return;
    };
    let out_bin = Path::new(&out_dir).join("meetnotes-sysaudio");

    // Prefer a universal (arm64 + x86_64) binary; fall back to a host-arch single slice.
    if !compile_universal(src, &out_dir, &out_bin) && !compile_single(src, &out_bin) {
        return; // warnings already emitted; system-audio capture unavailable
    }

    // Dev fallback path.
    println!("cargo:rustc-env=SYSAUDIO_BIN={}", out_bin.display());

    // Stage a copy at the stable in-crate path that `bundle.resources` points at so a
    // `tauri build` embeds it in `Contents/Resources`.
    let bundled = Path::new("binaries").join("meetnotes-sysaudio");
    if let Some(parent) = bundled.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::copy(&out_bin, &bundled) {
        println!(
            "cargo:warning=could not stage sidecar for bundling ({e}); a release bundle may lack system-audio"
        );
    }
}

/// Compile both arches and `lipo` them into one universal Mach-O. Returns false (so the
/// caller falls back to a single-arch compile) if any slice or the lipo step fails.
fn compile_universal(src: &Path, out_dir: &str, out_bin: &Path) -> bool {
    let mut slices: Vec<PathBuf> = Vec::new();
    for arch in ["arm64", "x86_64"] {
        let slice = Path::new(out_dir).join(format!("meetnotes-sysaudio-{arch}"));
        // Deployment target 13.0: the sidecar uses ScreenCaptureKit (macOS 13+) at top level,
        // so it cannot target lower. The main app keeps minimumSystemVersion 11.0; on 11–12 the
        // sidecar simply won't launch and capture degrades to mic-only.
        let ok = Command::new("swiftc")
            .arg("-O")
            .args(["-target", &format!("{arch}-apple-macos13.0")])
            .arg("-o")
            .arg(&slice)
            .arg(src)
            .args(["-framework", "ScreenCaptureKit", "-framework", "AVFoundation"])
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

/// Host-arch-only fallback (dev-friendly, non-fatal). Still pins the deployment target to
/// macOS 13.0 so even a fallback build carries the correct `minos` (not the build machine's).
fn compile_single(src: &Path, out_bin: &Path) -> bool {
    let host_arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    };
    match Command::new("swiftc")
        .arg("-O")
        .args(["-target", &format!("{host_arch}-apple-macos13.0")])
        .arg("-o")
        .arg(out_bin)
        .arg(src)
        .args(["-framework", "ScreenCaptureKit", "-framework", "AVFoundation"])
        .status()
    {
        Ok(s) if s.success() => true,
        Ok(s) => {
            println!(
                "cargo:warning=sysaudio sidecar compile failed ({s}); system-audio capture unavailable"
            );
            false
        }
        Err(e) => {
            println!("cargo:warning=swiftc not found ({e}); system-audio capture unavailable");
            false
        }
    }
}
