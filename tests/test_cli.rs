use std::process::Command;

/// Get the path to the built binary.
fn cargo_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_codetracer-fuel-recorder"))
}

#[test]
fn help_succeeds_and_mentions_name() {
    let output = cargo_bin()
        .arg("--help")
        .output()
        .expect("failed to run binary");

    assert!(output.status.success(), "exit code was not 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("codetracer-fuel-recorder"),
        "help output should mention codetracer-fuel-recorder, got:\n{}",
        stdout
    );
}

#[test]
fn version_succeeds_and_contains_version() {
    let output = cargo_bin()
        .arg("--version")
        .output()
        .expect("failed to run binary");

    assert!(output.status.success(), "exit code was not 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("0.1.0"),
        "version output should contain 0.1.0, got:\n{}",
        stdout
    );
}

#[test]
fn record_nonexistent_dir_fails() {
    let output = cargo_bin()
        .args(["record", "/tmp/nonexistent-dir-that-does-not-exist-12345"])
        .output()
        .expect("failed to run binary");

    assert!(
        !output.status.success(),
        "should fail with nonexistent directory"
    );
}

#[test]
fn record_flow_test_creates_output_files() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = temp_dir.path().join("traces");

    // The test-programs/flow_test directory relative to the project root
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_dir = manifest_dir.join("test-programs/flow_test");

    let output = cargo_bin()
        .args([
            "record",
            project_dir.to_str().unwrap(),
            "-o",
            out_dir.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run binary");

    assert!(
        output.status.success(),
        "record should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Check that output files were created
    assert!(
        out_dir.join("trace_metadata.json").exists(),
        "trace_metadata.json should exist"
    );
    assert!(
        out_dir.join("trace_paths.json").exists(),
        "trace_paths.json should exist"
    );

    // Verify trace_metadata.json content
    let metadata: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out_dir.join("trace_metadata.json")).unwrap(),
    )
    .expect("trace_metadata.json should be valid JSON");
    assert_eq!(
        metadata["recorder"],
        "codetracer-fuel-recorder",
        "recorder field should be set"
    );
}
