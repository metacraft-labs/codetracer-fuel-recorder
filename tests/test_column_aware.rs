//! Column-aware step emission regression test for the Fuel recorder.
//!
//! Mirrors `codetracer-evm-recorder/tests/test_column_aware.rs` and the JS
//! recorder's `tests/integration/column-aware.test.ts` — the cross-recorder
//! contract from the M-fuel rollout of
//! `codetracer-specs/Planned-Features/Column-Aware-Navigation-Other-Languages.plan.md`:
//!
//!   * `meta.dat` bit 4 (`FLAG_HAS_COLUMN_AWARE_STEPS`) MUST be set.  This
//!     is the only externally-visible signal that downstream tooling uses
//!     to opt into column-aware navigation.
//!   * The `paths.dat` Layout A record MUST be emitted (`register_path_with_line_lengths`)
//!     for the source path, even when no per-line byte-count data is
//!     available — the writer accepts an empty line-length slice and
//!     emits a stub Layout A record that the reader treats as "no per-line
//!     data; columns resolve to None".
//!   * Each step event MUST round-trip through the column-aware writer
//!     path (`register_step_with_column`) regardless of whether the
//!     recorder has real column data — Sway does not yet emit DWARF-style
//!     column information (see `src/variable_tracker.rs` / sway#2055), so
//!     columns are universally `None` today.  When forc-pkg integration
//!     surfaces real columns the test below can be tightened to assert
//!     distinct-column values per step on a single line.
//!
//! Both assertions go through `ct-print --full` (the canonical JSON dump
//! from `codetracer-trace-format-nim`) — the same mechanism the EVM and JS
//! sibling tests use.

use std::path::PathBuf;
use std::process::Command;

use fuel_asm::{RegId, op};

use codetracer_fuel_recorder::recorder::FuelRecorder;
use codetracer_fuel_recorder::source_map::SwaySourceMap;

fn ct_print_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("codetracer-trace-format-nim")
        .join(format!("ct-print{}", std::env::consts::EXE_SUFFIX))
}

fn ct_files_in(out_dir: &std::path::Path) -> Vec<PathBuf> {
    if !out_dir.exists() {
        return Vec::new();
    }
    std::fs::read_dir(out_dir)
        .expect("read_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "ct"))
        .collect()
}

/// Same bytecode shape `test_tracer.rs::simple_arithmetic_bytecode` uses —
/// a self-contained seven-instruction MOVI/ADD/MULI/LOG/RET program.
fn simple_arithmetic_bytecode() -> Vec<u8> {
    vec![
        op::movi(0x10, 10),
        op::movi(0x11, 32),
        op::add(0x12, 0x10, 0x11),
        op::muli(0x13, 0x12, 2),
        op::add(0x14, 0x13, 0x10),
        op::log(0x14, 0x00, 0x00, 0x00),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect()
}

#[test]
fn test_column_aware_flag_and_step_path_are_set() {
    let ct_print = ct_print_path();
    if !ct_print.exists() {
        eprintln!(
            "SKIP: test_column_aware_flag_and_step_path_are_set requires ct-print at {} — \
             only available within the metacraft workspace where \
             codetracer-trace-format-nim is a sibling.",
            ct_print.display()
        );
        return;
    }

    let tmp_dir = tempfile::tempdir().expect("tempdir");
    let out_dir = tmp_dir.path().join("traces");
    let source_path = PathBuf::from("/tmp/test_column_aware.sw");
    let bytecode = simple_arithmetic_bytecode();
    let num_instructions = bytecode.len() / 4;
    let entries: Vec<(usize, PathBuf, u32)> = (0..num_instructions)
        .map(|i| (i, source_path.clone(), (i + 1) as u32))
        .collect();
    let source_map = SwaySourceMap::from_line_mapping(entries);

    let recorder = FuelRecorder::new("test_column_aware", &out_dir);
    recorder
        .record(bytecode, &source_map, &source_path)
        .expect("recording should succeed");

    let ct_files = ct_files_in(&out_dir);
    assert!(
        !ct_files.is_empty(),
        "expected a .ct container in {:?}",
        out_dir
    );

    let dump = Command::new(&ct_print)
        .args(["--full", "--strip-paths"])
        .arg(&ct_files[0])
        .output()
        .expect("failed to run ct-print --full");
    assert!(
        dump.status.success(),
        "ct-print --full should succeed; stderr: {}",
        String::from_utf8_lossy(&dump.stderr)
    );

    let doc: serde_json::Value =
        serde_json::from_slice(&dump.stdout).expect("ct-print --full should emit valid JSON");

    // --- meta.dat bit 4: FLAG_HAS_COLUMN_AWARE_STEPS -----------------
    // The recorder calls `TraceWriter::enable_column_aware_steps` before
    // the first step; the writer must set the flag on close.  This is
    // the cross-recorder signal that downstream tooling reads to know
    // whether the trace carries column-aware step encoding.
    let has_column_aware = doc["metadata"]["flags"]["has_column_aware_steps"].as_bool();
    assert_eq!(
        has_column_aware,
        Some(true),
        "trace metadata must advertise has_column_aware_steps=true; got {:?}",
        doc["metadata"],
    );

    // --- Step events still surface --------------------------------------
    // `register_step_with_column` replaced `register_step` at both call
    // sites; the resulting step count is unchanged (one step event per
    // executed instruction since each maps to a fresh line in the
    // synthetic source map).  Asserts that the column-aware swap did not
    // accidentally drop the step path.
    let counts = &doc["counts"];
    assert_eq!(
        counts["steps"].as_u64(),
        Some(8),
        "expected 8 step events for the 7-instruction synthetic fixture; counts={counts}",
    );

    // --- Column field surfaces as None (Sway has no DWARF columns yet) --
    // ct-print --full normalises the absent column to JSON `null`; when
    // forc-pkg starts surfacing column data the recorder will start
    // emitting non-null columns and this assertion can be flipped to
    // pin a specific value.
    let events = doc["events"].as_array().expect("events array");
    let step_events: Vec<&serde_json::Value> =
        events.iter().filter(|e| e["kind"] == "step").collect();
    assert!(
        !step_events.is_empty(),
        "expected at least one step event in ct-print --full dump"
    );
    for ev in &step_events {
        // The column key may either be absent or present-and-null; both
        // are valid encodings of "no column data".  Reject any non-null
        // column for the synthetic fixture so this test catches a
        // regression where the recorder starts forwarding stale
        // column data.
        if let Some(col) = ev.get("column") {
            assert!(
                col.is_null(),
                "expected null column on synthetic fixture step; got {col}"
            );
        }
    }
}
