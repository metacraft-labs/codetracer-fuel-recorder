//! Integration tests for the FuelVM trace recorder.
//!
//! Tests cover three areas:
//!
//! 1. **Recorder API** — drive `FuelRecorder::record` directly with a
//!    fuel-asm-built bytecode program and assert the resulting `.ct`
//!    container exists and starts with the canonical CTFS magic bytes.
//!    These tests do not depend on `forc`.
//! 2. **`ct print` content** — record a fixture and pipe the resulting
//!    `.ct` container through `ct print` from
//!    `codetracer-trace-format-nim` to make content-level assertions.
//!    Skips gracefully when `ct-print` is not present (i.e. when this
//!    crate is built outside the metacraft workspace).
//! 3. **CLI env-var contract** — exercise the post-2026-05-08
//!    `CODETRACER_FUEL_RECORDER_OUT_DIR` /
//!    `CODETRACER_FUEL_RECORDER_DISABLED` env vars and the
//!    no-`--format` invariant from `Recorder-CLI-Conventions.md` §4 / §5.
//!    These invoke the recorder binary via `CARGO_BIN_EXE_*`.
//!
//! History note: pre-2026-05-08 the recorder shipped a `--format
//! ctfs|binary|json` flag and the JSON-content tests in this file
//! parsed a `trace.json` file directly.  The 2026-05-02 audit (M33-style)
//! had already migrated the on-disk shape to a single `.ct`
//! multi-stream container; the JSON-content assertions were stubbed
//! out (`parse_trace_json` returned an empty vector and each test
//! short-circuited with `if events.is_empty() { return; }`) but the
//! tests were still gated on `--format json` being accepted.  When the
//! convention switched to CTFS-only the `--format` argument was
//! removed and those stubbed JSON-content tests were rewritten as
//! pure structural assertions on the produced `.ct` container.  The
//! content coverage they used to provide is now delivered by the
//! `test_recorded_trace_via_ct_print_json` test below (which goes
//! through `ct print` rather than reading a recorder-emitted
//! `trace.json`).  See `AUDIT-CTFS-2026-05.md` ("Convention
//! compliance follow-up") for the full record.

use std::path::PathBuf;
use std::process::Command;

use fuel_asm::{op, RegId};

use codetracer_fuel_recorder::recorder::FuelRecorder;
use codetracer_fuel_recorder::source_map::SwaySourceMap;

/// CTFS magic header bytes — see `codetracer-trace-format-spec/`.
const CTFS_MAGIC: [u8; 5] = [0xC0, 0xDE, 0x72, 0xAC, 0xE2];

/// Build the simple arithmetic test bytecode:
///   r16 = 10, r17 = 32, r18 = r16 + r17 = 42,
///   r19 = r18 * 2 = 84, r20 = r19 + r16 = 94,
///   log(r20), ret
fn simple_arithmetic_bytecode() -> Vec<u8> {
    vec![
        op::movi(0x10, 10),              // r16 = 10
        op::movi(0x11, 32),              // r17 = 32
        op::add(0x12, 0x10, 0x11),       // r18 = r16 + r17 = 42
        op::muli(0x13, 0x12, 2),         // r19 = r18 * 2 = 84
        op::add(0x14, 0x13, 0x10),       // r20 = r19 + r16 = 94
        op::log(0x14, 0x00, 0x00, 0x00), // log final result
        op::ret(RegId::ONE),             // return
    ]
    .into_iter()
    .collect()
}

/// Create a synthetic source map that maps each instruction to a line number.
fn synthetic_source_map(source_path: &PathBuf, num_instructions: usize) -> SwaySourceMap {
    let entries: Vec<(usize, PathBuf, u32)> = (0..num_instructions)
        .map(|i| (i, source_path.clone(), (i + 1) as u32))
        .collect();
    SwaySourceMap::from_line_mapping(entries)
}

/// Run the recorder with simple arithmetic bytecode and return the output dir.
fn run_simple_trace() -> tempfile::TempDir {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = temp_dir.path().join("traces");
    let source_path = PathBuf::from("/tmp/test_arithmetic.sw");
    let bytecode = simple_arithmetic_bytecode();
    let num_instructions = bytecode.len() / 4;
    let source_map = synthetic_source_map(&source_path, num_instructions);

    let recorder = FuelRecorder::new("test_arithmetic", &out_dir);
    recorder
        .record(bytecode, &source_map, &source_path)
        .expect("recording should succeed");

    temp_dir
}

/// Helper: collect every `.ct` file in `out_dir`.
fn ct_files_in(out_dir: &std::path::Path) -> Vec<PathBuf> {
    std::fs::read_dir(out_dir)
        .expect("read_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "ct"))
        .collect()
}

/// Path to the `ct-print` binary shipped with `codetracer-trace-format-nim`.
///
/// The Fuel recorder is CTFS-only; tests that need to make
/// content-level assertions on a recorded trace pipe the `.ct`
/// container through `ct-print --json` and assert on the resulting
/// JSON.  This is the same workflow that `Recorder-CLI-Conventions.md`
/// §4 prescribes for downstream tools / golden snapshots.
fn ct_print_path() -> PathBuf {
    // The trace-format-nim sibling lives at a fixed relative path within
    // the workspace.  Tests skip gracefully when it's not present (e.g.
    // when this crate is built outside the metacraft workspace).
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("codetracer-trace-format-nim")
        .join("ct-print")
}

// ===========================================================================
// Recorder API smoke tests
// ===========================================================================

#[test]
fn test_fuel_basic_execution() {
    let temp_dir = run_simple_trace();
    let out_dir = temp_dir.path().join("traces");

    // Assert the output directory exists
    assert!(out_dir.exists(), "output directory should exist");

    // Assert .ct trace file exists with CTFS magic.
    let ct_files = ct_files_in(&out_dir);
    assert!(!ct_files.is_empty(), ".ct should exist");
    let bytes = std::fs::read(&ct_files[0]).unwrap();
    assert!(bytes.len() >= 5);
    assert_eq!(&bytes[..5], &CTFS_MAGIC);
}

#[test]
fn test_fuel_source_mapping() {
    // Structural smoke test: the recorder runs to completion against the
    // synthetic source map and emits a non-empty .ct container.
    // Per-step / per-line content assertions live in the
    // `test_recorded_trace_via_ct_print_json` ct-print round-trip.
    let temp_dir = run_simple_trace();
    let out_dir = temp_dir.path().join("traces");

    let ct_files = ct_files_in(&out_dir);
    assert!(!ct_files.is_empty(), "expected .ct file in {:?}", out_dir);
    let bytes = std::fs::read(&ct_files[0]).unwrap();
    assert!(
        bytes.len() > 256,
        "synthetic-source-map run should emit >256 bytes of CTFS payload, got {}",
        bytes.len()
    );
    assert_eq!(&bytes[..5], &CTFS_MAGIC);
}

#[test]
fn test_fuel_variable_extraction() {
    // Structural smoke test: the variable-extraction path runs end-to-end
    // and emits a populated .ct container.  Per-variable content
    // assertions live in `test_recorded_trace_via_ct_print_json` (and in
    // `test_variable_tracker.rs`'s ABI tests at the API layer).
    let temp_dir = run_simple_trace();
    let out_dir = temp_dir.path().join("traces");

    let ct_files = ct_files_in(&out_dir);
    assert!(!ct_files.is_empty(), "expected .ct file in {:?}", out_dir);
    let bytes = std::fs::read(&ct_files[0]).unwrap();
    assert_eq!(&bytes[..5], &CTFS_MAGIC);
}

#[test]
fn test_fuel_trace_3file_output() {
    let temp_dir = run_simple_trace();
    let out_dir = temp_dir.path().join("traces");

    // Verify .ct output with CTFS magic bytes.
    let ct_files = ct_files_in(&out_dir);
    assert!(!ct_files.is_empty(), "expected .ct file");
    let ct_content = std::fs::read(&ct_files[0]).unwrap();
    assert!(ct_content.len() >= 5);
    assert_eq!(&ct_content[..5], &CTFS_MAGIC);
}

#[test]
fn test_fuel_single_step_trace() {
    // Structural smoke test: the single-stepping interpreter path runs
    // end-to-end without panicking and emits a populated .ct container.
    let temp_dir = run_simple_trace();
    let out_dir = temp_dir.path().join("traces");

    let ct_files = ct_files_in(&out_dir);
    assert!(!ct_files.is_empty(), "expected .ct file");
    let bytes = std::fs::read(&ct_files[0]).unwrap();
    assert!(
        bytes.len() > 256,
        "7-instruction single-stepped run should emit >256 bytes of CTFS payload, got {}",
        bytes.len()
    );
    assert_eq!(&bytes[..5], &CTFS_MAGIC);
}

// ===========================================================================
// CTFS content via `ct-print` — replaces the legacy `--format json` content
// assertions
// ===========================================================================

/// Record the simple-arithmetic bytecode, then convert the produced `.ct`
/// container to JSON via `ct-print --json` and assert on the textual
/// representation.
///
/// Pre-2026-05-08 the recorder shipped a `--format json` mode and this
/// suite asserted on a recorder-emitted `trace.json` file.  The
/// convention now mandates CTFS-only output; `ct print` is the
/// canonical conversion tool.  See `Recorder-CLI-Conventions.md` §4.
///
/// The Fuel recorder's variable payload (general-purpose register
/// values encoded as `ValueRecord::Int { i, type_id }`) does not
/// round-trip through `ct print --json` today (same pre-existing
/// limitation as cardano / circom / flow), so this test asserts on
/// **structural anchors** — the source-path file name and at least one
/// of the inferred register / immediate variable names — rather than
/// on integer values.
#[test]
fn test_recorded_trace_via_ct_print_json() {
    let ct_print = ct_print_path();
    if !ct_print.exists() {
        eprintln!(
            "SKIP: ct-print not found at {} — only available within the \
             metacraft workspace where codetracer-trace-format-nim is a sibling.",
            ct_print.display()
        );
        return;
    }

    let temp_dir = run_simple_trace();
    let out_dir = temp_dir.path().join("traces");
    let ct_files = ct_files_in(&out_dir);
    assert!(
        !ct_files.is_empty(),
        "expected a .ct container in {:?}",
        out_dir
    );

    // ct-print --json <file.ct>
    let output = Command::new(&ct_print)
        .args(["--json"])
        .arg(&ct_files[0])
        .output()
        .expect("failed to run ct-print");

    assert!(
        output.status.success(),
        "ct-print should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.is_empty(), "ct-print --json produced empty output");

    // Structural anchor 1: the synthetic source path appears in the
    // path stream rendered by ct-print.
    assert!(
        stdout.contains("test_arithmetic.sw"),
        "ct-print --json output should mention the synthetic source path \
         (test_arithmetic.sw); got:\n{stdout}"
    );

    // Structural anchor 2: at least one of the recorder's variable
    // names should appear.  The variable-tracker infers `imm_<value>`
    // names from MOVI immediates; otherwise the recorder falls back to
    // the raw register name `r16`..`r23`.  We accept either shape.
    let variable_anchor = ["imm_10", "imm_32", "r16", "r17", "r18", "r19", "r20"]
        .iter()
        .any(|v| stdout.contains(v));
    assert!(
        variable_anchor,
        "ct-print --json output should mention at least one of the \
         recorder's variable names (imm_10/imm_32/r16..r20); got:\n{stdout}"
    );
}

// ===========================================================================
// CLI env-var contract
// ===========================================================================

/// Path to the test-programs/flow_test directory.  This is the same
/// fixture the CLI smoke test in `tests/test_cli.rs` uses, so we know
/// the recorder can complete end-to-end against it without a `forc`
/// invocation.
fn flow_test_project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test-programs/flow_test")
}

/// `CODETRACER_FUEL_RECORDER_OUT_DIR` must be honoured as a fallback
/// for `--out-dir`.  Convention: `Recorder-CLI-Conventions.md` §5.
///
/// We exercise the env var via the placeholder Sway-project `record`
/// path (which writes `trace_metadata.json` / `trace_paths.json` into
/// `--out-dir` even though the CTFS pipeline is not yet wired in for
/// Forc projects).  This is enough to prove the env-var fallback is
/// honoured at the `--out-dir` resolution step.
#[test]
fn test_env_out_dir_used_when_flag_omitted() {
    let tmp_dir = tempfile::tempdir().expect("tempdir");
    let env_out_dir = tmp_dir.path().join("via-env");

    let project_dir = flow_test_project_dir();

    let output = Command::new(env!("CARGO_BIN_EXE_codetracer-fuel-recorder"))
        .args(["record"])
        .arg(&project_dir)
        .env("CODETRACER_FUEL_RECORDER_OUT_DIR", &env_out_dir)
        // Make sure the env-var doesn't bleed in from the developer's shell.
        .env_remove("CODETRACER_FUEL_RECORDER_DISABLED")
        .output()
        .expect("failed to run recorder");

    assert!(
        output.status.success(),
        "recorder should succeed when CODETRACER_FUEL_RECORDER_OUT_DIR is set; \
         stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The placeholder Sway-project path writes trace_metadata.json
    // into the resolved out-dir.  The env-var must have been the
    // fallback (since --out-dir was omitted).
    assert!(
        env_out_dir.join("trace_metadata.json").exists(),
        "expected the env-supplied output dir {:?} to receive the placeholder \
         trace_metadata.json",
        env_out_dir
    );
}

/// `CODETRACER_FUEL_RECORDER_DISABLED=1` must skip recording entirely.
/// The recorder process should still exit 0 (the Fuel recorder doesn't
/// run a separate target subprocess — it executes the Sway project /
/// bytecode itself — so "disabled" simply means "don't write any
/// trace artefacts").
#[test]
fn test_env_disabled_skips_recording() {
    let tmp_dir = tempfile::tempdir().expect("tempdir");
    let out_dir = tmp_dir.path().join("should-stay-empty");

    let project_dir = flow_test_project_dir();

    let output = Command::new(env!("CARGO_BIN_EXE_codetracer-fuel-recorder"))
        .args(["record"])
        .arg(&project_dir)
        .args(["--out-dir"])
        .arg(&out_dir)
        .env("CODETRACER_FUEL_RECORDER_DISABLED", "1")
        .output()
        .expect("failed to run recorder");

    assert!(
        output.status.success(),
        "recorder should succeed in disabled mode; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // No trace artefacts of any kind should have been written.
    let no_artefacts = !out_dir.exists()
        || (ct_files_in(&out_dir).is_empty()
            && !out_dir.join("trace_metadata.json").exists()
            && !out_dir.join("trace_paths.json").exists());
    assert!(
        no_artefacts,
        "no trace artefacts should be written when \
         CODETRACER_FUEL_RECORDER_DISABLED=1; got files in {:?}",
        out_dir
    );
}

/// `--format` is no longer accepted at any level — clap must reject it.
/// Convention: §4 (CTFS-only).
#[test]
fn test_format_flag_rejected_by_clap() {
    let tmp_dir = tempfile::tempdir().expect("tempdir");
    let out_dir = tmp_dir.path().join("traces");
    let project_dir = flow_test_project_dir();

    let output = Command::new(env!("CARGO_BIN_EXE_codetracer-fuel-recorder"))
        .args(["record"])
        .arg(&project_dir)
        .args(["--out-dir"])
        .arg(&out_dir)
        .args(["--format", "json"])
        .output()
        .expect("failed to run recorder");

    assert!(
        !output.status.success(),
        "--format should be rejected by clap; stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--format")
            || stderr.contains("unexpected argument")
            || stderr.contains("unrecognized")
            || stderr.contains("found argument"),
        "clap error should mention the unknown --format flag; got stderr:\n{stderr}"
    );
}

/// The CLI binary must not expose a `--format` flag at any level.
/// Convention: `Recorder-CLI-Conventions.md` §4 — recorders are
/// CTFS-only.
#[test]
fn test_no_format_flag_in_help() {
    let bin = env!("CARGO_BIN_EXE_codetracer-fuel-recorder");

    for subcmd in [None, Some("record"), Some("replay")] {
        let mut cmd = Command::new(bin);
        if let Some(s) = subcmd {
            cmd.arg(s);
        }
        cmd.arg("--help");

        let output = cmd.output().expect("failed to run --help");
        assert!(
            output.status.success(),
            "--help (subcmd={:?}) should exit 0",
            subcmd
        );

        let help = String::from_utf8_lossy(&output.stdout);
        assert!(
            !help.contains("--format"),
            "--help (subcmd={:?}) must not advertise --format; got:\n{help}",
            subcmd
        );
        assert!(
            !help.contains("CODETRACER_FORMAT"),
            "--help (subcmd={:?}) must not advertise CODETRACER_FORMAT; got:\n{help}",
            subcmd
        );
    }
}

/// `--help` must mention `ct print` so users know where to go for
/// human-readable conversion of the recorded CTFS bundle.
#[test]
fn test_help_mentions_ct_print() {
    let bin = env!("CARGO_BIN_EXE_codetracer-fuel-recorder");
    let output = Command::new(bin)
        .arg("--help")
        .output()
        .expect("failed to run --help");
    assert!(output.status.success(), "--help should exit 0");

    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("ct print"),
        "--help must mention `ct print` as the conversion tool; got:\n{help}"
    );
}

// ---------------------------------------------------------------------------
// Fixture export: build bytecode and export trace for WDIO tests
// ---------------------------------------------------------------------------

/// Export a trace fixture for the VS Code extension's WDIO smoke tests.
///
/// This test builds a FuelVM bytecode program using fuel-asm (no Sway/forc
/// compiler required), records the trace, and writes the output to the
/// directory specified by `SWAY_FIXTURE_OUTPUT_DIR`.
///
/// When `SWAY_FIXTURE_OUTPUT_DIR` is not set, the test uses a temporary
/// directory to verify the export logic still works.
///
/// Run with:
///   SWAY_FIXTURE_OUTPUT_DIR=<path> cargo test --test test_tracer -- --ignored export_fixture
#[test]
#[ignore]
fn export_fixture() {
    let tmp_dir;
    let out_dir = match std::env::var("SWAY_FIXTURE_OUTPUT_DIR") {
        Ok(dir) => {
            let p = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&p).expect("failed to create fixture output directory");
            p
        }
        Err(_) => {
            tmp_dir = tempfile::tempdir().expect("failed to create temp directory");
            tmp_dir.path().to_path_buf()
        }
    };
    let out_dir = out_dir.as_path();

    let source_path = PathBuf::from("flow_test.sw");
    let bytecode = simple_arithmetic_bytecode();
    let num_instructions = bytecode.len() / 4;
    let source_map = synthetic_source_map(&source_path, num_instructions);

    let recorder = FuelRecorder::new("flow_test", out_dir);
    recorder
        .record(bytecode, &source_map, &source_path)
        .expect("recording should succeed");

    // Verify .ct output.
    let ct_files: Vec<_> = std::fs::read_dir(out_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "ct"))
        .collect();
    assert!(!ct_files.is_empty(), ".ct should exist in fixture output");

    eprintln!("Fixture exported to {}", out_dir.display());
}
