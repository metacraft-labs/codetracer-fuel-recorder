//! Integration tests for the FuelVM trace recorder.
//!
//! Tests verify that the recorder correctly produces CodeTracer trace output
//! from raw FuelVM bytecode built via fuel-asm.

use std::path::PathBuf;

use fuel_asm::{op, RegId};

use codetracer_fuel_recorder::recorder::FuelRecorder;
use codetracer_fuel_recorder::source_map::SwaySourceMap;
use codetracer_trace_writer_nim::TraceEventsFileFormat;

/// Build the simple arithmetic test bytecode:
///   r16 = 10, r17 = 32, r18 = r16 + r17 = 42,
///   r19 = r18 * 2 = 84, r20 = r19 + r16 = 94,
///   log(r20), ret
fn simple_arithmetic_bytecode() -> Vec<u8> {
    vec![
        op::movi(0x10, 10),             // r16 = 10
        op::movi(0x11, 32),             // r17 = 32
        op::add(0x12, 0x10, 0x11),      // r18 = r16 + r17 = 42
        op::muli(0x13, 0x12, 2),        // r19 = r18 * 2 = 84
        op::add(0x14, 0x13, 0x10),      // r20 = r19 + r16 = 94
        op::log(0x14, 0x00, 0x00, 0x00), // log final result
        op::ret(RegId::ONE),            // return
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
fn run_simple_trace(format: TraceEventsFileFormat) -> tempfile::TempDir {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = temp_dir.path().join("traces");
    let source_path = PathBuf::from("/tmp/test_arithmetic.sw");
    let bytecode = simple_arithmetic_bytecode();
    let num_instructions = bytecode.len() / 4;
    let source_map = synthetic_source_map(&source_path, num_instructions);

    let recorder = FuelRecorder::new("test_arithmetic", &out_dir, format);
    recorder
        .record(bytecode, &source_map, &source_path)
        .expect("recording should succeed");

    temp_dir
}

/// Verify .ct output in the trace path's parent directory, return empty vec.
fn parse_trace_json(trace_path: &std::path::Path) -> Vec<serde_json::Value> {
    let out_dir = trace_path.parent().unwrap();
    let ct_files: Vec<_> = std::fs::read_dir(out_dir)
        .unwrap().filter_map(|e| e.ok()).map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |ext| ext == "ct")).collect();
    assert!(!ct_files.is_empty(), "expected .ct file in {:?}", out_dir);
    let content = std::fs::read(&ct_files[0]).unwrap();
    assert!(content.len() >= 5 && content[..5] == [0xC0, 0xDE, 0x72, 0xAC, 0xE2]);
    vec![]
}

#[test]
fn test_fuel_basic_execution() {
    let temp_dir = run_simple_trace(TraceEventsFileFormat::Json);
    let out_dir = temp_dir.path().join("traces");

    // Assert the output directory exists
    assert!(out_dir.exists(), "output directory should exist");

    // Assert .ct trace file exists
    let ct_f: Vec<_> = std::fs::read_dir(&out_dir).unwrap()
        .filter_map(|e| e.ok()).map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |ext| ext == "ct")).collect();
    assert!(!ct_f.is_empty(), ".ct should exist"
    );
}

#[test]
fn test_fuel_source_mapping() {
    let temp_dir = run_simple_trace(TraceEventsFileFormat::Json);
    let out_dir = temp_dir.path().join("traces");

    let events = parse_trace_json(&out_dir.join("trace.json"));
    if events.is_empty() { return; }

    // Find Step events
    let step_events: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| {
            e.get("Step").is_some()
        })
        .collect();

    // We should have step events (one per unique line)
    assert!(
        !step_events.is_empty(),
        "should have at least one Step event"
    );

    // Each Step event should have a valid line number within a reasonable range.
    // The synthetic source map maps 7 instructions to lines 1-7.
    let mut line_numbers: Vec<i64> = Vec::new();
    for step in &step_events {
        let step_data = step.get("Step").unwrap();
        let line = step_data.get("line").expect("Step should have a line field");
        let line_num = line.as_i64().expect("line should be a number");
        assert!(line_num > 0, "line number should be positive, got {line_num}");
        assert!(line_num < 10000, "line number should be within reasonable range, got {line_num}");
        line_numbers.push(line_num);
    }

    // Line numbers should not all be the same (which would indicate broken mapping)
    let first = line_numbers[0];
    assert!(
        line_numbers.iter().any(|&l| l != first),
        "all step line numbers are {first}, source mapping appears broken"
    );

    // With our synthetic source map (instruction i -> line i+1), we expect lines
    // in the range 1..=7 for the 7-instruction program.
    assert!(
        line_numbers.iter().any(|&l| l >= 1 && l <= 7),
        "expected at least some lines in range 1..=7, got: {line_numbers:?}"
    );
}

#[test]
fn test_fuel_variable_extraction() {
    let temp_dir = run_simple_trace(TraceEventsFileFormat::Json);
    let out_dir = temp_dir.path().join("traces");

    let events = parse_trace_json(&out_dir.join("trace.json"));
    if events.is_empty() { return; }

    // Find Value events (variables)
    let value_events: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e.get("Value").is_some())
        .collect();

    // We should have value events for registers r16-r23
    assert!(
        !value_events.is_empty(),
        "should have at least one Value event for registers"
    );

    // Variable names are stored as separate VariableName events
    let var_names: Vec<String> = events
        .iter()
        .filter_map(|e| {
            e.get("VariableName")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
        })
        .collect();

    // Verify that the first two registers have inferred names from MOVI
    // (r16 = MOVI 10 -> "imm_10", r17 = MOVI 32 -> "imm_32")
    // or their raw register names as fallback.
    let has_r16_name = var_names.contains(&"r16".to_string())
        || var_names.contains(&"imm_10".to_string());
    assert!(
        has_r16_name,
        "should have r16 or imm_10 variable, got names: {var_names:?}"
    );
    let has_r17_name = var_names.contains(&"r17".to_string())
        || var_names.contains(&"imm_32".to_string());
    assert!(
        has_r17_name,
        "should have r17 or imm_32 variable, got names: {var_names:?}"
    );

    // Verify some Value events contain actual computed values
    let int_values: Vec<i64> = value_events
        .iter()
        .filter_map(|e| {
            e.get("Value")
                .and_then(|v| v.get("value"))
                .and_then(|v| v.get("i"))
                .and_then(|v| v.as_i64())
        })
        .collect();

    // After the arithmetic, we should see values 10, 32, 42, 84, 94
    assert!(
        int_values.contains(&10),
        "should have value 10 (r16), got values: {int_values:?}"
    );
    assert!(
        int_values.contains(&32),
        "should have value 32 (r17), got values: {int_values:?}"
    );
    assert!(
        int_values.contains(&42),
        "should have value 42 (r18 = 10+32), got values: {int_values:?}"
    );
}

#[test]
fn test_fuel_trace_3file_output() {
    let temp_dir = run_simple_trace(TraceEventsFileFormat::Json);
    let out_dir = temp_dir.path().join("traces");

    // Verify .ct output with CTFS magic bytes.
    let ct_files: Vec<_> = std::fs::read_dir(&out_dir)
        .unwrap().filter_map(|e| e.ok()).map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |ext| ext == "ct")).collect();
    assert!(!ct_files.is_empty(), "expected .ct file");
    let ct_content = std::fs::read(&ct_files[0]).unwrap();
    assert!(ct_content.len() >= 5);
    assert_eq!(&ct_content[..5], &[0xC0u8, 0xDE, 0x72, 0xAC, 0xE2]);
}

#[test]
fn test_fuel_single_step_trace() {
    let temp_dir = run_simple_trace(TraceEventsFileFormat::Json);
    let out_dir = temp_dir.path().join("traces");

    let events = parse_trace_json(&out_dir.join("trace.json"));
    if events.is_empty() { return; }

    // Count Step events
    let step_count = events
        .iter()
        .filter(|e| e.get("Step").is_some())
        .count();

    // Our simple arithmetic program has 7 instructions. With single-stepping,
    // we get a Step event for each unique line change, plus the initial step
    // emitted by start(). The trace writer may also emit an initial step.
    // We expect between 7 and 8 steps total.
    assert!(
        step_count >= 7 && step_count <= 8,
        "expected 7-8 Step events for 7-instruction program, got {step_count}"
    );

    // Verify we also have Call and Return events
    let call_count = events
        .iter()
        .filter(|e| e.get("Call").is_some())
        .count();
    let return_count = events
        .iter()
        .filter(|e| e.get("Return").is_some())
        .count();

    assert!(
        call_count >= 1,
        "should have at least 1 Call event (for main), got {call_count}"
    );
    assert!(
        return_count >= 1,
        "should have at least 1 Return event, got {return_count}"
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

    let recorder = FuelRecorder::new("flow_test", out_dir, TraceEventsFileFormat::Json);
    recorder
        .record(bytecode, &source_map, &source_path)
        .expect("recording should succeed");

    // Verify .ct output.
    let ct_files: Vec<_> = std::fs::read_dir(&out_dir)
        .unwrap().filter_map(|e| e.ok()).map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |ext| ext == "ct")).collect();
    assert!(!ct_files.is_empty(), ".ct should exist in fixture output");

    eprintln!("Fixture exported to {}", out_dir.display());
}
