use std::path::Path;
use std::process::Command;

fn main() {
    build_sysaudio_sidecar();
    tauri_build::build();
}

/// Best-effort compile of the ScreenCaptureKit system-audio sidecar (macOS 13+).
///
/// On success, exposes the binary path via the `SYSAUDIO_BIN` env var (read at compile
/// time with `option_env!`). On failure (no `swiftc` / SDK) it emits a `cargo:warning`
/// and continues — the runtime `audio::system::SystemAudioRecorder` then reports
/// "unavailable" and the app records mic-only. This keeps `cargo build` robust on
/// machines without a Swift toolchain while building + bundling the sidecar when present.
fn build_sysaudio_sidecar() {
    let src = Path::new("sysaudio/sysaudio.swift");
    println!("cargo:rerun-if-changed=sysaudio/sysaudio.swift");
    if !src.exists() {
        return;
    }
    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        return;
    };
    let bin = Path::new(&out_dir).join("meetnotes-sysaudio");

    let status = Command::new("swiftc")
        .arg("-O")
        .arg("-o")
        .arg(&bin)
        .arg(src)
        .arg("-framework")
        .arg("ScreenCaptureKit")
        .arg("-framework")
        .arg("AVFoundation")
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("cargo:rustc-env=SYSAUDIO_BIN={}", bin.display());
        }
        Ok(s) => println!(
            "cargo:warning=sysaudio sidecar compile failed ({s}); system-audio capture unavailable"
        ),
        Err(e) => {
            println!("cargo:warning=swiftc not found ({e}); system-audio capture unavailable")
        }
    }
}
