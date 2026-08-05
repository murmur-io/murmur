use std::path::Path;

fn run_artifact_oracle(mode: &str) {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri must have a repository parent");
    let verifier = repository.join("eval/results/verify_local_cloud_quality_artifacts.py");
    let output = std::process::Command::new("python3")
        .arg(&verifier)
        .arg(mode)
        .arg("--selftest")
        .current_dir(repository)
        .output()
        .expect("run synthetic quality artifact oracle");
    assert!(
        output.status.success(),
        "quality artifact oracle failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn final_quality_artifacts_round_trip_offline() {
    run_artifact_oracle("--final");
}
