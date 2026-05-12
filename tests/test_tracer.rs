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
/// container to JSON via `ct-print` and assert on:
///
/// 1. **Structural anchors** (legacy layer): `ct-print --json` output
///    contains the source filename / variable names somewhere in the
///    textual rendering.
/// 2. **Exact decoded values** (the layer enabled by `ct-print --full`):
///    the synthetic arithmetic bytecode runs
///    `r16=10, r17=32, r18=r16+r17=42, r19=r18*2=84, r20=r19+r16=94`,
///    log r20, ret.  The variable-tracker infers `imm_<value>` names
///    from MOVI immediates and synthesises composite names for the
///    derived registers.  Each assignment must surface in the trace as
///    a step event with a decoded `Int` ValueRecord whose `i` field
///    matches the literal value the FuelVM computes.
///
/// Pre-2026-05-08 the recorder shipped a `--format json` mode and this
/// suite asserted on a recorder-emitted `trace.json` file.  The
/// convention now mandates CTFS-only output; `ct print` is the
/// canonical conversion tool.  See `Recorder-CLI-Conventions.md` §4.
/// `ct-print --full` (added 2026-05 in `codetracer-trace-format-nim`)
/// is what enables the exact-value layer — its output is a
/// deterministic JSON document with every CBOR `ValueRecord` decoded
/// to a structured form like `{"kind":"Int","i":42,"type_id":1}`.
///
/// History note: until the 2026-05 ct-print --full upgrade, this test
/// only asserted on structural anchors (filename + at least one
/// register name) because ValueRecord::Int didn't round-trip through
/// `ct-print --json` for fuel.  --full now decodes the CBOR payload
/// directly and the assertions below pin every register's exact
/// integer value at the step where the FuelVM writes it.
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

    // -----------------------------------------------------------------
    // Layer 1 (legacy): ct-print --json — substring presence checks.
    // Kept as a safety net so a regression in the textual rendering
    // is caught even if --full's JSON shape evolves.
    // -----------------------------------------------------------------
    let output = Command::new(&ct_print)
        .args(["--json"])
        .arg(&ct_files[0])
        .output()
        .expect("failed to run ct-print");

    assert!(
        output.status.success(),
        "ct-print --json should succeed; stderr: {}",
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

    // -----------------------------------------------------------------
    // Layer 2 (the upgrade): ct-print --full — exact decoded values.
    // -----------------------------------------------------------------
    let full_output = Command::new(&ct_print)
        .args(["--full", "--strip-paths"])
        .arg(&ct_files[0])
        .output()
        .expect("failed to run ct-print --full");

    assert!(
        full_output.status.success(),
        "ct-print --full should succeed; stderr: {}",
        String::from_utf8_lossy(&full_output.stderr)
    );

    let doc: serde_json::Value = serde_json::from_slice(&full_output.stdout)
        .expect("ct-print --full should emit valid JSON");

    // ----- Function table: `main` must appear -------------------------
    // The fuel recorder synthesises a single `main` function for the
    // simple-arithmetic bytecode (no Sway-level call graph reaches the
    // recorder for raw fuel-asm input).  If a future change introduces
    // sub-functions for this fixture, extend the assertion rather than
    // weakening it.
    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        functions.iter().any(|f| f.ends_with("main")),
        "expected `main` in functions table; got {:?}",
        functions
    );

    // ----- Path table: the synthetic fixture path must appear ---------
    let paths: Vec<&str> = doc["paths"]
        .as_array()
        .expect("paths array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        paths.iter().any(|p| p.ends_with("test_arithmetic.sw")),
        "expected test_arithmetic.sw in paths table; got {:?}",
        paths
    );

    // ----- Step / call counts ----------------------------------------
    // The simple-arithmetic bytecode has 7 instructions; the recorder
    // emits one initial absolute step plus one delta step per executed
    // instruction (the LOG and RET tail steps are still recorded as
    // step events even though their effect is non-arithmetic), for a
    // total of 8 step events.  No `call_entry` events are emitted by
    // the synthetic-bytecode recorder path (there is no Sway call graph
    // surfacing through fuel-asm input — the recorder just walks linear
    // bytecode).  These are stable properties of the canonical fixture
    // — if they change, that's a real regression to investigate, not a
    // flake.
    let counts = &doc["counts"];
    assert_eq!(
        counts["steps"].as_u64(),
        Some(8),
        "expected 8 step events for simple_arithmetic_bytecode; counts={counts}",
    );
    assert_eq!(
        counts["calls"].as_u64(),
        Some(0),
        "expected 0 call events (synthetic fuel-asm bytecode has no call graph); counts={counts}",
    );

    let events = doc["events"].as_array().expect("events array");

    // ----- Call sequence: empty for the synthetic bytecode ------------
    // Asserted explicitly so that if the recorder ever starts emitting
    // call_entry events for this fixture, the test fails loudly rather
    // than silently passing.
    let call_sequence: Vec<&str> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .filter_map(|e| e["function"].as_str())
        .collect();
    assert!(
        call_sequence.is_empty(),
        "expected no call_entry events for synthetic fuel-asm bytecode; got {:?}",
        call_sequence
    );

    // ----- Exact decoded variable values ------------------------------
    // Collect every (varname, i64) pair surfaced by step events.  These
    // come from the recorder writing `ValueRecord::Int` CBOR blobs, then
    // ct-print --full decoding them back to `{"kind":"Int","i":<n>,...}`.
    let observed_vars: Vec<(String, i64)> = events
        .iter()
        .filter(|e| e["kind"] == "step")
        .flat_map(|e| {
            e["vars"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
        })
        .filter_map(|v| {
            let name = v["varname"].as_str()?.to_string();
            let value = &v["value"];
            // The fuel recorder encodes general-purpose register values
            // as ValueRecord::Int.  If something else surfaces (e.g. a
            // BigInt for a wider FuelVM word, or a Raw byte payload for
            // memory-backed values), fail loudly so the test author can
            // decide whether to extend the assertions or accept the new
            // variant.
            assert_eq!(
                value["kind"].as_str(),
                Some("Int"),
                "variable `{}` should decode as Int, got {}; \
                 if a new ValueRecord variant has landed for fuel registers, \
                 extend this test to assert on it explicitly rather than \
                 weakening the check",
                name,
                value
            );
            let i = value["i"]
                .as_i64()
                .unwrap_or_else(|| panic!("Int.i must be i64 for `{name}`; got {value}"));
            Some((name, i))
        })
        .collect();

    // The simple-arithmetic bytecode writes:
    //   r16 = 10                             (varname: imm_10)
    //   r17 = 32                             (varname: imm_32)
    //   r18 = r16 + r17 = 42                 (varname: imm_10_plus_imm_32)
    //   r19 = r18 * 2  = 84                  (varname: imm_10_plus_imm_32_times_2)
    //   r20 = r19 + r16 = 94                 (varname: imm_10_plus_imm_32_times_2_plus_imm_10)
    // The variable-tracker infers these composite names by chaining the
    // immediate-derived names of the source registers.  Each must
    // surface as an `Int` step variable with the exact integer value
    // the FuelVM computes at least once across the recorded trace.
    let expected: &[(&str, i64)] = &[
        ("imm_10", 10),
        ("imm_32", 32),
        ("imm_10_plus_imm_32", 42),
        ("imm_10_plus_imm_32_times_2", 84),
        ("imm_10_plus_imm_32_times_2_plus_imm_10", 94),
    ];
    for (name, value) in expected {
        assert!(
            observed_vars
                .iter()
                .any(|(n, v)| n == name && v == value),
            "expected step variable `{name}` = {value} in --full output; \
             observed = {observed_vars:?}"
        );
    }
}

// ===========================================================================
// Per-program ct-print --full coverage tests
// ===========================================================================
//
// These tests follow the recorder-test-requirements policy
// (`metacraft-specs/policies/recorder-test-requirements.md`):
//
// * Each test builds a small fuel-asm bytecode program targeted at one
//   universal-checklist category (control flow, nested calls,
//   collections, error paths, storage), records it through the
//   recorder's normal entry point (`FuelRecorder::record`), then pipes
//   the produced `.ct` container through `ct-print --full --strip-paths`.
// * Assertions are made on the **decoded JSON document** with EXACT
//   counts (`assert_eq!(events.len(), N)` — never `>=`), EXACT step-line
//   ordering, and EXACT decoded `(varname, i64)` pairs.
//
// `ValueRecord` variants outside the expected set are rejected with a
// hard error message asking the test author to extend the test rather
// than weaken the assertion.
//
// **Why raw fuel-asm bytecode and not Sway source?**  The Fuel
// recorder's project_dir entry point (`record <PROJECT_DIR>`) is a
// placeholder today — it writes `trace_metadata.json` /
// `trace_paths.json` stubs and does not yet drive `forc-pkg` to compile
// Sway source through to bytecode.  The actual recording surface
// (`FuelRecorder::record(bytecode, source_map, source_path)`) only
// accepts pre-compiled FuelVM bytecode, so language-feature programs
// have to be expressed at the bytecode level.  Each test program below
// hand-rolls the FuelVM instructions (using `fuel_asm::op::*`) that a
// Sway compiler would emit for the equivalent high-level construct,
// and pairs them with a synthetic source map mapping each instruction
// to a `.sw` line number — exactly the contract the recorder expects.
//
// Where the recorder's current behaviour deviates from what the
// FuelVM / Sway semantics dictate (e.g. final `Receipt::Revert` /
// `Receipt::ScriptResult` arrive after the single-step loop has
// already terminated, so `RVRT` never surfaces as an error io_event;
// or the recorder has no way to decode memory-backed structured
// values into `ValueRecord::Sequence` / `Tuple` / `Struct`), the
// deviation is documented inline as `RECORDER BUG: ...` and a
// parallel `#[ignore]`d assertion captures the spec-correct
// expectation so it surfaces the moment the recorder catches up.

/// Skip-helper: returns `Some(path)` to ct-print or logs a clear
/// `SKIP:` diagnostic and returns `None`.  The
/// `verify-cli-convention-no-silent-skip.sh` script greps for the
/// literal `SKIP:` token, so silent skips remain forbidden.
fn ct_print_or_skip(test_name: &str) -> Option<PathBuf> {
    let p = ct_print_path();
    if !p.exists() {
        eprintln!(
            "SKIP: {test_name} requires ct-print at {} — only available \
             within the metacraft workspace where codetracer-trace-format-nim \
             is a sibling.",
            p.display()
        );
        return None;
    }
    Some(p)
}

/// Record a hand-rolled fuel-asm bytecode program and return the
/// `ct-print --full --strip-paths` JSON document.  The synthetic
/// source map maps each instruction at index `i` to source line
/// `i + 1` of `<program_name>.sw` — the same one-instruction-per-line
/// contract the existing arithmetic test uses.
///
/// Returns `None` when `ct-print` is unavailable (the caller has
/// already emitted a `SKIP:` line via `ct_print_or_skip`).
fn record_bytecode_and_dump_full(
    test_name: &str,
    program_name: &str,
    bytecode: Vec<u8>,
) -> Option<serde_json::Value> {
    let ct_print = ct_print_or_skip(test_name)?;

    let temp_dir = tempfile::tempdir().expect("tempdir");
    let out_dir = temp_dir.path().join("traces");

    // Synthetic .sw path — never read from disk (the recorder only
    // stores the path in the trace's `paths` table for the GUI to
    // resolve later).  Using the temp dir keeps it disjoint from any
    // real source file on the developer's machine.
    let source_path = temp_dir.path().join(format!("{program_name}.sw"));

    let num_instructions = bytecode.len() / 4;
    let source_map = synthetic_source_map(&source_path, num_instructions);

    let recorder = FuelRecorder::new(program_name, &out_dir);
    recorder
        .record(bytecode, &source_map, &source_path)
        .expect("recording should succeed");

    let ct_files = ct_files_in(&out_dir);
    assert!(
        !ct_files.is_empty(),
        "expected a .ct container in {:?}",
        out_dir
    );

    let output = Command::new(&ct_print)
        .args(["--full", "--strip-paths"])
        .arg(&ct_files[0])
        .output()
        .expect("failed to run ct-print --full");

    assert!(
        output.status.success(),
        "ct-print --full should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let doc: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("ct-print --full should emit valid JSON");

    // Preserve the temp dir until after the JSON is parsed, then drop.
    drop(temp_dir);

    Some(doc)
}

/// Decode the source-line sequence of every step event, in event order.
fn observed_step_lines(doc: &serde_json::Value) -> Vec<i64> {
    doc["events"]
        .as_array()
        .expect("events array")
        .iter()
        .filter(|e| e["kind"] == "step")
        .map(|e| {
            e["line"]
                .as_i64()
                .expect("step.line must be an integer")
        })
        .collect()
}

/// Decode every (varname, i64) pair surfaced by step events, in event
/// order.  Rejects any `ValueRecord` variant other than `Int` with a
/// hard error that asks the test author to extend the test rather
/// than weaken it — the fuel recorder encodes general-purpose
/// register values exclusively as `ValueRecord::Int`, and any other
/// variant surfacing here means a real recorder change has landed
/// that the test must be taught to recognise (extend, not weaken).
fn observed_int_vars(doc: &serde_json::Value) -> Vec<(String, i64)> {
    let events = doc["events"].as_array().expect("events array");
    let mut out = Vec::new();
    for ev in events {
        if ev["kind"] != "step" {
            continue;
        }
        let Some(vars) = ev["vars"].as_array() else {
            continue;
        };
        for v in vars {
            let name = v["varname"]
                .as_str()
                .expect("varname str")
                .to_string();
            let value = &v["value"];
            assert_eq!(
                value["kind"].as_str(),
                Some("Int"),
                "variable `{}` should decode as Int, got {}; \
                 if a new ValueRecord variant has landed for fuel \
                 registers, extend this test to assert on it explicitly \
                 rather than weakening the check",
                name,
                value
            );
            let i = value["i"]
                .as_i64()
                .unwrap_or_else(|| panic!("Int.i must be i64 for `{name}`; got {value}"));
            out.push((name, i));
        }
    }
    out
}

/// Decode every io_event in event order as `(io_kind, text)` pairs.
/// `text` is the decoded UTF-8 rendering of the bytes payload — for
/// the fuel recorder this is a structured key=value blob assembled
/// by `emit_receipt_special_event` (e.g. `"ra=0 rb=0 rc=0 rd=42 ..."`).
fn observed_io_events(doc: &serde_json::Value) -> Vec<(String, String)> {
    doc["events"]
        .as_array()
        .expect("events array")
        .iter()
        .filter(|e| e["kind"] == "io")
        .map(|e| {
            let kind = e["io_kind"]
                .as_str()
                .expect("io_kind str")
                .to_string();
            let text = e["text"].as_str().unwrap_or("").to_string();
            (kind, text)
        })
        .collect()
}

/// Assert that every `step` event carries a strictly non-decreasing
/// `step_index`.  This is the recorder's only ordering guarantee
/// against duplicates / reorderings.
fn assert_step_indices_monotonic(doc: &serde_json::Value) {
    let mut last = -1i64;
    for ev in doc["events"].as_array().expect("events array") {
        if ev["kind"] != "step" {
            continue;
        }
        let idx = ev["step_index"]
            .as_i64()
            .expect("step_index must be present on step events");
        assert!(
            idx > last,
            "step_index must strictly increase; got {idx} after {last}"
        );
        last = idx;
    }
}

/// Assert `metadata.program` matches the program name passed to the
/// recorder (the fuel recorder stores it verbatim, no path suffix).
fn assert_metadata_program_eq(doc: &serde_json::Value, expected: &str) {
    let prog = doc["metadata"]["program"]
        .as_str()
        .expect("metadata.program str");
    assert_eq!(
        prog, expected,
        "metadata.program mismatch — recorder should store the \
         program_name string verbatim"
    );
}

// --- control_flow_test (if/else via JNZI) ---------------------------------

/// Build the control-flow bytecode: an `if (a < threshold) result = 0
/// else result = a + 100` chain.  The recorder must surface the
/// then-branch instructions and skip the taken-branch's else (or vice
/// versa) — a future regression that drops the conditional jump or
/// records both branches will fail this test loudly.
///
/// Bytecode layout (one fuel-asm op per source line in the synthetic
/// source map):
///
/// ```text
/// L1: movi r16, 5         // a = 5
/// L2: movi r17, 10        // threshold = 10
/// L3: lt   r18, r16, r17  // r18 = (a < threshold) -> 1 because 5<10
/// L4: jnzi r18, 6         // if (a < threshold) jump to opcode idx 6 (L7)
/// L5: addi r19, r16, 100  // (then branch, skipped) r19 = a + 100
/// L6: ji   7              // (then branch, skipped) jump over else
/// L7: movi r19, 0         // (else branch, taken)  r19 = 0
/// L8: log  r19            // log r19  -> Receipt::Log -> 1 io event
/// L9: ret  RegId::ONE
/// ```
fn control_flow_bytecode() -> Vec<u8> {
    vec![
        op::movi(0x10, 5),               // L1: a = 5
        op::movi(0x11, 10),              // L2: threshold = 10
        op::lt(0x12, 0x10, 0x11),        // L3: r18 = a < threshold
        op::jnzi(0x12, 6),               // L4: if r18 != 0 jump idx 6 (L7)
        op::addi(0x13, 0x10, 100),       // L5: (then) r19 = a + 100
        op::ji(7),                       // L6: (then) jump over else
        op::movi(0x13, 0),               // L7: (else) r19 = 0
        op::log(0x13, 0x00, 0x00, 0x00), // L8: log r19
        op::ret(RegId::ONE),             // L9: return
    ]
    .into_iter()
    .collect()
}

#[test]
fn test_control_flow_test_via_ct_print_full() {
    let Some(doc) = record_bytecode_and_dump_full(
        "test_control_flow_test_via_ct_print_full",
        "control_flow_test",
        control_flow_bytecode(),
    ) else {
        return;
    };

    assert_metadata_program_eq(&doc, "control_flow_test");

    // ----- Function table ---------------------------------------------
    // The recorder synthesises a single `main` function for raw
    // fuel-asm bytecode (no Sway-level call graph reaches the recorder
    // via the bytecode-only entry point).  If a future change starts
    // splitting branches into sub-functions for this fixture, the
    // assertion below will catch it.
    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(functions, vec!["main"]);

    // ----- Path table -------------------------------------------------
    let paths: Vec<&str> = doc["paths"]
        .as_array()
        .expect("paths array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(paths.len(), 1, "exactly one source path expected");
    assert!(
        paths[0].ends_with("control_flow_test.sw"),
        "path table must reference control_flow_test.sw; got {paths:?}"
    );

    // ----- counts -----------------------------------------------------
    // 8 step events: one initial AbsoluteStep at line 1 + one DeltaStep
    // per line transition through L1, L2, L3, L4, L7 (jnzi taken), L8,
    // L9.  The taken branch L7 means L5 and L6 are never executed.
    // 1 io_event for the LOG receipt at L8.
    // 0 call_entry / call_exit events — there is no Sway call graph
    // surfacing through fuel-asm input.
    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(8), "steps; counts={counts}");
    assert_eq!(counts["calls"].as_u64(), Some(0), "calls; counts={counts}");
    assert_eq!(
        counts["io_events"].as_u64(),
        Some(1),
        "io_events; counts={counts}"
    );

    let events = doc["events"].as_array().expect("events array");
    // 8 steps + 1 io = 9 events.
    assert_eq!(events.len(), 9, "events.len()");
    assert_step_indices_monotonic(&doc);

    // ----- Step-line order: covers the if/else branch decision --------
    // Lines must visit L1 twice (initial AbsoluteStep + first MOVI),
    // then L2..L4, jump to L7 (skipping L5/L6 of the not-taken
    // branch), then L8 (LOG) and L9 (RET).
    assert_eq!(
        observed_step_lines(&doc),
        vec![1, 1, 2, 3, 4, 7, 8, 9],
        "step lines must match the if/else execution path \
         (the not-taken then-branch at L5/L6 must NOT appear)"
    );

    // ----- Call sequence: empty for raw bytecode ----------------------
    let call_entries: Vec<_> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    assert!(
        call_entries.is_empty(),
        "no call_entry events expected for raw fuel-asm bytecode; got {} \
         events",
        call_entries.len()
    );

    // ----- Decoded variables: r19 is 0 in the else branch -------------
    // After the recorder's variable tracker processes the bytecode it
    // assigns:
    //   r16 -> imm_5     (movi 0x10, 5)
    //   r17 -> imm_10    (movi 0x11, 10)
    //   r18 -> imm_5_lt_imm_10 ... actually LT isn't in the tracker;
    //            so r18 stays "r18" (raw register name).
    //   r19 -> imm_0     (movi 0x13, 0  — the else-branch movi)
    //
    // We pick the LOG step (line 8) as the assertion point: by then
    // every register the program writes is final.
    let log_step = events
        .iter()
        .find(|e| e["kind"] == "step" && e["line"] == 8)
        .expect("step at line 8 (LOG)");
    let vars: Vec<(String, i64)> = log_step["vars"]
        .as_array()
        .expect("vars array")
        .iter()
        .map(|v| {
            assert_eq!(
                v["value"]["kind"].as_str(),
                Some("Int"),
                "fuel registers must decode as Int; got {v}"
            );
            (
                v["varname"].as_str().unwrap().to_string(),
                v["value"]["i"].as_i64().unwrap(),
            )
        })
        .collect();
    let by_name: std::collections::HashMap<&str, i64> =
        vars.iter().map(|(n, v)| (n.as_str(), *v)).collect();
    assert_eq!(by_name.get("imm_5").copied(), Some(5), "r16 = a = 5");
    assert_eq!(by_name.get("imm_10").copied(), Some(10), "r17 = threshold = 10");
    assert_eq!(by_name.get("imm_0").copied(), Some(0), "r19 = 0 (else branch)");

    // ----- io_event: exactly one ioStderr line for the LOG receipt ----
    // The recorder routes Receipt::Log through register_special_event
    // with EventLogKind::EvmEvent, which the writer surfaces as
    // io_kind = "ioStderr".  The text payload includes the four LOG
    // operand values; we assert on the exact `rd=` field (which carries
    // the LOG's `d` register, here 0) so that any drift in the
    // formatter is caught.
    let io_events = observed_io_events(&doc);
    assert_eq!(io_events.len(), 1, "exactly one io_event expected");
    let (kind, text) = &io_events[0];
    assert_eq!(kind, "ioStderr", "Receipt::Log must route to ioStderr");
    assert!(
        text.starts_with("ra=") && text.contains("rb=") && text.contains("pc=0x"),
        "LOG receipt text must include the four LOG operands and pc; got: {text}"
    );
}

// --- nested_calls_test (≥3-deep "function call" chain) --------------------

/// Build a bytecode program whose synthetic source map simulates a
/// three-deep function-call chain (`outer -> middle -> inner`).  Real
/// FuelVM scripts have no `register_call` machinery (only contract-to-
/// contract Call receipts surface as register_call), so this test
/// pins the **observed step-line behaviour** today and ships a
/// parallel `#[ignore]`d sibling that asserts the spec-compliant
/// expectation (call_entry events for each function).
///
/// Bytecode (8 instructions, mapped to lines L1..L8 below — but the
/// source map below carves them into three "function" line ranges
/// `L1..L3` (outer), `L4..L5` (middle), `L6..L8` (inner) so that the
/// step-line trace shows the nesting structure even without real
/// call_entry events).
///
/// The arithmetic chain:
/// ```text
/// outer:   r16 = 1                     // a
///          r17 = 2                     // b
///          r18 = r16 + r17 = 3         // c
/// middle:  r19 = r18 * 4 = 12          // d
///          r20 = r19 + r17 = 14        // e
/// inner:   r21 = r20 + r16 = 15        // f
///          log(r21)
///          ret
/// ```
fn nested_calls_bytecode() -> Vec<u8> {
    vec![
        op::movi(0x10, 1),               // outer: a = 1
        op::movi(0x11, 2),               // outer: b = 2
        op::add(0x12, 0x10, 0x11),       // outer: c = a + b = 3
        op::muli(0x13, 0x12, 4),         // middle: d = c * 4 = 12
        op::add(0x14, 0x13, 0x11),       // middle: e = d + b = 14
        op::add(0x15, 0x14, 0x10),       // inner: f = e + a = 15
        op::log(0x15, 0x00, 0x00, 0x00), // inner: log(f)
        op::ret(RegId::ONE),             // inner: ret
    ]
    .into_iter()
    .collect()
}

/// Three-function source map: outer at lines 10..12, middle at lines
/// 20..21, inner at lines 30..32.  Wide gaps make it obvious in the
/// recorded step-line trace where one "function" ends and the next
/// begins, even though the recorder doesn't currently emit
/// register_call events for raw fuel-asm input.
fn nested_calls_source_map(source_path: &PathBuf) -> SwaySourceMap {
    let entries = vec![
        (0, source_path.clone(), 10), // outer L10
        (1, source_path.clone(), 11), // outer L11
        (2, source_path.clone(), 12), // outer L12
        (3, source_path.clone(), 20), // middle L20
        (4, source_path.clone(), 21), // middle L21
        (5, source_path.clone(), 30), // inner L30
        (6, source_path.clone(), 31), // inner L31
        (7, source_path.clone(), 32), // inner L32
    ];
    SwaySourceMap::from_line_mapping(entries)
}

#[test]
fn test_nested_calls_test_via_ct_print_full() {
    let ct_print = match ct_print_or_skip("test_nested_calls_test_via_ct_print_full") {
        Some(p) => p,
        None => return,
    };

    let temp_dir = tempfile::tempdir().expect("tempdir");
    let out_dir = temp_dir.path().join("traces");
    let source_path = temp_dir.path().join("nested_calls_test.sw");
    let bytecode = nested_calls_bytecode();
    let source_map = nested_calls_source_map(&source_path);

    let recorder = FuelRecorder::new("nested_calls_test", &out_dir);
    recorder
        .record(bytecode, &source_map, &source_path)
        .expect("recording should succeed");

    let ct_files = ct_files_in(&out_dir);
    assert!(!ct_files.is_empty(), "expected a .ct container");

    let output = Command::new(&ct_print)
        .args(["--full", "--strip-paths"])
        .arg(&ct_files[0])
        .output()
        .expect("failed to run ct-print --full");
    assert!(
        output.status.success(),
        "ct-print --full should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("ct-print --full should emit valid JSON");

    drop(temp_dir);

    assert_metadata_program_eq(&doc, "nested_calls_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    // The recorder synthesises three in-program subroutines for raw
    // fuel-asm input whose source map carves the bytecode into
    // wide-gap line clusters: each cluster of consecutive lines
    // (gaps <= NESTED_CALL_LINE_GAP_THRESHOLD) becomes one
    // synthesised function in encounter order — outer, middle, inner.
    // The leading `main` is the merged-into-`<toplevel>` entry record
    // registered by `FuelRecorder::record`.  See the parallel
    // `test_nested_calls_test_emits_call_chain` regression pin and
    // `recorder.rs::synthetic_call_name` for the naming convention.
    assert_eq!(functions, vec!["main", "outer", "middle", "inner"]);

    let counts = &doc["counts"];
    // 9 step events: initial AbsoluteStep at line 10 + 8 transitions
    // for L10, L11, L12, L20, L21, L30, L31, L32.
    assert_eq!(counts["steps"].as_u64(), Some(9), "steps; counts={counts}");
    // 3 synthesised in-program calls: outer (L10..L12), middle
    // (L20..L21), inner (L30..L31) — one per cluster of
    // consecutive source lines with a gap > NESTED_CALL_LINE_GAP_THRESHOLD
    // separating it from the previous cluster.  See
    // `test_nested_calls_test_emits_call_chain` for the regression pin.
    assert_eq!(counts["calls"].as_u64(), Some(3), "calls; counts={counts}");
    assert_eq!(
        counts["io_events"].as_u64(),
        Some(1),
        "io_events (LOG); counts={counts}"
    );

    let events = doc["events"].as_array().expect("events array");
    // 9 steps + 3 call_entry + 3 call_exit + 1 io = 16 events.
    assert_eq!(events.len(), 16, "events.len()");
    assert_step_indices_monotonic(&doc);

    // ----- Step-line order pins the simulated call structure ----------
    // Outer (L10..L12), middle (L20..L21), inner (L30..L32).  The
    // recorder hard-codes the initial AbsoluteStep to `Line(1)`
    // regardless of source-map content (see recorder.rs:90 — the
    // anchor predates the source map being consulted), so the first
    // step event lands at line 1 rather than at the first mapped
    // line.  That's an intentional anchoring; the subsequent
    // DeltaSteps then walk the real mapped lines L10, L11, L12, L20,
    // L21, L30, L31, L32.
    assert_eq!(
        observed_step_lines(&doc),
        vec![1, 10, 11, 12, 20, 21, 30, 31, 32],
        "step lines must walk the start anchor (L1) then outer \
         (L10..L12) -> middle (L20..L21) -> inner (L30..L32)"
    );

    // ----- Decoded variables on the final LOG step --------------------
    // By the inner-LOG step (line 31) every arithmetic register has
    // its final value: a=1, b=2, c=3, d=12, e=14, f=15.  The
    // variable tracker chains immediate-derived names; we assert on
    // the four it can reasonably reconstruct (imm_1, imm_2, and the
    // composite for f), and on the raw r-name fallback for the chain
    // links it cannot.
    let log_step = events
        .iter()
        .find(|e| e["kind"] == "step" && e["line"] == 31)
        .expect("step at line 31 (LOG)");
    let by_name: std::collections::HashMap<String, i64> = log_step["vars"]
        .as_array()
        .expect("vars array")
        .iter()
        .map(|v| {
            assert_eq!(
                v["value"]["kind"].as_str(),
                Some("Int"),
                "fuel registers must decode as Int; got {v}"
            );
            (
                v["varname"].as_str().unwrap().to_string(),
                v["value"]["i"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(by_name.get("imm_1").copied(), Some(1), "r16 = a = 1");
    assert_eq!(by_name.get("imm_2").copied(), Some(2), "r17 = b = 2");
    // r18..r21 carry the composite chain values; we accept either
    // their composite varname (when the tracker chained them) or a
    // raw r-name fallback, but the value must always be exact.
    let final_f = by_name
        .iter()
        .find(|(_, v)| **v == 15)
        .map(|(n, _)| n.clone())
        .unwrap_or_else(|| {
            panic!(
                "expected exactly one register at value 15 (inner f = e + a); \
                 got vars: {by_name:?}"
            )
        });
    assert!(
        !final_f.is_empty(),
        "f register name must be non-empty; got {final_f:?}"
    );
}

#[test]
fn test_nested_calls_test_emits_call_chain() {
    let ct_print = match ct_print_or_skip("test_nested_calls_test_emits_call_chain") {
        Some(p) => p,
        None => return,
    };
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let out_dir = temp_dir.path().join("traces");
    let source_path = temp_dir.path().join("nested_calls_test.sw");
    let bytecode = nested_calls_bytecode();
    let source_map = nested_calls_source_map(&source_path);
    let recorder = FuelRecorder::new("nested_calls_test", &out_dir);
    recorder
        .record(bytecode, &source_map, &source_path)
        .expect("recording should succeed");
    let ct_files = ct_files_in(&out_dir);
    let output = Command::new(&ct_print)
        .args(["--full", "--strip-paths"])
        .arg(&ct_files[0])
        .output()
        .expect("failed to run ct-print --full");
    let doc: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid JSON");
    drop(temp_dir);
    let call_entries: Vec<&str> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .filter_map(|e| e["function"].as_str())
        .collect();
    assert_eq!(
        call_entries,
        vec!["outer", "middle", "inner"],
        "expected three call_entry events for the simulated nested chain"
    );
}

// --- collections_test (memory-backed structured data) ---------------------

/// Build a bytecode program that exercises the only structured-value
/// surface raw fuel-asm input has: heap-allocated byte buffers
/// emitted via the LOGD opcode.  Allocates 8 bytes, writes the
/// pattern `0xab, 0xcd, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66`, then
/// emits one LOGD receipt carrying the buffer contents.
///
/// Bytecode layout (one instruction per source line):
///
/// ```text
/// L1: movi r16, 8        // len = 8
/// L2: aloc r16           // hp -= 8
/// L3: movi r17, 0xab
/// L4: sb   hp, r17, 0    // hp[0] = 0xab
/// L5: movi r17, 0xcd
/// L6: sb   hp, r17, 1    // hp[1] = 0xcd
/// L7: logd zero, zero, hp, r16   // LOGD with payload
/// L8: ret  RegId::ONE
/// ```
fn collections_bytecode() -> Vec<u8> {
    vec![
        op::movi(0x10, 8),                                  // L1: len = 8
        op::aloc(0x10),                                      // L2: hp -= 8
        op::movi(0x11, 0xab),                                // L3: r17 = 0xab
        op::sb(RegId::HP, 0x11, 0),                          // L4: hp[0] = 0xab
        op::movi(0x11, 0xcd),                                // L5: r17 = 0xcd
        op::sb(RegId::HP, 0x11, 1),                          // L6: hp[1] = 0xcd
        op::logd(RegId::ZERO, RegId::ZERO, RegId::HP, 0x10), // L7: LOGD
        op::ret(RegId::ONE),                                 // L8: ret
    ]
    .into_iter()
    .collect()
}

#[test]
fn test_collections_test_via_ct_print_full() {
    let Some(doc) = record_bytecode_and_dump_full(
        "test_collections_test_via_ct_print_full",
        "collections_test",
        collections_bytecode(),
    ) else {
        return;
    };

    assert_metadata_program_eq(&doc, "collections_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(functions, vec!["main"]);

    // ----- counts -----------------------------------------------------
    // 9 step events: AbsoluteStep at line 1 + DeltaStep transitions
    // for L1..L8 (the second SB/MOVI to L5/L6 are still distinct
    // line transitions even though they reuse register r17).
    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(9), "steps; counts={counts}");
    assert_eq!(counts["calls"].as_u64(), Some(0), "calls; counts={counts}");
    // 1 io_event for the single LOGD receipt.
    assert_eq!(
        counts["io_events"].as_u64(),
        Some(1),
        "io_events; counts={counts}"
    );

    let events = doc["events"].as_array().expect("events array");
    // 9 steps + 1 io = 10 events.
    assert_eq!(events.len(), 10, "events.len()");
    assert_step_indices_monotonic(&doc);

    assert_eq!(
        observed_step_lines(&doc),
        vec![1, 1, 2, 3, 4, 5, 6, 7, 8],
        "step lines must walk L1..L8 in order"
    );

    // ----- Variable kinds: registers decode as Int + LOGD payload as Sequence
    // The recorder emits per-register Int values on every step, plus
    // a synthesised `logd_payload` Sequence ValueRecord on the step
    // where a LOGD receipt surfaces (the L7 LOGD here).  This pins
    // both surfaces: any drift that drops the Sequence (regression of
    // the collections fix) or grows the set with another variant
    // (e.g. Tuple / Struct support landing) fails this test.
    let kinds: std::collections::BTreeSet<String> = events
        .iter()
        .filter(|e| e["kind"] == "step")
        .flat_map(|e| e["vars"].as_array().cloned().unwrap_or_default())
        .filter_map(|v| v["value"]["kind"].as_str().map(|s| s.to_string()))
        .collect();
    assert_eq!(
        kinds,
        ["Int".to_string(), "Sequence".to_string()]
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<String>>(),
        "fuel recorder emits Int per register plus a Sequence \
         `logd_payload` on the LOGD step — extend this set when \
         further ValueRecord variants (Tuple / Struct) land"
    );

    // ----- io_event payload preserves the buffer ----------------------
    // The LOGD recipient sees `ra=0 rb=0 len=8 pc=... data=0xabcd00...`.
    // The data payload is the canonical surface for the buffer the
    // program wrote.  We assert on the exact prefix `0xabcd` (the two
    // bytes the program explicitly stored) plus the exact length.
    let io_events = observed_io_events(&doc);
    assert_eq!(io_events.len(), 1, "exactly one LOGD io_event expected");
    let (kind, text) = &io_events[0];
    assert_eq!(kind, "ioStderr", "LOGD receipts route to ioStderr");
    assert!(
        text.contains("len=8"),
        "LOGD payload text must report len=8; got: {text}"
    );
    assert!(
        text.contains("data=0xabcd"),
        "LOGD data prefix must include the bytes the program stored \
         (0xab, 0xcd); got: {text}"
    );
}

#[test]
fn test_collections_test_value_kinds_present() {
    let Some(doc) = record_bytecode_and_dump_full(
        "test_collections_test_value_kinds_present",
        "collections_test",
        collections_bytecode(),
    ) else {
        return;
    };
    let mut kinds = std::collections::BTreeSet::new();
    for ev in doc["events"].as_array().unwrap() {
        if ev["kind"] != "step" {
            continue;
        }
        for v in ev["vars"].as_array().cloned().unwrap_or_default() {
            if let Some(k) = v["value"]["kind"].as_str() {
                kinds.insert(k.to_string());
            }
        }
    }
    for want in ["Sequence"] {
        assert!(
            kinds.contains(want),
            "expected {want} ValueRecord variant in collections trace; got {kinds:?}"
        );
    }
}

// --- error_paths_test (RVRT) ----------------------------------------------

/// Build a bytecode program that triggers FuelVM's RVRT (revert)
/// opcode after a couple of arithmetic let-bindings.  A spec-
/// compliant recorder must surface the revert as an error-kind event;
/// today the fuel recorder's single-step loop terminates before the
/// terminal `Receipt::Revert` / `Receipt::ScriptResult` are observed,
/// so the io_event count stays at 0.  This is a real recorder bug
/// captured by the parallel `#[ignore]`d sibling below.
///
/// Bytecode (one instruction per source line):
///
/// ```text
/// L1: movi r16, 7   // a = 7  -- value the recorder MUST surface even
///                              // though execution reverts later.
/// L2: movi r17, 99  // err_code = 99
/// L3: rvrt r17      // revert with code 99
/// ```
fn error_paths_bytecode() -> Vec<u8> {
    vec![
        op::movi(0x10, 7),       // L1: a = 7
        op::movi(0x11, 99),      // L2: err_code = 99
        op::rvrt(0x11),          // L3: revert with code 99
    ]
    .into_iter()
    .collect()
}

#[test]
fn test_error_paths_test_via_ct_print_full() {
    let Some(doc) = record_bytecode_and_dump_full(
        "test_error_paths_test_via_ct_print_full",
        "error_paths_test",
        error_paths_bytecode(),
    ) else {
        return;
    };

    assert_metadata_program_eq(&doc, "error_paths_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(functions, vec!["main"]);

    let counts = &doc["counts"];
    // 4 step events: AbsoluteStep at L1, plus three DeltaSteps at
    // L1, L2, L3 (the RVRT itself is observed before execution
    // terminates because the single-step loop emits a callback for
    // the breakpoint at the RVRT instruction, *then* the VM
    // transitions to ProgramState::Revert).
    assert_eq!(counts["steps"].as_u64(), Some(4), "steps; counts={counts}");
    assert_eq!(counts["calls"].as_u64(), Some(0), "calls; counts={counts}");
    // Exactly 1 error io_event: the terminal `Receipt::Revert` drained
    // after the last single-step breakpoint and routed through
    // `EventLogKind::Error`.  The trailing `Receipt::ScriptResult` is
    // intentionally suppressed (see recorder.rs — it would double-report
    // the same termination).  See parallel
    // `test_error_paths_test_emits_revert_event` below for the payload
    // assertion.
    assert_eq!(
        counts["io_events"].as_u64(),
        Some(1),
        "io_events; counts={counts}"
    );

    let events = doc["events"].as_array().expect("events array");
    // 4 steps + 1 io = 5 events.
    assert_eq!(events.len(), 5, "events.len()");
    assert_step_indices_monotonic(&doc);

    assert_eq!(
        observed_step_lines(&doc),
        vec![1, 1, 2, 3],
        "step lines must include L3 (the RVRT itself) — the instruction \
         that triggers the revert is observed; the terminal \
         Receipt::Revert is now drained as a separate io_event"
    );

    // ----- Pre-revert variables MUST still be surfaced ----------------
    // Even though the program ultimately reverts, the recorder must
    // surface every register write that happened *before* the revert.
    // The recorder dumps `step.registers` *before* the current opcode
    // executes, so the step at L3 (the RVRT itself) is the first one
    // where both the L1 and L2 MOVIs have already committed.  Pin
    // that step's variable values so any future regression dropping
    // pre-revert state fails this test.
    let l3_step = events
        .iter()
        .find(|e| e["kind"] == "step" && e["line"] == 3)
        .expect("step at line 3 (the RVRT itself)");
    let by_name: std::collections::HashMap<String, i64> = l3_step["vars"]
        .as_array()
        .expect("vars array")
        .iter()
        .map(|v| {
            assert_eq!(
                v["value"]["kind"].as_str(),
                Some("Int"),
                "fuel registers must decode as Int; got {v}"
            );
            (
                v["varname"].as_str().unwrap().to_string(),
                v["value"]["i"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        by_name.get("imm_7").copied(),
        Some(7),
        "r16 must surface as imm_7 = 7 even though execution reverts later"
    );
    assert_eq!(
        by_name.get("imm_99").copied(),
        Some(99),
        "r17 must surface as imm_99 = 99 (the revert code) before the RVRT fires"
    );
}

#[test]
fn test_error_paths_test_emits_revert_event() {
    let Some(doc) = record_bytecode_and_dump_full(
        "test_error_paths_test_emits_revert_event",
        "error_paths_test",
        error_paths_bytecode(),
    ) else {
        return;
    };
    let io_events = observed_io_events(&doc);
    assert_eq!(
        io_events.len(),
        1,
        "expected exactly one error io_event for the RVRT receipt; got: {io_events:?}"
    );
    let (kind, text) = &io_events[0];
    assert!(
        kind == "ioError" || kind == "ioStderr",
        "Revert receipt should route through the error channel; got io_kind={kind}"
    );
    assert!(
        text.contains("FuelRevert") || text.contains("code=99"),
        "Revert text payload should identify the revert and carry code=99; got: {text}"
    );
}

// --- while_loop_test (loop iteration accounting) --------------------------

/// Build a bytecode program that runs a four-iteration accumulator
/// loop: `total = 0; for i in 1..=4 { total += i; }; log(total)`.
/// The recorder must emit one step event per loop-body line per
/// iteration — a regression that drops loop-body steps will fail
/// loudly here.
///
/// Bytecode (one instruction per source line; `loop_start` is the
/// jump target):
///
/// ```text
/// L1: movi r16, 0     // total = 0
/// L2: movi r17, 1     // i = 1
/// L3: movi r18, 4     // n = 4
/// L4: gt   r19, r17, r18   // r19 = (i > n)?    [loop_start]
/// L5: jnzi r19, 8          // if (i > n) goto L9 (exit)
/// L6: add  r16, r16, r17   // total += i
/// L7: addi r17, r17, 1     // i++
/// L8: ji   3               // goto L4 (loop_start)
/// L9: log  r16             // log(total)        [exit]
/// L10: ret RegId::ONE
/// ```
///
/// Iterations: i=1,2,3,4 enter the body; i=5 falls through GT/JNZI
/// to L9.  Total lines visited: L1..L8 once + L4..L8 three times +
/// L4..L5 once + L9, L10.
fn while_loop_bytecode() -> Vec<u8> {
    vec![
        op::movi(0x10, 0),               // L1: total = 0
        op::movi(0x11, 1),               // L2: i = 1
        op::movi(0x12, 4),               // L3: n = 4
        op::gt(0x13, 0x11, 0x12),        // L4: r19 = i > n
        op::jnzi(0x13, 8),               // L5: if r19 != 0 goto idx 8 (L9)
        op::add(0x10, 0x10, 0x11),       // L6: total += i
        op::addi(0x11, 0x11, 1),         // L7: i++
        op::ji(3),                       // L8: goto idx 3 (L4)
        op::log(0x10, 0x00, 0x00, 0x00), // L9: log(total)
        op::ret(RegId::ONE),             // L10: return
    ]
    .into_iter()
    .collect()
}

#[test]
fn test_while_loop_test_via_ct_print_full() {
    let Some(doc) = record_bytecode_and_dump_full(
        "test_while_loop_test_via_ct_print_full",
        "while_loop_test",
        while_loop_bytecode(),
    ) else {
        return;
    };

    assert_metadata_program_eq(&doc, "while_loop_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(functions, vec!["main"]);

    let counts = &doc["counts"];
    // 28 step events:
    //   L1 (initial AbsoluteStep) + L1, L2, L3 (one DeltaStep each) = 4
    //   first iteration body: L4, L5, L6, L7, L8                     = 5  (running 9)
    //   iterations 2..4: 3 * (L4, L5, L6, L7, L8)                    = 15 (running 24)
    //   exit iteration:  L4, L5 (jnzi taken)                          = 2  (running 26)
    //   L9 (LOG), L10 (RET)                                           = 2  (running 28)
    assert_eq!(
        counts["steps"].as_u64(),
        Some(28),
        "steps; counts={counts} (4-iteration loop should yield 28 step events)"
    );
    assert_eq!(counts["calls"].as_u64(), Some(0), "calls; counts={counts}");
    assert_eq!(
        counts["io_events"].as_u64(),
        Some(1),
        "io_events (LOG); counts={counts}"
    );

    let events = doc["events"].as_array().expect("events array");
    // 28 steps + 1 io = 29 events.
    assert_eq!(events.len(), 29, "events.len()");
    assert_step_indices_monotonic(&doc);

    // ----- Step-line order: 4 full iterations + 1 partial -------------
    let mut expected_lines: Vec<i64> = vec![1, 1, 2, 3];
    for _ in 0..4 {
        expected_lines.extend_from_slice(&[4, 5, 6, 7, 8]);
    }
    expected_lines.extend_from_slice(&[4, 5, 9, 10]);
    assert_eq!(
        observed_step_lines(&doc),
        expected_lines,
        "step lines must walk the loop exactly 4 times then exit"
    );

    // ----- Loop accumulator must take the values 0, 1, 3, 6, 10 ------
    // The accumulator register r16 evolves 0 -> 1 -> 3 -> 6 -> 10
    // across the four loop iterations.  The variable tracker
    // synthesises a *new* composite varname every time an ADD writes
    // r16 (chaining the lhs/rhs immediate-derived names), so the
    // accumulator surfaces under five different varnames:
    //   - "imm_0"                                                       value 0
    //   - "imm_0_plus_imm_1"                                            value 1
    //   - "imm_0_plus_imm_1_plus_imm_1_plus_1"                          value 3
    //   - "imm_0_plus_imm_1_plus_imm_1_plus_1_plus_imm_1_plus_1_plus_1" value 6
    //   - "imm_0_plus_..._plus_1_plus_1_plus_1_plus_1"                  value 10
    // Rather than pin the (chain-length-sensitive) exact varnames, we
    // collect every Int value that surfaces under any varname starting
    // with the `imm_0` chain prefix and assert on the exact set.  Any
    // drop or duplication of a loop iteration changes this set.
    let acc_values: std::collections::BTreeSet<i64> = observed_int_vars(&doc)
        .into_iter()
        .filter(|(n, _)| n == "imm_0" || n.starts_with("imm_0_plus_"))
        .map(|(_, v)| v)
        .collect();
    assert_eq!(
        acc_values,
        [0, 1, 3, 6, 10]
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<i64>>(),
        "accumulator (varnames in the `imm_0` chain) must take exactly the \
         values 0, 1, 3, 6, 10 across the trace"
    );

    // ----- Loop induction variable: i takes values 0, 1, 2, 3, 4, 5 --
    // The induction variable r17 is the L2 MOVI immediate `imm_1`,
    // updated by ADDI each iteration.  The tracker names the new
    // register `imm_1_plus_1`, then `imm_1_plus_1_plus_1`, etc.
    // Across the trace we expect:
    //   - 0 — the register's initial value, dumped at the step before
    //     the L2 MOVI commits (the recorder dumps `step.registers`
    //     *before* the current opcode executes).
    //   - 1, 2, 3, 4 — the values during the four loop iterations.
    //   - 5 — the final value that triggers the loop-exit comparison
    //     i > n=4.
    let i_values: std::collections::BTreeSet<i64> = observed_int_vars(&doc)
        .into_iter()
        .filter(|(n, _)| n == "imm_1" || n.starts_with("imm_1_plus_"))
        .map(|(_, v)| v)
        .collect();
    assert_eq!(
        i_values,
        [0, 1, 2, 3, 4, 5]
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<i64>>(),
        "induction variable (varnames in the `imm_1` chain) must take \
         exactly the values 0, 1, 2, 3, 4, 5 across the trace"
    );

    // ----- One io_event for the final LOG -----------------------------
    let io_events = observed_io_events(&doc);
    assert_eq!(io_events.len(), 1, "exactly one LOG io_event expected");
    let (kind, _text) = &io_events[0];
    assert_eq!(kind, "ioStderr", "LOG receipt must route to ioStderr");
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
