//! Post-1.53 CTFS audit invariants for the Fuel/Sway recorder.
//!
//! These tests guard the structural fixes applied during the 2026-05-02 CTFS
//! audit (see `AUDIT-CTFS-2026-05.md`):
//!
//!   * The CTFS writer produces a `.ct` container starting with the
//!     canonical magic bytes (0xC0 0xDE 0x72 0xAC 0xE2).
//!   * A simple bytecode trace produces a structurally non-empty CTFS
//!     container (size + magic guard against silent regressions where an
//!     audit-related change empties the event stream).
//!
//! The 2026-05-08 convention compliance follow-up tightened §4 of
//! `Recorder-CLI-Conventions.md`: recorders are now CTFS-only and must
//! not expose a `--format` flag.  The pre-follow-up
//! `ctfs_format_advertised_in_record_help` test was replaced with
//! `test_no_format_flag_in_help` / `test_help_mentions_ct_print` (see
//! `tests/test_tracer.rs` for those).
//!
//! End-to-end content assertions on the embedded event records (e.g. that
//! `register_special_event(EventLogKind::EvmEvent, "FuelLog:…", …)`
//! actually lands in the event stream when a script executes a `LOG`
//! opcode) require the read-side `codetracer_trace_reader_nim` dev-dep
//! and a small reader-walk helper.  Tracked as an open follow-up in
//! `AUDIT-CTFS-2026-05.md` (open for Cairo, Cardano, Flow, and now Fuel).

use std::path::PathBuf;

use fuel_asm::{op, RegId};

use codetracer_fuel_recorder::recorder::FuelRecorder;
use codetracer_fuel_recorder::source_map::SwaySourceMap;

/// CTFS magic header bytes — see `codetracer-trace-format-spec/`.
const CTFS_MAGIC: [u8; 5] = [0xC0, 0xDE, 0x72, 0xAC, 0xE2];

/// Build the same simple-arithmetic bytecode that `test_tracer.rs` uses,
/// duplicated here so audit tests do not depend on test-only items in the
/// other integration test crate.
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

/// Synthetic 1:1 (instruction → line) source map.
fn synthetic_source_map(source_path: &PathBuf, num_instructions: usize) -> SwaySourceMap {
    let entries: Vec<(usize, PathBuf, u32)> = (0..num_instructions)
        .map(|i| (i, source_path.clone(), (i + 1) as u32))
        .collect();
    SwaySourceMap::from_line_mapping(entries)
}

/// Run the recorder with the simple-arithmetic bytecode (always CTFS) and
/// return the temp dir handle (kept alive for the assertion scope) plus the
/// discovered `.ct` file path.
fn run_trace() -> (tempfile::TempDir, PathBuf) {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = temp_dir.path().join("traces");
    let source_path = PathBuf::from("/tmp/test_ctfs_audit.sw");
    let bytecode = simple_arithmetic_bytecode();
    let num_instructions = bytecode.len() / 4;
    let source_map = synthetic_source_map(&source_path, num_instructions);

    let recorder = FuelRecorder::new("test_ctfs_audit", &out_dir);
    recorder
        .record(bytecode, &source_map, &source_path)
        .expect("recording should succeed");

    let ct_path = std::fs::read_dir(&out_dir)
        .expect("output dir should exist")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().and_then(|ext| ext.to_str()) == Some("ct"))
        .expect("expected a .ct container in the output dir");
    (temp_dir, ct_path)
}

#[test]
fn ctfs_writer_produces_ct_container() {
    let (_keepalive, ct_path) = run_trace();

    let bytes = std::fs::read(&ct_path).expect("ct file should be readable");
    assert!(
        bytes.len() > 64,
        "ct container should be materially populated, got {} bytes at {}",
        bytes.len(),
        ct_path.display()
    );
    assert_eq!(
        &bytes[..5],
        &CTFS_MAGIC,
        "ct file should start with canonical CTFS magic 0xC0 0xDE 0x72 0xAC 0xE2"
    );
}

/// Smoke-test that a structurally meaningful trace is produced for a
/// program containing a `LOG` opcode.  Pre-1.53 the LOG receipt was
/// silently dropped (no register_special_event); post-1.53 it is routed
/// through `EventLogKind::EvmEvent` and therefore the .ct container is
/// strictly larger than a no-op trace.  We do not assert on the embedded
/// event content directly — that needs the read-side dev-dep tracked in
/// the audit memo — but a size lower-bound + magic check guards against
/// silent regressions.
#[test]
fn log_receipt_does_not_empty_trace() {
    let (_keepalive, ct_path) = run_trace();
    let bytes = std::fs::read(&ct_path).unwrap();
    assert_eq!(&bytes[..5], &CTFS_MAGIC);
    // The simple-arithmetic program emits a LOG opcode at line 6 and a
    // ret + ScriptResult.  All three of those are now mirrored into the
    // structured event stream, on top of 7 step events and 8*r16..r23
    // variable records, so the container should comfortably exceed 256
    // bytes.  This is intentionally a loose lower bound.
    assert!(
        bytes.len() > 256,
        "expected materially populated CTFS container, got {} bytes",
        bytes.len()
    );
}
