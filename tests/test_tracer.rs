//! Integration tests for the FuelVM trace recorder.
//!
//! Tests verify that the recorder correctly produces CodeTracer trace output
//! from raw FuelVM bytecode built via fuel-asm.

use std::path::PathBuf;

use fuel_asm::{op, RegId};

use codetracer_fuel_recorder::recorder::FuelRecorder;
use codetracer_fuel_recorder::source_map::SwaySourceMap;
use codetracer_trace_writer::TraceEventsFileFormat;

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

/// Parse a JSON trace file and return the events as a JSON array.
fn parse_trace_json(trace_path: &std::path::Path) -> Vec<serde_json::Value> {
    let content = std::fs::read_to_string(trace_path)
        .expect("failed to read trace.bin");
    // The JSON format writes a single JSON array with all events
    let parsed: serde_json::Value = serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("failed to parse trace JSON: {e}"));
    match parsed {
        serde_json::Value::Array(arr) => arr,
        other => vec![other],
    }
}

#[test]
fn test_fuel_basic_execution() {
    let temp_dir = run_simple_trace(TraceEventsFileFormat::Json);
    let out_dir = temp_dir.path().join("traces");

    // Assert the output directory exists
    assert!(out_dir.exists(), "output directory should exist");

    // Assert the three trace files exist
    assert!(
        out_dir.join("trace.bin").exists(),
        "trace.bin should exist"
    );
    assert!(
        out_dir.join("trace_metadata.json").exists(),
        "trace_metadata.json should exist"
    );
    assert!(
        out_dir.join("trace_paths.json").exists(),
        "trace_paths.json should exist"
    );
}

#[test]
fn test_fuel_source_mapping() {
    let temp_dir = run_simple_trace(TraceEventsFileFormat::Json);
    let out_dir = temp_dir.path().join("traces");

    let events = parse_trace_json(&out_dir.join("trace.bin"));

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

    // Each Step event should have a valid line number (positive)
    for step in &step_events {
        let step_data = step.get("Step").unwrap();
        let line = step_data.get("line").expect("Step should have a line field");
        let line_num = line.as_i64().expect("line should be a number");
        assert!(line_num > 0, "line number should be positive, got {line_num}");
    }
}

#[test]
fn test_fuel_variable_extraction() {
    let temp_dir = run_simple_trace(TraceEventsFileFormat::Json);
    let out_dir = temp_dir.path().join("traces");

    let events = parse_trace_json(&out_dir.join("trace.bin"));

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

    // Verify that r16 and r17 appear (the first two registers we set)
    assert!(
        var_names.contains(&"r16".to_string()),
        "should have r16 variable, got names: {var_names:?}"
    );
    assert!(
        var_names.contains(&"r17".to_string()),
        "should have r17 variable, got names: {var_names:?}"
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

    // Verify trace.bin exists and is non-empty
    let trace_path = out_dir.join("trace.bin");
    assert!(trace_path.exists(), "trace.bin should exist");
    let trace_size = std::fs::metadata(&trace_path).unwrap().len();
    assert!(trace_size > 0, "trace.bin should not be empty");

    // Verify trace_metadata.json exists and is valid JSON
    let metadata_path = out_dir.join("trace_metadata.json");
    assert!(metadata_path.exists(), "trace_metadata.json should exist");
    let metadata_content = std::fs::read_to_string(&metadata_path).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&metadata_content)
        .expect("trace_metadata.json should be valid JSON");
    // Metadata should have some structure (at minimum it should be an object or array)
    assert!(
        metadata.is_object() || metadata.is_array(),
        "metadata should be a JSON object or array"
    );

    // Verify trace_paths.json exists and is valid JSON
    let paths_path = out_dir.join("trace_paths.json");
    assert!(paths_path.exists(), "trace_paths.json should exist");
    let paths_content = std::fs::read_to_string(&paths_path).unwrap();
    let paths: serde_json::Value = serde_json::from_str(&paths_content)
        .expect("trace_paths.json should be valid JSON");
    assert!(
        paths.is_object() || paths.is_array(),
        "paths should be a JSON object or array"
    );
}

#[test]
fn test_fuel_single_step_trace() {
    let temp_dir = run_simple_trace(TraceEventsFileFormat::Json);
    let out_dir = temp_dir.path().join("traces");

    let events = parse_trace_json(&out_dir.join("trace.bin"));

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
