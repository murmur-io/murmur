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
/// The score-replay manifest deliberately keeps its source-bound strict schema. This separate,
/// Rust-invoked oracle binds that manifest together with R1/R2, combined, the exact synthetic
/// fixture snapshot, complete all-string inventory, and bounded review projection, then runs
/// mutation selftests. The projection keeps the large generated JSON reviewable without becoming
/// a second source of truth: the Python oracle must regenerate it exactly from the bound artifacts.
fn final_quality_artifacts_fixture_inventory_and_review_projection_round_trip_offline() {
    run_artifact_oracle("--final");
}

#[test]
fn historical_quality_artifacts_round_trip_offline() {
    run_artifact_oracle("--history");
}
