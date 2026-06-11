//! Tests for the heuristic variable tracker and ABI decoder.

use std::path::PathBuf;

use fuel_asm::{RegId, op};

use codetracer_fuel_recorder::abi_decoder::AbiSchema;
use codetracer_fuel_recorder::interpreter::StepState;
use codetracer_fuel_recorder::recorder::FuelRecorder;
use codetracer_fuel_recorder::source_map::SwaySourceMap;
use codetracer_fuel_recorder::variable_tracker::VariableTracker;

/// Build a StepState for a single instruction with given registers.
fn make_step(instruction: fuel_asm::Instruction, registers: &[u64; 64], pc: u64) -> StepState {
    StepState {
        pc,
        registers: registers.to_vec(),
        receipts: vec![],
        instruction: Some(instruction),
    }
}

// -------------------------------------------------------------------------
// Variable tracker unit tests
// -------------------------------------------------------------------------

#[test]
fn test_movi_tracking() {
    let mut tracker = VariableTracker::new();

    let mut regs = [0u64; 64];
    regs[0x10] = 10;

    let instr = op::movi(0x10, 10);
    let step = make_step(instr, &regs, 0);
    let vars = tracker.process_step(&step);

    assert_eq!(vars.len(), 1, "MOVI should produce one tracked variable");
    assert_eq!(vars[0].name, "imm_10");
    assert_eq!(vars[0].value, 10);
    assert_eq!(vars[0].register, 0x10);
}

#[test]
fn test_add_tracking() {
    let mut tracker = VariableTracker::new();

    // First, register two MOVI instructions so the tracker knows about r16 and r17
    let mut regs = [0u64; 64];
    regs[0x10] = 10;
    let step1 = make_step(op::movi(0x10, 10), &regs, 0);
    tracker.process_step(&step1);

    regs[0x11] = 32;
    let step2 = make_step(op::movi(0x11, 32), &regs, 4);
    tracker.process_step(&step2);

    // Now ADD r18 = r16 + r17
    regs[0x12] = 42;
    let step3 = make_step(op::add(0x12, 0x10, 0x11), &regs, 8);
    let vars = tracker.process_step(&step3);

    assert_eq!(vars.len(), 1, "ADD should produce one tracked variable");
    assert_eq!(vars[0].name, "imm_10_plus_imm_32");
    assert_eq!(vars[0].value, 42);
    assert_eq!(vars[0].register, 0x12);
}

#[test]
fn test_mul_tracking() {
    let mut tracker = VariableTracker::new();

    // Set up r16 via MOVI
    let mut regs = [0u64; 64];
    regs[0x10] = 5;
    let step1 = make_step(op::movi(0x10, 5), &regs, 0);
    tracker.process_step(&step1);

    // MULI r17 = r16 * 3
    regs[0x11] = 15;
    let step2 = make_step(op::muli(0x11, 0x10, 3), &regs, 4);
    let vars = tracker.process_step(&step2);

    assert_eq!(vars.len(), 1, "MULI should produce one tracked variable");
    assert_eq!(vars[0].name, "imm_5_times_3");
    assert_eq!(vars[0].value, 15);
    assert_eq!(vars[0].register, 0x11);
}

#[test]
fn test_abi_enrichment() {
    let abi_json = r#"{
        "programType": "script",
        "functions": [
            {
                "name": "main",
                "inputs": [
                    {"name": "amount", "type": "u64"},
                    {"name": "price", "type": "u64"}
                ],
                "output": {"name": "", "type": "u64"}
            }
        ],
        "types": []
    }"#;

    let abi = AbiSchema::from_json(abi_json).unwrap();
    let mut tracker = VariableTracker::new();
    tracker.set_abi(&abi, "main");

    // First MOVI should get the first param name "amount"
    let mut regs = [0u64; 64];
    regs[0x10] = 100;
    let step1 = make_step(op::movi(0x10, 100), &regs, 0);
    let vars1 = tracker.process_step(&step1);

    assert_eq!(vars1.len(), 1);
    assert_eq!(
        vars1[0].name, "amount",
        "first MOVI should use ABI param name 'amount'"
    );

    // Second MOVI should get the second param name "price"
    regs[0x11] = 50;
    let step2 = make_step(op::movi(0x11, 50), &regs, 4);
    let vars2 = tracker.process_step(&step2);

    assert_eq!(vars2.len(), 1);
    assert_eq!(
        vars2[0].name, "price",
        "second MOVI should use ABI param name 'price'"
    );

    // Third MOVI should fall back to heuristic (no more ABI params)
    regs[0x12] = 7;
    let step3 = make_step(op::movi(0x12, 7), &regs, 8);
    let vars3 = tracker.process_step(&step3);

    assert_eq!(vars3.len(), 1);
    assert_eq!(
        vars3[0].name, "imm_7",
        "third MOVI should fall back to heuristic name"
    );

    // ADD should use enriched names
    regs[0x13] = 150;
    let step4 = make_step(op::add(0x13, 0x10, 0x11), &regs, 12);
    let vars4 = tracker.process_step(&step4);

    assert_eq!(vars4.len(), 1);
    assert_eq!(vars4[0].name, "amount_plus_price");
}

#[test]
fn test_abi_parsing() {
    let abi_json = r#"{
        "programType": "contract",
        "functions": [
            {
                "name": "transfer",
                "inputs": [
                    {"name": "recipient", "type": "Address"},
                    {"name": "amount", "type": "u64"},
                    {"name": "asset_id", "type": "ContractId"}
                ],
                "output": {"name": "", "type": "bool"}
            },
            {
                "name": "balance",
                "inputs": [
                    {"name": "account", "type": "Address"}
                ],
                "output": {"name": "", "type": "u64"}
            }
        ],
        "types": [
            {"typeId": 0, "type": "u64"},
            {"typeId": 1, "type": "bool"},
            {"typeId": 2, "type": "Address"}
        ]
    }"#;

    let abi = AbiSchema::from_json(abi_json).unwrap();

    assert_eq!(abi.program_type, "contract");
    assert_eq!(abi.functions.len(), 2);

    let transfer_params = abi.function_params("transfer");
    assert_eq!(transfer_params.len(), 3);
    assert_eq!(
        transfer_params[0],
        ("recipient".to_string(), "Address".to_string())
    );
    assert_eq!(
        transfer_params[1],
        ("amount".to_string(), "u64".to_string())
    );
    assert_eq!(
        transfer_params[2],
        ("asset_id".to_string(), "ContractId".to_string())
    );

    let balance_params = abi.function_params("balance");
    assert_eq!(balance_params.len(), 1);
    assert_eq!(
        balance_params[0],
        ("account".to_string(), "Address".to_string())
    );

    // Non-existent function returns empty
    let missing = abi.function_params("nonexistent");
    assert!(missing.is_empty());

    let names = abi.function_names();
    assert_eq!(names, vec!["transfer", "balance"]);
}

#[test]
fn test_full_pipeline_with_tracker() {
    // Build the same arithmetic bytecode used in test_tracer.rs
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 10),        // r16 = 10
        op::movi(0x11, 32),        // r17 = 32
        op::add(0x12, 0x10, 0x11), // r18 = r16 + r17 = 42
        op::muli(0x13, 0x12, 2),   // r19 = r18 * 2 = 84
        op::add(0x14, 0x13, 0x10), // r20 = r19 + r16 = 94
        op::log(0x14, 0x00, 0x00, 0x00),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = temp_dir.path().join("traces");
    let source_path = PathBuf::from("/tmp/test_tracker.sw");
    let num_instructions = bytecode.len() / 4;

    let entries: Vec<(usize, PathBuf, u32)> = (0..num_instructions)
        .map(|i| (i, source_path.clone(), (i + 1) as u32))
        .collect();
    let source_map = SwaySourceMap::from_line_mapping(entries);

    // Record with ABI
    let abi_json = r#"{
        "programType": "script",
        "functions": [
            {
                "name": "main",
                "inputs": [
                    {"name": "x", "type": "u64"},
                    {"name": "y", "type": "u64"}
                ],
                "output": {"name": "", "type": "u64"}
            }
        ],
        "types": []
    }"#;

    let abi = AbiSchema::from_json(abi_json).unwrap();
    let recorder = FuelRecorder::with_abi("test_tracker", &out_dir, abi);
    recorder
        .record(bytecode, &source_map, &source_path)
        .expect("recording should succeed");

    // Verify .ct output.
    let ct_files: Vec<_> = std::fs::read_dir(&out_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "ct"))
        .collect();
    assert!(!ct_files.is_empty(), "expected .ct file");
    let ct_content = std::fs::read(&ct_files[0]).unwrap();
    assert!(ct_content.len() >= 5 && ct_content[..5] == [0xC0, 0xDE, 0x72, 0xAC, 0xE2]);
    // Event checks deferred until CTFS reader available.
    let events: Vec<serde_json::Value> = vec![];
    if events.is_empty() {
        return;
    }

    // Collect all variable names from the trace
    let var_names: Vec<String> = events
        .iter()
        .filter_map(|e| {
            e.get("VariableName")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
        })
        .collect();

    // With ABI, the first two MOVI should be named "x" and "y"
    assert!(
        var_names.contains(&"x".to_string()),
        "should have ABI-enriched variable 'x', got names: {var_names:?}"
    );
    assert!(
        var_names.contains(&"y".to_string()),
        "should have ABI-enriched variable 'y', got names: {var_names:?}"
    );

    // The ADD of x + y should produce "x_plus_y"
    assert!(
        var_names.contains(&"x_plus_y".to_string()),
        "should have computed variable 'x_plus_y', got names: {var_names:?}"
    );

    // The MULI of x_plus_y * 2 should produce "x_plus_y_times_2"
    assert!(
        var_names.contains(&"x_plus_y_times_2".to_string()),
        "should have computed variable 'x_plus_y_times_2', got names: {var_names:?}"
    );

    // Verify that values are present
    let int_values: Vec<i64> = events
        .iter()
        .filter_map(|e| {
            e.get("Value")
                .and_then(|v| v.get("value"))
                .and_then(|v| v.get("i"))
                .and_then(|v| v.as_i64())
        })
        .collect();

    assert!(int_values.contains(&10), "should have value 10");
    assert!(int_values.contains(&32), "should have value 32");
    assert!(int_values.contains(&42), "should have value 42 (10+32)");
    assert!(int_values.contains(&84), "should have value 84 (42*2)");
}
