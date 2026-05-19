//! Tests for M5: Contract Call Debugging.
//!
//! Tests cover:
//! 1. Single-contract execution (no context switch)
//! 2. Cross-contract call detection via Receipt::Call
//! 3. Nested cross-contract calls (A -> B -> C -> B -> A)
//! 4. Predicate execution context handling
//! 5. Per-contract source map resolution
//! 6. GraphQL debug API query/response types

use std::path::{Path, PathBuf};

use fuel_tx::Receipt;
use fuel_types::{AssetId, ContractId};

use codetracer_fuel_recorder::contract_call::{
    ContractCallTracker, ContractSwitch, ExecutionContext,
};
use codetracer_fuel_recorder::graphql_debug::{
    self, DebugContinueResult, GraphQLDebugConfig, GraphQLResponse, SessionId,
};
use codetracer_fuel_recorder::source_map::SwaySourceMap;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_contract_id(byte: u8) -> ContractId {
    ContractId::new([byte; 32])
}

fn make_call_receipt(from: ContractId, to: ContractId) -> Receipt {
    Receipt::call(
        from,
        to,
        0,                 // amount
        AssetId::zeroed(), // asset_id
        1_000_000,         // gas
        0,                 // param1
        0,                 // param2
        0,                 // pc
        0,                 // is
    )
}

fn make_return_receipt(id: ContractId) -> Receipt {
    Receipt::ret(id, 0, 0, 0)
}

fn make_return_data_receipt(id: ContractId) -> Receipt {
    Receipt::return_data(id, 0, 0, 0, vec![])
}

// ---------------------------------------------------------------------------
// Test 1: Single contract, no switch
// ---------------------------------------------------------------------------

#[test]
fn test_single_contract_no_switch() {
    let mut tracker = ContractCallTracker::new();

    // Verify initial state
    assert_eq!(tracker.call_depth(), 1, "should start at depth 1 (script)");
    assert_eq!(
        tracker.current_execution_context(),
        ExecutionContext::Script
    );
    assert_eq!(
        tracker.current_contract_id(),
        &ContractId::zeroed(),
        "script context has zeroed contract ID"
    );

    // Process empty receipts -- no switches
    let switches = tracker.process_receipts(&[]);
    assert!(
        switches.is_empty(),
        "no receipts should produce no switches"
    );
    assert_eq!(tracker.call_depth(), 1, "depth should remain 1");

    // Process non-call/return receipts -- still no switches
    let log_receipt = Receipt::log(ContractId::zeroed(), 42, 0, 0, 0, 0, 0);
    let switches = tracker.process_receipts(&[log_receipt]);
    assert!(
        switches.is_empty(),
        "Log receipt should not produce a contract switch"
    );
    assert_eq!(tracker.call_depth(), 1);
}

// ---------------------------------------------------------------------------
// Test 2: Cross-contract call detection
// ---------------------------------------------------------------------------

#[test]
fn test_cross_contract_call() {
    let mut tracker = ContractCallTracker::new();
    let contract_a = make_contract_id(0xAA);

    // Script calls contract A
    let receipts = vec![make_call_receipt(ContractId::zeroed(), contract_a)];
    let switches = tracker.process_receipts(&receipts);

    // Should detect one Enter switch
    assert_eq!(switches.len(), 1, "should have exactly one switch");
    match &switches[0] {
        ContractSwitch::Enter { from, to, depth } => {
            assert_eq!(from, &ContractId::zeroed(), "call from script (zeroed ID)");
            assert_eq!(to, &contract_a, "call to contract A");
            assert_eq!(*depth, 2, "depth after entering should be 2");
        }
        other => panic!("expected Enter, got {:?}", other),
    }

    // Verify tracker state
    assert_eq!(tracker.call_depth(), 2);
    assert_eq!(tracker.current_contract_id(), &contract_a);
    assert_eq!(
        tracker.current_execution_context(),
        ExecutionContext::Contract
    );

    // Contract A returns
    let mut receipts2 = receipts;
    receipts2.push(make_return_receipt(contract_a));
    let switches2 = tracker.process_receipts(&receipts2);

    assert_eq!(switches2.len(), 1, "should have one Exit switch");
    match &switches2[0] {
        ContractSwitch::Exit { from, to, depth } => {
            assert_eq!(from, &contract_a, "returning from contract A");
            assert_eq!(to, &ContractId::zeroed(), "returning to script");
            assert_eq!(*depth, 1, "depth after return should be 1");
        }
        other => panic!("expected Exit, got {:?}", other),
    }

    assert_eq!(tracker.call_depth(), 1);
    assert_eq!(tracker.current_contract_id(), &ContractId::zeroed());
}

// ---------------------------------------------------------------------------
// Test 3: Nested cross-contract calls (A -> B -> C -> B -> A)
// ---------------------------------------------------------------------------

#[test]
fn test_nested_cross_contract() {
    let mut tracker = ContractCallTracker::new();
    let contract_a = make_contract_id(0xAA);
    let contract_b = make_contract_id(0xBB);
    let contract_c = make_contract_id(0xCC);

    let mut receipts: Vec<Receipt> = Vec::new();

    // Script -> A
    receipts.push(make_call_receipt(ContractId::zeroed(), contract_a));
    let switches = tracker.process_receipts(&receipts);
    assert_eq!(switches.len(), 1);
    assert_eq!(tracker.call_depth(), 2);
    assert_eq!(tracker.current_contract_id(), &contract_a);

    // A -> B
    receipts.push(make_call_receipt(contract_a, contract_b));
    let switches = tracker.process_receipts(&receipts);
    assert_eq!(switches.len(), 1);
    assert_eq!(tracker.call_depth(), 3);
    assert_eq!(tracker.current_contract_id(), &contract_b);

    // B -> C
    receipts.push(make_call_receipt(contract_b, contract_c));
    let switches = tracker.process_receipts(&receipts);
    assert_eq!(switches.len(), 1);
    assert_eq!(tracker.call_depth(), 4);
    assert_eq!(tracker.current_contract_id(), &contract_c);

    // Verify full call stack: [Script, A, B, C]
    let stack = tracker.call_stack();
    assert_eq!(stack.len(), 4);
    assert_eq!(stack[0].context, ExecutionContext::Script);
    assert_eq!(stack[1].contract_id, contract_a);
    assert_eq!(stack[2].contract_id, contract_b);
    assert_eq!(stack[3].contract_id, contract_c);

    // C returns -> B
    receipts.push(make_return_receipt(contract_c));
    let switches = tracker.process_receipts(&receipts);
    assert_eq!(switches.len(), 1);
    assert_eq!(tracker.call_depth(), 3);
    assert_eq!(tracker.current_contract_id(), &contract_b);

    // B returns -> A
    receipts.push(make_return_receipt(contract_b));
    let switches = tracker.process_receipts(&receipts);
    assert_eq!(switches.len(), 1);
    assert_eq!(tracker.call_depth(), 2);
    assert_eq!(tracker.current_contract_id(), &contract_a);

    // A returns -> Script
    receipts.push(make_return_receipt(contract_a));
    let switches = tracker.process_receipts(&receipts);
    assert_eq!(switches.len(), 1);
    assert_eq!(tracker.call_depth(), 1);
    assert_eq!(tracker.current_contract_id(), &ContractId::zeroed());
    assert_eq!(
        tracker.current_execution_context(),
        ExecutionContext::Script
    );
}

// ---------------------------------------------------------------------------
// Test 4: Predicate execution context
// ---------------------------------------------------------------------------

#[test]
fn test_predicate_context() {
    let mut tracker = ContractCallTracker::new();

    // Initially not in predicate
    assert!(!tracker.is_predicate());
    assert_eq!(
        tracker.current_execution_context(),
        ExecutionContext::Script
    );

    // Enter predicate mode
    tracker.enter_predicate();
    assert!(tracker.is_predicate());
    assert_eq!(
        tracker.current_execution_context(),
        ExecutionContext::Predicate
    );
    assert_eq!(
        tracker.call_depth(),
        2,
        "predicate pushes a context on the stack"
    );

    // Predicate context should have zeroed contract ID
    assert_eq!(tracker.current_contract_id(), &ContractId::zeroed());

    // Exit predicate mode
    tracker.exit_predicate();
    assert!(!tracker.is_predicate());
    assert_eq!(
        tracker.current_execution_context(),
        ExecutionContext::Script
    );
    assert_eq!(tracker.call_depth(), 1);

    // Can enter and exit predicate multiple times
    tracker.enter_predicate();
    assert!(tracker.is_predicate());
    tracker.exit_predicate();
    assert!(!tracker.is_predicate());
}

// ---------------------------------------------------------------------------
// Test 5: Source map per contract
// ---------------------------------------------------------------------------

#[test]
fn test_source_map_per_contract() {
    let mut tracker = ContractCallTracker::new();
    let contract_a = make_contract_id(0xAA);
    let contract_b = make_contract_id(0xBB);

    // Register source maps for two contracts
    let entries_a = vec![
        (0, PathBuf::from("contract_a/main.sw"), 10),
        (1, PathBuf::from("contract_a/main.sw"), 20),
        (2, PathBuf::from("contract_a/main.sw"), 30),
    ];
    tracker.register_source_map(
        contract_a,
        SwaySourceMap::from_line_mapping(entries_a),
        PathBuf::from("contract_a/main.sw"),
    );

    let entries_b = vec![
        (0, PathBuf::from("contract_b/lib.sw"), 100),
        (1, PathBuf::from("contract_b/lib.sw"), 200),
    ];
    tracker.register_source_map(
        contract_b,
        SwaySourceMap::from_line_mapping(entries_b),
        PathBuf::from("contract_b/lib.sw"),
    );

    // Default source map for the script
    let default_entries = vec![
        (0, PathBuf::from("script.sw"), 1),
        (1, PathBuf::from("script.sw"), 2),
    ];
    let default_map = SwaySourceMap::from_line_mapping(default_entries);
    let default_path = Path::new("script.sw");

    // In script context, should use default source map
    let (path, line) = tracker.lookup_source(0, &default_map, default_path);
    assert_eq!(path, Path::new("script.sw"));
    assert_eq!(line, 1);

    // Enter contract A
    let mut receipts = vec![make_call_receipt(ContractId::zeroed(), contract_a)];
    tracker.process_receipts(&receipts);

    // Should use contract A's source map
    let (path, line) = tracker.lookup_source(0, &default_map, default_path);
    assert_eq!(path, Path::new("contract_a/main.sw"));
    assert_eq!(line, 10);

    let (path, line) = tracker.lookup_source(1, &default_map, default_path);
    assert_eq!(path, Path::new("contract_a/main.sw"));
    assert_eq!(line, 20);

    // Verify current_source_path for contract A
    let src_path = tracker.current_source_path(default_path);
    assert_eq!(src_path, Path::new("contract_a/main.sw"));

    // Contract A calls contract B
    receipts.push(make_call_receipt(contract_a, contract_b));
    tracker.process_receipts(&receipts);

    // Should use contract B's source map
    let (path, line) = tracker.lookup_source(0, &default_map, default_path);
    assert_eq!(path, Path::new("contract_b/lib.sw"));
    assert_eq!(line, 100);

    let (path, line) = tracker.lookup_source(1, &default_map, default_path);
    assert_eq!(path, Path::new("contract_b/lib.sw"));
    assert_eq!(line, 200);

    // Return from B back to A
    receipts.push(make_return_receipt(contract_b));
    tracker.process_receipts(&receipts);

    // Should be back to contract A's source map
    let (path, line) = tracker.lookup_source(0, &default_map, default_path);
    assert_eq!(path, Path::new("contract_a/main.sw"));
    assert_eq!(line, 10);

    // Return from A back to script
    receipts.push(make_return_receipt(contract_a));
    tracker.process_receipts(&receipts);

    // Should be back to default source map
    let (path, line) = tracker.lookup_source(0, &default_map, default_path);
    assert_eq!(path, Path::new("script.sw"));
    assert_eq!(line, 1);
}

// ---------------------------------------------------------------------------
// Test 6: GraphQL debug API types
// ---------------------------------------------------------------------------

#[test]
fn test_graphql_debug_api_types() {
    // Config construction
    let config = GraphQLDebugConfig::local();
    assert!(config.endpoint.contains("localhost"));
    assert!(config.endpoint.contains("4000"));

    let custom_config =
        GraphQLDebugConfig::with_endpoint("https://mainnet.fuel.network/v1/graphql");
    assert_eq!(
        custom_config.endpoint,
        "https://mainnet.fuel.network/v1/graphql"
    );

    // Query construction
    let session = SessionId("test-session".to_string());

    let start_req = graphql_debug::start_session_query();
    assert!(start_req.query.contains("startSession"));

    let end_req = graphql_debug::end_session_query(&session);
    assert!(end_req.query.contains("endSession"));
    assert!(end_req.query.contains("test-session"));

    let step_req = graphql_debug::set_single_stepping_query(&session, true);
    assert!(step_req.query.contains("setSingleStepping"));
    assert!(step_req.query.contains("true"));

    let continue_req = graphql_debug::continue_tx_query(&session);
    assert!(continue_req.query.contains("continueTx"));

    let reg_req = graphql_debug::register_query(&session, 16);
    assert!(reg_req.query.contains("register"));

    let mem_req = graphql_debug::memory_query(&session, 0x1000, 64);
    assert!(mem_req.query.contains("memory"));

    // Response parsing -- startSession
    let start_resp = GraphQLResponse {
        data: Some(serde_json::json!({
            "startSession": { "id": "sess-abc" }
        })),
        errors: None,
    };
    let parsed_session = graphql_debug::parse_start_session_response(&start_resp).unwrap();
    assert_eq!(parsed_session.0, "sess-abc");

    // Response parsing -- continueTx breakpoint
    let bp_resp = GraphQLResponse {
        data: Some(serde_json::json!({
            "continueTx": { "state": "BREAKPOINT:128" }
        })),
        errors: None,
    };
    let continue_result = graphql_debug::parse_continue_response(&bp_resp).unwrap();
    assert_eq!(continue_result, DebugContinueResult::Breakpoint(128));

    // Response parsing -- continueTx completed
    let done_resp = GraphQLResponse {
        data: Some(serde_json::json!({
            "continueTx": { "state": "COMPLETED" }
        })),
        errors: None,
    };
    let done_result = graphql_debug::parse_continue_response(&done_resp).unwrap();
    assert_eq!(done_result, DebugContinueResult::Completed);

    // Response parsing -- register
    let reg_resp = GraphQLResponse {
        data: Some(serde_json::json!({ "register": "0xff" })),
        errors: None,
    };
    let reg_val = graphql_debug::parse_register_response(&reg_resp).unwrap();
    assert_eq!(reg_val, 255);

    // Response parsing -- memory
    let mem_resp = GraphQLResponse {
        data: Some(serde_json::json!({ "memory": "0xcafebabe" })),
        errors: None,
    };
    let mem_bytes = graphql_debug::parse_memory_response(&mem_resp).unwrap();
    assert_eq!(mem_bytes, vec![0xca, 0xfe, 0xba, 0xbe]);

    // Error response
    let err_resp = GraphQLResponse {
        data: None,
        errors: Some(vec![graphql_debug::GraphQLError {
            message: "session expired".to_string(),
            locations: None,
            path: None,
        }]),
    };
    assert!(graphql_debug::parse_start_session_response(&err_resp).is_none());
    assert_eq!(
        format!("{}", err_resp.errors.as_ref().unwrap()[0]),
        "session expired"
    );
}

// ---------------------------------------------------------------------------
// Additional edge-case tests
// ---------------------------------------------------------------------------

#[test]
fn test_return_data_receipt_pops_stack() {
    // Verify that ReturnData receipts also pop the call stack
    let mut tracker = ContractCallTracker::new();
    let contract_a = make_contract_id(0xAA);

    let mut receipts = vec![make_call_receipt(ContractId::zeroed(), contract_a)];
    tracker.process_receipts(&receipts);
    assert_eq!(tracker.call_depth(), 2);

    // Return with ReturnData instead of Return
    receipts.push(make_return_data_receipt(contract_a));
    let switches = tracker.process_receipts(&receipts);

    assert_eq!(switches.len(), 1);
    match &switches[0] {
        ContractSwitch::Exit { from, .. } => {
            assert_eq!(from, &contract_a);
        }
        other => panic!("expected Exit, got {:?}", other),
    }
    assert_eq!(tracker.call_depth(), 1);
}

#[test]
fn test_multiple_calls_same_contract() {
    // A contract can be called multiple times
    let mut tracker = ContractCallTracker::new();
    let contract_a = make_contract_id(0xAA);

    let mut receipts: Vec<Receipt> = Vec::new();

    // First call to A
    receipts.push(make_call_receipt(ContractId::zeroed(), contract_a));
    tracker.process_receipts(&receipts);
    assert_eq!(tracker.call_depth(), 2);

    // Return from A
    receipts.push(make_return_receipt(contract_a));
    tracker.process_receipts(&receipts);
    assert_eq!(tracker.call_depth(), 1);

    // Second call to A
    receipts.push(make_call_receipt(ContractId::zeroed(), contract_a));
    tracker.process_receipts(&receipts);
    assert_eq!(tracker.call_depth(), 2);
    assert_eq!(tracker.current_contract_id(), &contract_a);

    // Return from A again
    receipts.push(make_return_receipt(contract_a));
    tracker.process_receipts(&receipts);
    assert_eq!(tracker.call_depth(), 1);
}

#[test]
fn test_source_map_fallback_for_unknown_contract() {
    // When a contract has no registered source map, fall back to default
    let mut tracker = ContractCallTracker::new();
    let unknown_contract = make_contract_id(0xFF);

    let default_entries = vec![(0, PathBuf::from("default.sw"), 5)];
    let default_map = SwaySourceMap::from_line_mapping(default_entries);
    let default_path = Path::new("default.sw");

    // Enter unknown contract (no source map registered)
    let receipts = vec![make_call_receipt(ContractId::zeroed(), unknown_contract)];
    tracker.process_receipts(&receipts);

    // Should fall back to default source map
    let (path, line) = tracker.lookup_source(0, &default_map, default_path);
    assert_eq!(path, Path::new("default.sw"));
    assert_eq!(line, 5);

    // For unmapped opcode index, should use default path with index+1 as line
    let (path, line) = tracker.lookup_source(99, &default_map, default_path);
    assert_eq!(path, Path::new("default.sw"));
    assert_eq!(line, 100); // 99 + 1
}
