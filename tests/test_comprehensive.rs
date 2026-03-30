//! Comprehensive integration tests for the FuelVM trace recorder.
//!
//! Each test builds a FuelVM bytecode program programmatically using
//! fuel_asm instructions, runs it through the recorder, and verifies
//! the trace output (Step events, Call/Return events, variable values).
//!
//! The tests cover: arithmetic, comparison/branching, register manipulation,
//! memory operations, logging, and control flow patterns (loops, nested
//! branches, early return).

use std::path::PathBuf;

use fuel_asm::{op, RegId};

use codetracer_fuel_recorder::interpreter::{FuelInterpreter, StepState};
use codetracer_fuel_recorder::recorder::FuelRecorder;
use codetracer_fuel_recorder::source_map::SwaySourceMap;
use codetracer_trace_writer::TraceEventsFileFormat;

// =========================================================================
// Helpers
// =========================================================================

/// Create a synthetic source map (one instruction per line).
fn synthetic_source_map(source_path: &PathBuf, num_instructions: usize) -> SwaySourceMap {
    let entries: Vec<(usize, PathBuf, u32)> = (0..num_instructions)
        .map(|i| (i, source_path.clone(), (i + 1) as u32))
        .collect();
    SwaySourceMap::from_line_mapping(entries)
}

/// Run bytecode through the recorder with JSON output, return parsed events.
fn record_and_parse(bytecode: &[u8]) -> (tempfile::TempDir, Vec<serde_json::Value>) {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = temp_dir.path().join("traces");
    let source_path = PathBuf::from("/tmp/comprehensive_test.sw");
    let num_instructions = bytecode.len() / 4;
    let source_map = synthetic_source_map(&source_path, num_instructions);

    let recorder = FuelRecorder::new("comprehensive_test", &out_dir, TraceEventsFileFormat::Json);
    recorder
        .record(bytecode.to_vec(), &source_map, &source_path)
        .expect("recording should succeed");

    let content = std::fs::read_to_string(out_dir.join("trace.bin"))
        .expect("failed to read trace.bin");
    let events: Vec<serde_json::Value> =
        serde_json::from_str(&content).expect("failed to parse trace JSON");

    (temp_dir, events)
}

/// Run bytecode through the interpreter and collect step states.
fn run_and_collect_steps(bytecode: Vec<u8>) -> Vec<(u64, Vec<u64>)> {
    let interp = FuelInterpreter::new(bytecode).expect("interpreter creation failed");
    let mut steps: Vec<(u64, Vec<u64>)> = Vec::new();
    interp
        .run_with_callback(|step: &StepState| {
            steps.push((step.pc, step.registers.clone()));
        })
        .expect("execution should succeed");
    steps
}

/// Extract all integer values from Value events in the trace.
fn extract_int_values(events: &[serde_json::Value]) -> Vec<i64> {
    events
        .iter()
        .filter_map(|e| {
            e.get("Value")
                .and_then(|v| v.get("value"))
                .and_then(|v| v.get("i"))
                .and_then(|v| v.as_i64())
        })
        .collect()
}

/// Extract all variable names from VariableName events.
fn extract_var_names(events: &[serde_json::Value]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| {
            e.get("VariableName")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
        })
        .collect()
}

/// Count Step events.
fn count_steps(events: &[serde_json::Value]) -> usize {
    events.iter().filter(|e| e.get("Step").is_some()).count()
}

/// Count Call events.
fn count_calls(events: &[serde_json::Value]) -> usize {
    events.iter().filter(|e| e.get("Call").is_some()).count()
}

/// Count Return events.
fn count_returns(events: &[serde_json::Value]) -> usize {
    events.iter().filter(|e| e.get("Return").is_some()).count()
}

// =========================================================================
// 1. Arithmetic operations
// =========================================================================

/// Test ADD, SUB, MUL, DIV, MOD with register operands.
#[test]
fn test_arithmetic_register_ops() {
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 10),        // r16 = 10
        op::movi(0x11, 20),        // r17 = 20
        op::add(0x12, 0x10, 0x11), // r18 = 10 + 20 = 30
        op::sub(0x13, 0x11, 0x10), // r19 = 20 - 10 = 10
        op::mul(0x14, 0x10, 0x11), // r20 = 10 * 20 = 200
        op::div(0x15, 0x11, 0x10), // r21 = 20 / 10 = 2
        op::mod_(0x16, 0x11, 0x10), // r22 = 20 % 10 = 0
        op::log(0x12, 0x13, 0x14, 0x15), // log results
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    assert!(!steps.is_empty(), "should have steps");

    // Find the last step before ret to check register values
    let last = &steps[steps.len() - 1];
    let regs = &last.1;

    // After all arithmetic instructions execute, verify registers
    assert_eq!(regs[0x12], 30, "ADD: 10 + 20 = 30");
    assert_eq!(regs[0x13], 10, "SUB: 20 - 10 = 10");
    assert_eq!(regs[0x14], 200, "MUL: 10 * 20 = 200");
    assert_eq!(regs[0x15], 2, "DIV: 20 / 10 = 2");
    assert_eq!(regs[0x16], 0, "MOD: 20 % 10 = 0");

    // Verify trace output
    let (_dir, events) = record_and_parse(&bytecode);
    let values = extract_int_values(&events);
    assert!(values.contains(&30), "trace should contain ADD result 30");
    assert!(values.contains(&200), "trace should contain MUL result 200");
    assert!(values.contains(&2), "trace should contain DIV result 2");
}

/// Test ADDI, SUBI, MULI, DIVI, MODI with immediate operands.
#[test]
fn test_arithmetic_immediate_ops() {
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 100),         // r16 = 100
        op::addi(0x11, 0x10, 50),    // r17 = 100 + 50 = 150
        op::subi(0x12, 0x10, 30),    // r18 = 100 - 30 = 70
        op::muli(0x13, 0x10, 3),     // r19 = 100 * 3 = 300
        op::divi(0x14, 0x10, 4),     // r20 = 100 / 4 = 25
        op::modi(0x15, 0x10, 7),     // r21 = 100 % 7 = 2
        op::log(0x11, 0x12, 0x13, 0x14),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    let regs = &last.1;

    assert_eq!(regs[0x11], 150, "ADDI: 100 + 50 = 150");
    assert_eq!(regs[0x12], 70, "SUBI: 100 - 30 = 70");
    assert_eq!(regs[0x13], 300, "MULI: 100 * 3 = 300");
    assert_eq!(regs[0x14], 25, "DIVI: 100 / 4 = 25");
    assert_eq!(regs[0x15], 2, "MODI: 100 % 7 = 2");

    let (_dir, events) = record_and_parse(&bytecode);
    let values = extract_int_values(&events);
    assert!(values.contains(&150), "trace should contain ADDI result 150");
    assert!(values.contains(&300), "trace should contain MULI result 300");
}

/// Test chained arithmetic producing a larger computation.
#[test]
fn test_arithmetic_chained() {
    // Compute: ((10 + 20) * 3) - 5 = 85
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 10),          // r16 = 10
        op::movi(0x11, 20),          // r17 = 20
        op::add(0x12, 0x10, 0x11),   // r18 = 30
        op::muli(0x13, 0x12, 3),     // r19 = 90
        op::subi(0x14, 0x13, 5),     // r20 = 85
        op::log(0x14, 0x00, 0x00, 0x00),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    assert_eq!(last.1[0x14], 85, "chained arithmetic: ((10+20)*3)-5 = 85");

    let (_dir, events) = record_and_parse(&bytecode);
    let values = extract_int_values(&events);
    assert!(values.contains(&85), "trace should contain final result 85");
    assert!(values.contains(&30), "trace should contain intermediate 30");
    assert!(values.contains(&90), "trace should contain intermediate 90");
}

// =========================================================================
// 2. Comparison and branching
// =========================================================================

/// Test EQ, GT, LT comparisons.
#[test]
fn test_comparison_ops() {
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 42),          // r16 = 42
        op::movi(0x11, 42),          // r17 = 42
        op::movi(0x12, 10),          // r18 = 10
        op::eq(0x13, 0x10, 0x11),    // r19 = (42 == 42) = 1
        op::gt(0x14, 0x10, 0x12),    // r20 = (42 > 10) = 1
        op::lt(0x15, 0x12, 0x10),    // r21 = (10 < 42) = 1
        op::gt(0x16, 0x12, 0x10),    // r22 = (10 > 42) = 0
        op::eq(0x17, 0x10, 0x12),    // r23 = (42 == 10) = 0
        op::log(0x13, 0x14, 0x15, 0x16),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    let regs = &last.1;

    assert_eq!(regs[0x13], 1, "EQ: 42 == 42 should be 1");
    assert_eq!(regs[0x14], 1, "GT: 42 > 10 should be 1");
    assert_eq!(regs[0x15], 1, "LT: 10 < 42 should be 1");
    assert_eq!(regs[0x16], 0, "GT: 10 > 42 should be 0");
    assert_eq!(regs[0x17], 0, "EQ: 42 == 10 should be 0");

    let (_dir, events) = record_and_parse(&bytecode);
    let values = extract_int_values(&events);
    // Registers r19-r23 (0x13-0x17) are tracked: should see 1s and 0s
    assert!(values.contains(&1), "trace should contain comparison result 1");
}

/// Test JNZI: conditional jump when register is not zero.
/// Program: if r16 != 0, skip one instruction.
#[test]
fn test_jnzi_conditional_jump() {
    // Layout (instruction indices, relative to script start):
    //   0: MOVI r16, 1          -- r16 = 1 (nonzero)
    //   1: MOVI r17, 100        -- r17 = 100 (value if not jumped)
    //   2: JNZI r16, <instr 4>  -- if r16 != 0, jump to instruction 4
    //   3: MOVI r17, 999        -- r17 = 999 (skipped if jump taken)
    //   4: MOVI r18, 200        -- r18 = 200 (landing pad)
    //   5: LOG r17, r18, ...
    //   6: RET
    //
    // FuelVM scripts start execution after a preamble. The JNZI target is
    // an absolute byte offset in the VM address space.  We need to account
    // for the script transaction preamble which places user instructions
    // starting at some offset.  We will discover the correct offset
    // dynamically by running with an unconditional program first.

    // First, figure out the base PC offset by running a trivial program.
    let probe: Vec<u8> = vec![op::ret(RegId::ONE)].into_iter().collect();
    let probe_steps = run_and_collect_steps(probe);
    let base_pc = probe_steps[0].0; // PC of the first user instruction

    // Now build the real program. JNZI target is in words (PC / 4).
    let target_word = ((base_pc / 4) + 4) as u32; // instruction index 4

    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 1),             // 0: r16 = 1
        op::movi(0x11, 100),           // 1: r17 = 100
        op::jnzi(0x10, target_word),   // 2: if r16 != 0, jump to 4
        op::movi(0x11, 999),           // 3: r17 = 999 (should be skipped)
        op::movi(0x12, 200),           // 4: r18 = 200
        op::log(0x11, 0x12, 0x00, 0x00),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    let regs = &last.1;

    assert_eq!(regs[0x11], 100, "JNZI should skip instruction 3, r17 stays 100");
    assert_eq!(regs[0x12], 200, "instruction 4 should execute");

    // Verify the trace records the jump
    let (_dir, events) = record_and_parse(&bytecode);
    let step_count = count_steps(&events);
    // With jump, we skip instruction 3, so fewer steps
    assert!(step_count >= 5, "should have at least 5 step events, got {step_count}");
}

/// Test JNEI: conditional jump when two registers are not equal.
#[test]
fn test_jnei_conditional_jump() {
    // Probe for base PC
    let probe: Vec<u8> = vec![op::ret(RegId::ONE)].into_iter().collect();
    let probe_steps = run_and_collect_steps(probe);
    let base_pc = probe_steps[0].0;
    let target_word = ((base_pc / 4) + 4) as u16;

    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 10),            // 0: r16 = 10
        op::movi(0x11, 20),            // 1: r17 = 20
        op::jnei(0x10, 0x11, target_word), // 2: if r16 != r17, jump to 4
        op::movi(0x12, 999),           // 3: skipped
        op::movi(0x12, 42),            // 4: r18 = 42
        op::log(0x12, 0x00, 0x00, 0x00),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    assert_eq!(last.1[0x12], 42, "JNEI should jump when r16 != r17, r18 = 42");
}

/// Test JI: unconditional jump.
#[test]
fn test_ji_unconditional_jump() {
    let probe: Vec<u8> = vec![op::ret(RegId::ONE)].into_iter().collect();
    let probe_steps = run_and_collect_steps(probe);
    let base_pc = probe_steps[0].0;
    let target_word = ((base_pc / 4) + 3) as u32;

    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 10),            // 0: r16 = 10
        op::ji(target_word),           // 1: jump to instruction 3
        op::movi(0x10, 999),           // 2: skipped
        op::movi(0x11, 42),            // 3: r17 = 42
        op::log(0x10, 0x11, 0x00, 0x00),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    assert_eq!(last.1[0x10], 10, "JI: r16 should remain 10 (instruction 2 skipped)");
    assert_eq!(last.1[0x11], 42, "JI: r17 should be 42 (instruction 3 executed)");
}

// =========================================================================
// 3. Register manipulation
// =========================================================================

/// Test MOVI (load immediate) and MOVE (register copy).
#[test]
fn test_movi_and_move() {
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 0x1234),        // r16 = 0x1234
        op::move_(0x11, 0x10),         // r17 = r16 = 0x1234
        op::movi(0x12, 0),             // r18 = 0
        op::move_(0x12, 0x11),         // r18 = r17 = 0x1234
        op::log(0x10, 0x11, 0x12, 0x00),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    let regs = &last.1;

    assert_eq!(regs[0x10], 0x1234, "MOVI: r16 = 0x1234");
    assert_eq!(regs[0x11], 0x1234, "MOVE: r17 = r16 = 0x1234");
    assert_eq!(regs[0x12], 0x1234, "MOVE: r18 = r17 = 0x1234");

    let (_dir, events) = record_and_parse(&bytecode);
    let values = extract_int_values(&events);
    assert!(
        values.contains(&0x1234),
        "trace should contain MOVI value 0x1234"
    );
}

/// Test MROO: integer square root.
#[test]
fn test_mroo_integer_sqrt() {
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 144),          // r16 = 144
        op::movi(0x11, 2),            // r17 = 2 (square root = nth root where n=2)
        op::mroo(0x12, 0x10, 0x11),   // r18 = isqrt(144) = 12
        op::movi(0x13, 27),           // r19 = 27
        op::movi(0x14, 3),            // r20 = 3 (cube root)
        op::mroo(0x15, 0x13, 0x14),   // r21 = icbrt(27) = 3
        op::log(0x12, 0x15, 0x00, 0x00),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    assert_eq!(last.1[0x12], 12, "MROO: isqrt(144) = 12");
    assert_eq!(last.1[0x15], 3, "MROO: icbrt(27) = 3");

    let (_dir, events) = record_and_parse(&bytecode);
    let values = extract_int_values(&events);
    assert!(values.contains(&12), "trace should contain sqrt(144) = 12");
    assert!(values.contains(&3), "trace should contain cbrt(27) = 3");
}

// =========================================================================
// 4. Memory operations
// =========================================================================

/// Test ALOC, SW (store word), LW (load word).
#[test]
fn test_memory_store_load() {
    // Allocate heap memory, store a word, load it back.
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 8),            // r16 = 8 (bytes to allocate)
        op::aloc(0x10),               // allocate 8 bytes on heap
        // HP register (RegId::HP = 6) now points to the allocated memory
        op::movi(0x11, 0xCAFE),       // r17 = 0xCAFE (value to store)
        op::sw(RegId::HP, 0x11, 0),   // store r17 at HP+0
        op::lw(0x12, RegId::HP, 0),   // r18 = load word from HP+0
        op::log(0x11, 0x12, 0x00, 0x00),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    assert_eq!(
        last.1[0x12], 0xCAFE,
        "LW should load the same value that SW stored"
    );
    assert_eq!(last.1[0x11], last.1[0x12], "stored and loaded values should match");

    let (_dir, events) = record_and_parse(&bytecode);
    let values = extract_int_values(&events);
    assert!(values.contains(&0xCAFE), "trace should contain stored value 0xCAFE");
}

/// Test MCL (memory clear) -- clear a region of memory.
#[test]
fn test_memory_clear() {
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 16),           // r16 = 16 bytes to allocate
        op::aloc(0x10),               // allocate 16 bytes
        op::movi(0x11, 0xBEEF),       // r17 = value
        op::sw(RegId::HP, 0x11, 0),   // store at HP
        op::movi(0x12, 8),            // r18 = 8 bytes to clear
        op::mcl(RegId::HP, 0x12),     // clear 8 bytes starting at HP
        op::lw(0x13, RegId::HP, 0),   // r19 = load from HP (should be 0 after clear)
        op::log(0x13, 0x00, 0x00, 0x00),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    assert_eq!(last.1[0x13], 0, "MCL: memory should be cleared to 0");
}

/// Test MCP (memory copy).
#[test]
fn test_memory_copy() {
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 32),           // r16 = 32 bytes to allocate (two 8-byte regions)
        op::aloc(0x10),               // allocate
        op::movi(0x11, 0x4242),       // r17 = value
        op::sw(RegId::HP, 0x11, 0),   // store at HP (source region)
        // Compute destination = HP + 16
        op::addi(0x12, RegId::HP, 16), // r18 = HP + 16
        op::movi(0x13, 8),            // r19 = 8 bytes to copy
        op::mcp(0x12, RegId::HP, 0x13), // copy 8 bytes from HP to HP+16
        op::lw(0x14, 0x12, 0),        // r20 = load from destination
        op::log(0x11, 0x14, 0x00, 0x00),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    assert_eq!(last.1[0x14], 0x4242, "MCP: copied value should match source");
}

// =========================================================================
// 5. Logging
// =========================================================================

/// Test LOG instruction (logs 4 register values).
#[test]
fn test_log_instruction() {
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 111),          // r16 = 111
        op::movi(0x11, 222),          // r17 = 222
        op::movi(0x12, 333),          // r18 = 333
        op::movi(0x13, 444),          // r19 = 444
        op::log(0x10, 0x11, 0x12, 0x13),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let _steps = run_and_collect_steps(bytecode.clone());

    // Verify the LOG instruction was executed (it produces a Log receipt)
    let interp = FuelInterpreter::new(bytecode.clone()).expect("interp");
    let mut saw_log_receipt = false;
    interp
        .run_with_callback(|step: &StepState| {
            for receipt in &step.receipts {
                if let fuel_tx::Receipt::Log { ra, rb, rc, rd, .. } = receipt {
                    if *ra == 111 && *rb == 222 && *rc == 333 && *rd == 444 {
                        saw_log_receipt = true;
                    }
                }
            }
        })
        .unwrap();

    assert!(saw_log_receipt, "should see Log receipt with values 111,222,333,444");

    // Verify trace captures the values
    let (_dir, events) = record_and_parse(&bytecode);
    let values = extract_int_values(&events);
    assert!(values.contains(&111), "trace should contain log value 111");
    assert!(values.contains(&222), "trace should contain log value 222");
    assert!(values.contains(&333), "trace should contain log value 333");
    assert!(values.contains(&444), "trace should contain log value 444");
}

/// Test LOGD instruction (logs data from memory).
#[test]
fn test_logd_instruction() {
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 16),           // r16 = 16 bytes to allocate
        op::aloc(0x10),               // allocate
        op::movi(0x11, 0xDEAD),       // r17 = value
        op::sw(RegId::HP, 0x11, 0),   // store at HP
        op::movi(0x12, 0),            // r18 = 0 (ra for logd)
        op::movi(0x13, 0),            // r19 = 0 (rb for logd)
        op::movi(0x14, 8),            // r20 = 8 (length)
        op::logd(0x12, 0x13, RegId::HP, 0x14), // logd(0, 0, HP, 8)
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let interp = FuelInterpreter::new(bytecode.clone()).expect("interp");
    let mut saw_logd = false;
    interp
        .run_with_callback(|step: &StepState| {
            for receipt in &step.receipts {
                if let fuel_tx::Receipt::LogData { data, .. } = receipt {
                    if data.is_some() {
                        saw_logd = true;
                    }
                }
            }
        })
        .unwrap();

    assert!(saw_logd, "should see LogData receipt");

    // Also verify through the recorder
    let (_dir, events) = record_and_parse(&bytecode);
    let step_count = count_steps(&events);
    assert!(step_count >= 7, "should have steps for all instructions");
}

// =========================================================================
// 6. Control flow patterns
// =========================================================================

/// Simple branch: if value > threshold, set result to 1, else 0.
#[test]
fn test_simple_branch() {
    let probe: Vec<u8> = vec![op::ret(RegId::ONE)].into_iter().collect();
    let probe_steps = run_and_collect_steps(probe);
    let base_pc = probe_steps[0].0;

    // if r16 > 15: r18 = 1  else: r18 = 0
    //
    // Layout:
    //   0: MOVI r16, 20        -- value = 20
    //   1: MOVI r17, 15        -- threshold = 15
    //   2: GT r18, r16, r17    -- r18 = (20 > 15) = 1
    //   3: JNZI r18, <6>       -- if true, jump to 6
    //   4: MOVI r18, 0         -- false branch: r18 = 0
    //   5: JI <7>              -- skip true branch
    //   6: MOVI r18, 1         -- true branch: r18 = 1
    //   7: LOG r18, ...
    //   8: RET

    let jnzi_target = ((base_pc / 4) + 6) as u32;
    let ji_target = ((base_pc / 4) + 7) as u32;

    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 20),           // 0: value = 20
        op::movi(0x11, 15),           // 1: threshold = 15
        op::gt(0x12, 0x10, 0x11),     // 2: r18 = (20 > 15) = 1
        op::jnzi(0x12, jnzi_target),  // 3: if true, jump to 6
        op::movi(0x12, 0),            // 4: false branch
        op::ji(ji_target),            // 5: skip true branch
        op::movi(0x12, 1),            // 6: true branch
        op::log(0x12, 0x00, 0x00, 0x00), // 7
        op::ret(RegId::ONE),          // 8
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    assert_eq!(last.1[0x12], 1, "branch: 20 > 15 should take true branch, r18 = 1");

    // Verify trace has correct steps (some instructions skipped)
    let (_dir, events) = record_and_parse(&bytecode);
    let step_count = count_steps(&events);
    // Instructions 4 and 5 are skipped, so we should see about 7 steps
    assert!(
        step_count >= 5 && step_count <= 9,
        "branch should produce 5-9 step events, got {step_count}"
    );
}

/// Test simple branch with false condition.
#[test]
fn test_simple_branch_false() {
    let probe: Vec<u8> = vec![op::ret(RegId::ONE)].into_iter().collect();
    let probe_steps = run_and_collect_steps(probe);
    let base_pc = probe_steps[0].0;

    let jnzi_target = ((base_pc / 4) + 6) as u32;
    let ji_target = ((base_pc / 4) + 7) as u32;

    // Same structure but value (5) is NOT > threshold (15)
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 5),            // 0: value = 5
        op::movi(0x11, 15),           // 1: threshold = 15
        op::gt(0x12, 0x10, 0x11),     // 2: r18 = (5 > 15) = 0
        op::jnzi(0x12, jnzi_target),  // 3: jump NOT taken (r18 = 0)
        op::movi(0x12, 0),            // 4: false branch (executed)
        op::ji(ji_target),            // 5: skip true branch
        op::movi(0x12, 1),            // 6: true branch (skipped)
        op::log(0x12, 0x00, 0x00, 0x00), // 7
        op::ret(RegId::ONE),          // 8
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    assert_eq!(last.1[0x12], 0, "branch: 5 > 15 is false, should take false branch, r18 = 0");
}

/// Loop: count down from 5 to 0.
#[test]
fn test_loop_countdown() {
    let probe: Vec<u8> = vec![op::ret(RegId::ONE)].into_iter().collect();
    let probe_steps = run_and_collect_steps(probe);
    let base_pc = probe_steps[0].0;

    // Layout:
    //   0: MOVI r16, 5         -- counter = 5
    //   1: MOVI r17, 0         -- accumulator = 0
    //   2: JNZI r16, <4>       -- loop_start: if counter != 0, goto body (4)
    //   3: JI <7>              -- exit loop (jump to 7)
    //   4: ADDI r17, r17, 1    -- body: accumulator += 1
    //   5: SUBI r16, r16, 1    -- counter -= 1
    //   6: JI <2>              -- jump back to loop_start
    //   7: LOG r16, r17, ...   -- log final values
    //   8: RET

    let body_target = ((base_pc / 4) + 4) as u32;
    let exit_target = ((base_pc / 4) + 7) as u32;
    let loop_start = ((base_pc / 4) + 2) as u32;

    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 5),            // 0: counter = 5
        op::movi(0x11, 0),            // 1: accumulator = 0
        op::jnzi(0x10, body_target),  // 2: if counter != 0, goto 4
        op::ji(exit_target),          // 3: exit loop
        op::addi(0x11, 0x11, 1),      // 4: accumulator += 1
        op::subi(0x10, 0x10, 1),      // 5: counter -= 1
        op::ji(loop_start),           // 6: jump to loop_start
        op::log(0x10, 0x11, 0x00, 0x00), // 7: log
        op::ret(RegId::ONE),          // 8
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    assert_eq!(last.1[0x10], 0, "loop: counter should be 0 after countdown");
    assert_eq!(last.1[0x11], 5, "loop: accumulator should be 5 (iterated 5 times)");

    // Verify the trace recorded multiple iterations
    let (_dir, events) = record_and_parse(&bytecode);
    let step_count = count_steps(&events);
    // 5 iterations of body (3 instructions each: ADDI, SUBI, JI) + check + exit
    // Plus initial setup (2 instructions) and final (2 instructions)
    // Roughly: 2 + 5*(3+1) + 1 + 2 = 25 instructions executed
    assert!(
        step_count >= 15,
        "loop should produce many step events (5 iterations), got {step_count}"
    );
}

/// Nested branches: if a > 10 { if b > 20 { result = 1 } else { result = 2 } } else { result = 3 }
#[test]
fn test_nested_branches() {
    let probe: Vec<u8> = vec![op::ret(RegId::ONE)].into_iter().collect();
    let probe_steps = run_and_collect_steps(probe);
    let base_pc = probe_steps[0].0;

    // Values: a=15, b=25 -> should reach result=1
    //
    // Layout:
    //   0:  MOVI r16, 15       -- a = 15
    //   1:  MOVI r17, 25       -- b = 25
    //   2:  MOVI r19, 10       -- threshold1
    //   3:  GT r18, r16, r19   -- r18 = (a > 10)
    //   4:  JNZI r18, <7>      -- if a > 10, goto inner_check (7)
    //   5:  MOVI r20, 3        -- else: result = 3
    //   6:  JI <12>            -- goto end
    //   7:  MOVI r19, 20       -- threshold2
    //   8:  GT r18, r17, r19   -- r18 = (b > 20)
    //   9:  JNZI r18, <11>     -- if b > 20, goto true_true
    //  10:  MOVI r20, 2        -- else (a>10, b<=20): result = 2
    //       JI <12>            -- goto end  (11)
    //  11:  MOVI r20, 1        -- true_true: result = 1  (12)
    //  12:  LOG ...            -- (13)
    //  13:  RET                -- (14)

    // Corrected: we need to be careful about instruction count
    let inner_check = ((base_pc / 4) + 7) as u32;
    let end_target = ((base_pc / 4) + 13) as u32;
    let true_true = ((base_pc / 4) + 12) as u32;

    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 15),           // 0: a = 15
        op::movi(0x11, 25),           // 1: b = 25
        op::movi(0x13, 10),           // 2: threshold1 = 10
        op::gt(0x12, 0x10, 0x13),     // 3: r18 = (15 > 10) = 1
        op::jnzi(0x12, inner_check),  // 4: jump to 7
        op::movi(0x14, 3),            // 5: result = 3 (skipped)
        op::ji(end_target),           // 6: goto end (skipped)
        op::movi(0x13, 20),           // 7: threshold2 = 20
        op::gt(0x12, 0x11, 0x13),     // 8: r18 = (25 > 20) = 1
        op::jnzi(0x12, true_true),    // 9: jump to 12
        op::movi(0x14, 2),            // 10: result = 2 (skipped)
        op::ji(end_target),           // 11: goto end (skipped)
        op::movi(0x14, 1),            // 12: result = 1
        op::log(0x14, 0x00, 0x00, 0x00), // 13
        op::ret(RegId::ONE),          // 14
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    assert_eq!(
        last.1[0x14], 1,
        "nested branch: a=15>10 and b=25>20 -> result should be 1"
    );

    let (_dir, events) = record_and_parse(&bytecode);
    let values = extract_int_values(&events);
    // The final result (1) should be in the trace
    assert!(
        values.contains(&1),
        "trace should contain nested branch result 1, values: {values:?}"
    );
}

/// Early return with RET instruction.
#[test]
fn test_early_return() {
    // The program returns immediately after setting r16 = 42.
    // Instructions after RET should not execute.
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 42),           // r16 = 42
        op::ret(RegId::ONE),          // return early
        op::movi(0x10, 999),          // should NOT execute
        op::movi(0x11, 888),          // should NOT execute
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    assert_eq!(last.1[0x10], 42, "early return: r16 should be 42, not overwritten");
    assert_eq!(last.1[0x11], 0, "early return: r17 should be 0 (instruction never executed)");

    // Verify only 2 steps recorded (MOVI + RET)
    let (_dir, events) = record_and_parse(&bytecode);
    let step_count = count_steps(&events);
    assert!(
        step_count <= 3,
        "early return should produce at most 3 step events, got {step_count}"
    );
}

// =========================================================================
// 7. Trace structure verification
// =========================================================================

/// Verify that the recorder produces Call and Return events for main.
#[test]
fn test_trace_call_return_structure() {
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 1),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let (_dir, events) = record_and_parse(&bytecode);

    let calls = count_calls(&events);
    let returns = count_returns(&events);

    assert!(calls >= 1, "should have at least 1 Call event for main, got {calls}");
    assert!(returns >= 1, "should have at least 1 Return event, got {returns}");
}

/// Verify that the trace has VariableName events for tracked registers.
#[test]
fn test_trace_variable_names() {
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 7),
        op::movi(0x11, 13),
        op::add(0x12, 0x10, 0x11),
        op::log(0x12, 0x00, 0x00, 0x00),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let (_dir, events) = record_and_parse(&bytecode);
    let names = extract_var_names(&events);

    // The variable tracker should name MOVIs as "imm_N" and ADDs as "X_plus_Y".
    // We require actual meaningful names, not raw register fallbacks like "r16".
    assert!(
        names.contains(&"imm_7".to_string()),
        "should have 'imm_7' for MOVI r16,7, got: {names:?}"
    );
    assert!(
        names.contains(&"imm_13".to_string()),
        "should have 'imm_13' for MOVI r17,13, got: {names:?}"
    );
    assert!(
        names.contains(&"imm_7_plus_imm_13".to_string()),
        "should have 'imm_7_plus_imm_13' for ADD r18,r16,r17, got: {names:?}"
    );
    // Verify that the tracker produces all three expected meaningful names,
    // not just one. Raw register names (r17, r18) may appear in early steps
    // before those registers are written by tracked instructions -- that is
    // expected since the recorder emits all registers r16-r23 on every step.
    assert_eq!(
        names.iter().filter(|n| *n == "imm_7" || *n == "imm_13" || *n == "imm_7_plus_imm_13").count(),
        3,
        "expected exactly 3 meaningful variable names (imm_7, imm_13, imm_7_plus_imm_13), got: {names:?}"
    );
}

/// Verify that trace output files are all created and non-empty.
#[test]
fn test_trace_output_completeness() {
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 1),
        op::movi(0x11, 2),
        op::add(0x12, 0x10, 0x11),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let (dir, _events) = record_and_parse(&bytecode);
    let out_dir = dir.path().join("traces");

    // Check all three files exist and are non-empty
    for filename in &["trace.bin", "trace_metadata.json", "trace_paths.json"] {
        let path = out_dir.join(filename);
        assert!(path.exists(), "{filename} should exist");
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size > 0, "{filename} should be non-empty");
    }

    // Verify metadata is valid JSON
    let metadata: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out_dir.join("trace_metadata.json")).unwrap(),
    )
    .expect("trace_metadata.json should be valid JSON");
    assert!(metadata.is_object() || metadata.is_array());

    // Verify paths is valid JSON
    let paths: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out_dir.join("trace_paths.json")).unwrap(),
    )
    .expect("trace_paths.json should be valid JSON");
    assert!(paths.is_object() || paths.is_array());
}

/// Verify that Step events have incrementing line numbers (for linear code).
#[test]
fn test_step_line_numbers_monotonic() {
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 1),
        op::movi(0x11, 2),
        op::movi(0x12, 3),
        op::movi(0x13, 4),
        op::movi(0x14, 5),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let (_dir, events) = record_and_parse(&bytecode);

    let step_lines: Vec<i64> = events
        .iter()
        .filter_map(|e| {
            e.get("Step")
                .and_then(|s| s.get("line"))
                .and_then(|l| l.as_i64())
        })
        .collect();

    assert!(!step_lines.is_empty(), "should have step events");

    // For linear code with synthetic source map, lines should be monotonically
    // increasing (each instruction maps to line i+1).
    for i in 1..step_lines.len() {
        assert!(
            step_lines[i] >= step_lines[i - 1],
            "step lines should be non-decreasing: {:?}",
            step_lines
        );
    }
}

// =========================================================================
// 8. Edge cases and stress tests
// =========================================================================

/// Test with many arithmetic operations to verify no stack overflow or
/// resource exhaustion in the recorder.
#[test]
fn test_many_operations() {
    let mut instructions: Vec<fuel_asm::Instruction> = Vec::new();

    // Initialize r16 = 1
    instructions.push(op::movi(0x10, 1));

    // Perform 50 ADDI operations: r16 += 1 each time
    for _ in 0..50 {
        instructions.push(op::addi(0x10, 0x10, 1));
    }

    instructions.push(op::log(0x10, 0x00, 0x00, 0x00));
    instructions.push(op::ret(RegId::ONE));

    let bytecode: Vec<u8> = instructions.into_iter().collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    assert_eq!(last.1[0x10], 51, "after 50 additions of 1 starting from 1, r16 = 51");

    let (_dir, events) = record_and_parse(&bytecode);
    let step_count = count_steps(&events);
    assert!(
        step_count >= 50,
        "should have at least 50 step events for 50+ instructions, got {step_count}"
    );
}

/// Test division by immediate produces correct result.
#[test]
fn test_division_and_modulo_edge_cases() {
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 100),          // r16 = 100
        op::divi(0x11, 0x10, 3),      // r17 = 100 / 3 = 33
        op::modi(0x12, 0x10, 3),      // r18 = 100 % 3 = 1
        // Verify: 33 * 3 + 1 = 100
        op::muli(0x13, 0x11, 3),      // r19 = 33 * 3 = 99
        op::add(0x14, 0x13, 0x12),    // r20 = 99 + 1 = 100
        op::log(0x11, 0x12, 0x14, 0x00),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    assert_eq!(last.1[0x11], 33, "100 / 3 = 33");
    assert_eq!(last.1[0x12], 1, "100 % 3 = 1");
    assert_eq!(last.1[0x14], 100, "quotient * divisor + remainder = original");
}

/// Test that NOOP instructions are handled correctly.
#[test]
fn test_noop_instructions() {
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 42),
        op::noop(),
        op::noop(),
        op::noop(),
        op::movi(0x11, 7),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    assert_eq!(last.1[0x10], 42, "MOVI before NOOPs should work");
    assert_eq!(last.1[0x11], 7, "MOVI after NOOPs should work");

    let (_dir, events) = record_and_parse(&bytecode);
    let step_count = count_steps(&events);
    assert!(step_count >= 5, "should step through NOOPs, got {step_count}");
}

/// Test EXP (exponentiation).
#[test]
fn test_exponentiation() {
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 2),            // r16 = 2
        op::movi(0x11, 10),           // r17 = 10
        op::exp(0x12, 0x10, 0x11),    // r18 = 2^10 = 1024
        op::log(0x12, 0x00, 0x00, 0x00),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    assert_eq!(last.1[0x12], 1024, "EXP: 2^10 = 1024");

    let (_dir, events) = record_and_parse(&bytecode);
    let values = extract_int_values(&events);
    assert!(values.contains(&1024), "trace should contain 2^10 = 1024");
}

/// Test bitwise operations: AND, OR, XOR, NOT, SLL, SRL.
#[test]
fn test_bitwise_operations() {
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 0xFF),         // r16 = 0xFF
        op::movi(0x11, 0x0F),         // r17 = 0x0F
        op::and(0x12, 0x10, 0x11),    // r18 = 0xFF & 0x0F = 0x0F
        op::or(0x13, 0x10, 0x11),     // r19 = 0xFF | 0x0F = 0xFF
        op::xor(0x14, 0x10, 0x11),    // r20 = 0xFF ^ 0x0F = 0xF0
        op::movi(0x15, 1),            // r21 = 1
        op::sll(0x16, 0x15, 0x11),    // r22 = 1 << 15 (but r17=0x0F=15)
        op::movi(0x17, 3),            // r23 shift amount
        op::srl(0x17, 0x10, 0x17),    // r23 = 0xFF >> 3 = 31
        op::log(0x12, 0x13, 0x14, 0x16),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    assert_eq!(last.1[0x12], 0x0F, "AND: 0xFF & 0x0F = 0x0F");
    assert_eq!(last.1[0x13], 0xFF, "OR: 0xFF | 0x0F = 0xFF");
    assert_eq!(last.1[0x14], 0xF0, "XOR: 0xFF ^ 0x0F = 0xF0");
    assert_eq!(last.1[0x16], 1 << 15, "SLL: 1 << 15 = 32768");
    assert_eq!(last.1[0x17], 0xFF >> 3, "SRL: 0xFF >> 3 = 31");
}

/// Test a fibonacci-like computation to exercise loops with
/// register-to-register operations.
#[test]
fn test_fibonacci_loop() {
    let probe: Vec<u8> = vec![op::ret(RegId::ONE)].into_iter().collect();
    let probe_steps = run_and_collect_steps(probe);
    let base_pc = probe_steps[0].0;

    // Compute fib(10) = 55
    // r16 = fib(n-2) = 0
    // r17 = fib(n-1) = 1
    // r18 = counter = 10
    // Loop: r19 = r16 + r17; r16 = r17; r17 = r19; counter -= 1
    //
    // Layout:
    //   0: MOVI r16, 0          -- fib(0)
    //   1: MOVI r17, 1          -- fib(1)
    //   2: MOVI r18, 10         -- counter
    //   3: JNZI r18, <5>        -- loop_check: if counter != 0, goto body
    //   4: JI <9>               -- exit loop
    //   5: ADD r19, r16, r17    -- body: next = a + b
    //   6: MOVE r16, r17        -- a = b
    //   7: MOVE r17, r19        -- b = next
    //   8: SUBI r18, r18, 1     -- counter -= 1
    //      JI <3>               -- (9) goto loop_check
    //   9: LOG r17, ...         -- (10) log result (fib(10))
    //  10: RET                  -- (11)

    let body_target = ((base_pc / 4) + 5) as u32;
    let exit_target = ((base_pc / 4) + 10) as u32;
    let loop_check = ((base_pc / 4) + 3) as u32;

    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 0),            // 0: a = 0
        op::movi(0x11, 1),            // 1: b = 1
        op::movi(0x12, 10),           // 2: counter = 10
        op::jnzi(0x12, body_target),  // 3: loop_check
        op::ji(exit_target),          // 4: exit
        op::add(0x13, 0x10, 0x11),    // 5: next = a + b
        op::move_(0x10, 0x11),        // 6: a = b
        op::move_(0x11, 0x13),        // 7: b = next
        op::subi(0x12, 0x12, 1),      // 8: counter--
        op::ji(loop_check),           // 9: back to check
        op::log(0x11, 0x10, 0x12, 0x00), // 10: log
        op::ret(RegId::ONE),          // 11
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];
    // 10 iterations starting from fib(0)=0, fib(1)=1 computes fib(11)=89
    assert_eq!(last.1[0x11], 89, "10 fibonacci iterations: result = 89");
    assert_eq!(last.1[0x12], 0, "counter should be 0 after loop");

    let (_dir, events) = record_and_parse(&bytecode);
    let values = extract_int_values(&events);
    assert!(values.contains(&89), "trace should contain fibonacci result 89");
}

/// Verify that the recorder correctly handles multiple LOG instructions.
#[test]
fn test_multiple_logs() {
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 1),
        op::log(0x10, 0x00, 0x00, 0x00),
        op::movi(0x10, 2),
        op::log(0x10, 0x00, 0x00, 0x00),
        op::movi(0x10, 3),
        op::log(0x10, 0x00, 0x00, 0x00),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let interp = FuelInterpreter::new(bytecode.clone()).expect("interp");
    let mut log_count = 0;
    interp
        .run_with_callback(|step: &StepState| {
            for receipt in &step.receipts {
                if matches!(receipt, fuel_tx::Receipt::Log { .. }) {
                    log_count += 1;
                }
            }
        })
        .unwrap();

    // We count accumulated receipts at each step, so the total count of
    // Log receipts seen across all steps is >= 3 (they accumulate).
    // The important thing is that all 3 LOG instructions produced receipts.
    assert!(log_count >= 3, "should see at least 3 Log receipts (accumulated), got {log_count}");
}

/// Test stack frame operations: CFEI (extend) and CFSI (shrink).
#[test]
fn test_stack_frame_operations() {
    let bytecode: Vec<u8> = vec![
        op::move_(0x10, RegId::SP),    // r16 = SP (before extend)
        op::cfei(64),                  // extend stack by 64 bytes
        op::move_(0x11, RegId::SP),    // r17 = SP (after extend)
        op::cfsi(64),                  // shrink stack by 64 bytes
        op::move_(0x12, RegId::SP),    // r18 = SP (after shrink, should equal r16)
        op::log(0x10, 0x11, 0x12, 0x00),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let steps = run_and_collect_steps(bytecode.clone());
    let last = &steps[steps.len() - 1];

    let sp_before = last.1[0x10];
    let sp_after_extend = last.1[0x11];
    let sp_after_shrink = last.1[0x12];

    assert_eq!(
        sp_after_extend,
        sp_before + 64,
        "CFEI should extend stack pointer by 64"
    );
    assert_eq!(
        sp_after_shrink, sp_before,
        "CFSI should restore stack pointer"
    );
}

/// Test interaction between multiple register groups.
/// Verify that registers r16-r23 (the tracked range) are all captured.
#[test]
fn test_all_tracked_registers() {
    let bytecode: Vec<u8> = vec![
        op::movi(0x10, 16),           // r16 = 16
        op::movi(0x11, 17),           // r17 = 17
        op::movi(0x12, 18),           // r18 = 18
        op::movi(0x13, 19),           // r19 = 19
        op::movi(0x14, 20),           // r20 = 20
        op::movi(0x15, 21),           // r21 = 21
        op::movi(0x16, 22),           // r22 = 22
        op::movi(0x17, 23),           // r23 = 23
        op::log(0x10, 0x11, 0x12, 0x13),
        op::ret(RegId::ONE),
    ]
    .into_iter()
    .collect();

    let (_dir, events) = record_and_parse(&bytecode);
    let values = extract_int_values(&events);

    for val in 16..=23 {
        assert!(
            values.contains(&(val as i64)),
            "trace should contain register value {val}, got: {values:?}"
        );
    }
}
