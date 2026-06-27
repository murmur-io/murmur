use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
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
    tauri_build::build();
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
/// Best-effort: missing `swiftc`/SDK → `cargo:warning` (capture degrades); a missing x86_64 slice
/// → fall back to a host-arch-only binary. It never fails `cargo build`.
fn build_swift_helper(
    src_rel: &str,
    bin: &str,
    env_var: &str,
    deploy_target: &str,
    frameworks: &[&str],
) {
    let src = Path::new(src_rel);
    println!("cargo:rerun-if-changed={src_rel}");
    println!("cargo:rerun-if-changed=build.rs");
    if !src.exists() {
        return;
    }
    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        return;
    };
    let out_bin = Path::new(&out_dir).join(bin);

    if !compile_universal(src, &out_dir, &out_bin, deploy_target, frameworks)
        && !compile_single(src, &out_bin, deploy_target, frameworks)
    {
        return; // warnings already emitted; this helper is unavailable
    }

    // Dev fallback path.
    println!("cargo:rustc-env={env_var}={}", out_bin.display());

    // Stage a copy at the stable in-crate path that `bundle.resources` embeds.
    let bundled = Path::new("binaries").join(bin);
    if let Some(parent) = bundled.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::copy(&out_bin, &bundled) {
        println!(
            "cargo:warning=could not stage {bin} for bundling ({e}); a release bundle may lack it"
        );
    }
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
        .args(["-target", &format!("{host_arch}-apple-macos{deploy_target}")])
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
