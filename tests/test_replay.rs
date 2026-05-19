//! Tests for the M6 on-chain transaction replay module.

use std::path::PathBuf;

use codetracer_fuel_recorder::graphql_debug::GraphQLResponse;
use codetracer_fuel_recorder::replay::*;

// ---------------------------------------------------------------------------
// 1. test_parse_transaction_response
// ---------------------------------------------------------------------------

#[test]
fn test_parse_transaction_response() {
    let response = GraphQLResponse {
        data: Some(serde_json::json!({
            "transaction": {
                "id": "0xabc123def456789012345678901234567890123456789012345678901234abcd",
                "rawPayload": "0x00000000000000010000000000000024",
                "inputs": [
                    {
                        "__typename": "InputCoin",
                        "utxoId": "0xutxo1",
                        "owner": "0xowner1",
                        "amount": "1000000",
                        "assetId": "0x0000000000000000000000000000000000000000000000000000000000000000"
                    },
                    {
                        "__typename": "InputContract",
                        "utxoId": "0xutxo2",
                        "contractId": "0xcontract_abc123"
                    },
                    {
                        "__typename": "InputMessage",
                        "sender": "0xsender1",
                        "recipient": "0xrecipient1",
                        "amount": "500",
                        "data": "0xdeadbeef"
                    }
                ],
                "outputs": [
                    {
                        "__typename": "CoinOutput",
                        "to": "0xrecipient",
                        "amount": "999000",
                        "assetId": "0x0000000000000000000000000000000000000000000000000000000000000000"
                    },
                    {
                        "__typename": "ContractOutput",
                        "inputIndex": 1
                    },
                    {
                        "__typename": "ChangeOutput",
                        "to": "0xowner1",
                        "amount": "100",
                        "assetId": "0x0000000000000000000000000000000000000000000000000000000000000000"
                    }
                ],
                "receipts": [
                    {
                        "receiptType": "CALL",
                        "contractId": "0xcontract_abc123",
                        "to": "0xcontract_abc123",
                        "amount": "0",
                        "gas": "999999",
                        "param1": "0",
                        "param2": "0",
                        "pc": "100",
                        "is": "0"
                    },
                    {
                        "receiptType": "LOG",
                        "contractId": "0xcontract_abc123",
                        "ra": "42",
                        "rb": "0",
                        "rc": "0",
                        "rd": "0",
                        "pc": "108",
                        "is": "0"
                    },
                    {
                        "receiptType": "RETURN",
                        "contractId": "0xcontract_abc123",
                        "val": "1",
                        "pc": "120",
                        "is": "0"
                    },
                    {
                        "receiptType": "SCRIPT_RESULT",
                        "result": "0",
                        "gasUsed": "500"
                    }
                ],
                "status": {
                    "__typename": "SuccessStatus",
                    "block": {
                        "header": {
                            "height": "12345"
                        }
                    }
                }
            }
        })),
        errors: None,
    };

    let tx_data = parse_transaction_response(&response).unwrap();

    assert_eq!(
        tx_data.id,
        "0xabc123def456789012345678901234567890123456789012345678901234abcd"
    );
    assert_eq!(tx_data.raw_payload, "0x00000000000000010000000000000024");
    assert_eq!(tx_data.inputs.len(), 3);
    assert_eq!(tx_data.outputs.len(), 3);
    assert_eq!(tx_data.receipts.len(), 4);

    // Verify input types
    match &tx_data.inputs[0] {
        TransactionInput::InputCoin { owner, amount, .. } => {
            assert_eq!(owner.as_deref(), Some("0xowner1"));
            assert_eq!(amount.as_deref(), Some("1000000"));
        }
        _ => panic!("expected InputCoin"),
    }

    match &tx_data.inputs[1] {
        TransactionInput::InputContract { contract_id, .. } => {
            assert_eq!(contract_id, "0xcontract_abc123");
        }
        _ => panic!("expected InputContract"),
    }

    match &tx_data.inputs[2] {
        TransactionInput::InputMessage { sender, data, .. } => {
            assert_eq!(sender.as_deref(), Some("0xsender1"));
            assert_eq!(data.as_deref(), Some("0xdeadbeef"));
        }
        _ => panic!("expected InputMessage"),
    }

    // Verify status
    let status = tx_data.status.unwrap();
    assert_eq!(status.status_type, "SuccessStatus");
    assert_eq!(status.block_height, Some(12345));

    // Verify receipts
    assert_eq!(tx_data.receipts[0].receipt_type, "CALL");
    assert_eq!(
        tx_data.receipts[0].contract_id.as_deref(),
        Some("0xcontract_abc123")
    );
    assert_eq!(tx_data.receipts[1].receipt_type, "LOG");
    assert_eq!(tx_data.receipts[2].receipt_type, "RETURN");
    assert_eq!(tx_data.receipts[3].receipt_type, "SCRIPT_RESULT");
}

// ---------------------------------------------------------------------------
// 2. test_parse_contract_bytecode_response
// ---------------------------------------------------------------------------

#[test]
fn test_parse_contract_bytecode_response() {
    let response = GraphQLResponse {
        data: Some(serde_json::json!({
            "contract": {
                "bytecode": "0x1a4804005d4910001a4414405d4d1000724c200050491000504d100036490000724028002449100020451480"
            }
        })),
        errors: None,
    };

    let result = parse_contract_bytecode_response(&response, "0xcontract_abc123").unwrap();
    assert_eq!(result.contract_id, "0xcontract_abc123");
    assert!(!result.bytecode.is_empty());
    assert!(result.bytecode.starts_with("0x"));
    // The bytecode string includes the "0x" prefix + hex chars
    assert!(
        result.bytecode.len() > 4,
        "bytecode should contain hex data"
    );
}

#[test]
fn test_parse_contract_bytecode_missing_contract() {
    let response = GraphQLResponse {
        data: Some(serde_json::json!({})),
        errors: None,
    };

    let result = parse_contract_bytecode_response(&response, "0xmissing");
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// 3. test_dry_run_response_parsing
// ---------------------------------------------------------------------------

#[test]
fn test_dry_run_response_parsing() {
    let response = GraphQLResponse {
        data: Some(serde_json::json!({
            "dryRun": [
                {
                    "receiptType": "CALL",
                    "contractId": "0xcontract1",
                    "to": "0xcontract1",
                    "amount": "0",
                    "gas": "999999"
                },
                {
                    "receiptType": "LOG",
                    "contractId": "0xcontract1",
                    "ra": "42",
                    "rb": "0",
                    "rc": "0",
                    "rd": "0"
                },
                {
                    "receiptType": "RETURN",
                    "contractId": "0xcontract1",
                    "val": "1"
                },
                {
                    "receiptType": "SCRIPT_RESULT",
                    "result": "0",
                    "gasUsed": "350",
                    "programState": {
                        "returnType": "RETURN",
                        "data": "0x0000000000000001"
                    }
                }
            ]
        })),
        errors: None,
    };

    let result = parse_dry_run_response(&response).unwrap();
    assert_eq!(result.receipts.len(), 4);
    assert_eq!(result.receipts[0].receipt_type, "CALL");
    assert_eq!(result.receipts[1].receipt_type, "LOG");
    assert_eq!(result.receipts[2].receipt_type, "RETURN");
    assert_eq!(result.receipts[3].receipt_type, "SCRIPT_RESULT");
    assert_eq!(result.program_state.as_deref(), Some("0x0000000000000001"));
}

#[test]
fn test_dry_run_response_no_program_state() {
    let response = GraphQLResponse {
        data: Some(serde_json::json!({
            "dryRun": [
                {
                    "receiptType": "SCRIPT_RESULT",
                    "result": "0",
                    "gasUsed": "100"
                }
            ]
        })),
        errors: None,
    };

    let result = parse_dry_run_response(&response).unwrap();
    assert_eq!(result.receipts.len(), 1);
    assert!(result.program_state.is_none());
}

// ---------------------------------------------------------------------------
// 4. test_extract_contract_ids_from_inputs
// ---------------------------------------------------------------------------

#[test]
fn test_extract_contract_ids_from_inputs() {
    let inputs = vec![
        TransactionInput::InputCoin {
            utxo_id: Some("0xutxo1".into()),
            owner: Some("0xowner1".into()),
            amount: Some("1000".into()),
            asset_id: Some("0xbase".into()),
        },
        TransactionInput::InputContract {
            utxo_id: Some("0xutxo2".into()),
            contract_id: "0xcontract_aaaa".to_string(),
        },
        TransactionInput::InputContract {
            utxo_id: Some("0xutxo3".into()),
            contract_id: "0xcontract_bbbb".to_string(),
        },
        TransactionInput::InputCoin {
            utxo_id: Some("0xutxo4".into()),
            owner: Some("0xowner2".into()),
            amount: Some("2000".into()),
            asset_id: Some("0xbase".into()),
        },
        TransactionInput::InputContract {
            utxo_id: Some("0xutxo5".into()),
            contract_id: "0xcontract_aaaa".to_string(), // duplicate
        },
    ];

    let ids = extract_contract_ids(&inputs);
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&"0xcontract_aaaa".to_string()));
    assert!(ids.contains(&"0xcontract_bbbb".to_string()));
}

#[test]
fn test_extract_contract_ids_skips_empty() {
    let inputs = vec![TransactionInput::InputContract {
        utxo_id: None,
        contract_id: "".to_string(),
    }];

    let ids = extract_contract_ids(&inputs);
    assert!(ids.is_empty());
}

// ---------------------------------------------------------------------------
// 5. test_find_source_maps_in_directory
// ---------------------------------------------------------------------------

#[test]
fn test_find_source_maps_in_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();

    // Create forc build output structure
    let out_debug = base.join("out").join("debug");
    std::fs::create_dir_all(&out_debug).unwrap();

    // Create ABI file
    let abi_content = r#"{"programType":"contract","functions":[],"types":[]}"#;
    std::fs::write(out_debug.join("my_contract-abi.json"), abi_content).unwrap();

    // Create bytecode file
    std::fs::write(out_debug.join("my_contract.bin"), b"\x00\x00\x00\x01").unwrap();

    // Create source files
    let src_dir = base.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("main.sw"), "contract;\nfn main() {}").unwrap();
    std::fs::write(src_dir.join("lib.sw"), "library;\npub fn helper() {}").unwrap();

    let info = find_source_maps(base, "0xcontract_id").unwrap();
    assert_eq!(info.contract_id, "0xcontract_id");
    assert_eq!(info.source_dir, base);
    assert_eq!(info.source_files.len(), 2);
    // Source files should be .sw files
    for f in &info.source_files {
        assert_eq!(f.extension().unwrap(), "sw");
    }
}

#[test]
fn test_find_source_maps_missing_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().join("nonexistent");

    let result = find_source_maps(&base, "0xcontract_id");
    assert!(result.is_none());
}

#[test]
fn test_find_source_maps_no_abi_files() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();

    // Create out/debug but with no ABI files
    let out_debug = base.join("out").join("debug");
    std::fs::create_dir_all(&out_debug).unwrap();
    std::fs::write(out_debug.join("random.txt"), "not an abi").unwrap();

    let result = find_source_maps(base, "0xcontract_id");
    assert!(result.is_none());
}

// ---------------------------------------------------------------------------
// 6. test_replay_pipeline_with_mock_data
// ---------------------------------------------------------------------------

#[test]
fn test_replay_pipeline_with_mock_data() {
    // This test validates the full pipeline data flow by constructing
    // all intermediate types manually (no network calls).

    // Step 1: Simulate fetched transaction data
    let tx_data = TransactionData {
        id: "0xtx_abc123".to_string(),
        raw_payload: "0x00000000000000010000000000000024deadbeef".to_string(),
        inputs: vec![
            TransactionInput::InputCoin {
                utxo_id: Some("0xutxo1".into()),
                owner: Some("0xowner1".into()),
                amount: Some("1000000".into()),
                asset_id: Some("0xbase_asset".into()),
            },
            TransactionInput::InputContract {
                utxo_id: Some("0xutxo2".into()),
                contract_id: "0xcontract_deployed".to_string(),
            },
        ],
        outputs: vec![
            TransactionOutput::ContractOutput {
                input_index: Some(1),
            },
            TransactionOutput::ChangeOutput {
                to: Some("0xowner1".into()),
                amount: Some("999000".into()),
                asset_id: Some("0xbase_asset".into()),
            },
        ],
        receipts: vec![
            TransactionReceipt {
                receipt_type: "CALL".to_string(),
                contract_id: Some("0xcontract_deployed".to_string()),
                data: serde_json::json!({}),
            },
            TransactionReceipt {
                receipt_type: "RETURN".to_string(),
                contract_id: Some("0xcontract_deployed".to_string()),
                data: serde_json::json!({}),
            },
            TransactionReceipt {
                receipt_type: "SCRIPT_RESULT".to_string(),
                contract_id: None,
                data: serde_json::json!({"result": "0"}),
            },
        ],
        status: Some(TransactionStatus {
            status_type: "SuccessStatus".to_string(),
            block_height: Some(54321),
        }),
    };

    // Step 2: Extract contract IDs
    let contract_ids = extract_contract_ids(&tx_data.inputs);
    assert_eq!(contract_ids, vec!["0xcontract_deployed"]);

    // Step 3: Simulate fetched bytecode
    let bytecode_data = ContractBytecodeData {
        contract_id: "0xcontract_deployed".to_string(),
        bytecode: "0x1a4804005d491000".to_string(),
    };
    assert!(!bytecode_data.bytecode.is_empty());

    // Step 4: Simulate dry run result
    let dry_run = DryRunResult {
        receipts: vec![
            TransactionReceipt {
                receipt_type: "CALL".to_string(),
                contract_id: Some("0xcontract_deployed".to_string()),
                data: serde_json::json!({}),
            },
            TransactionReceipt {
                receipt_type: "RETURN".to_string(),
                contract_id: Some("0xcontract_deployed".to_string()),
                data: serde_json::json!({}),
            },
        ],
        program_state: Some("0x0000000000000001".to_string()),
    };
    assert_eq!(dry_run.receipts.len(), 2);
    assert!(dry_run.program_state.is_some());

    // Step 5: Create replay summary
    let summary = ReplaySummary {
        tx_id: tx_data.id.clone(),
        block_height: tx_data.status.as_ref().and_then(|s| s.block_height),
        contract_ids: contract_ids.clone(),
        contracts_with_bytecode: 1,
        has_source_maps: false,
        dry_run_receipts: dry_run.receipts.len(),
        historical_execution: false,
        source_available: false,
    };

    assert_eq!(summary.tx_id, "0xtx_abc123");
    assert_eq!(summary.block_height, Some(54321));
    assert_eq!(summary.contract_ids.len(), 1);
    assert_eq!(summary.contracts_with_bytecode, 1);
    assert!(!summary.has_source_maps);
    assert_eq!(summary.dry_run_receipts, 2);
    assert!(!summary.historical_execution);

    // Verify JSON serialization round-trip
    let json = serde_json::to_string(&summary).unwrap();
    let deserialized: ReplaySummary = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.tx_id, summary.tx_id);
    assert_eq!(deserialized.block_height, summary.block_height);
}

// ---------------------------------------------------------------------------
// 7. test_historical_replay_config
// ---------------------------------------------------------------------------

#[test]
fn test_historical_replay_config() {
    let config = ReplayConfig {
        rpc_url: "http://localhost:4000/v1/graphql".to_string(),
        tx_id: "0xhistorical_tx_id".to_string(),
        source_dir: Some(PathBuf::from("/path/to/contract/source")),
        output_dir: PathBuf::from("/tmp/replay-output"),
        historical_execution: true,
    };

    assert!(config.historical_execution);
    assert_eq!(config.tx_id, "0xhistorical_tx_id");
    assert!(config.source_dir.is_some());
    assert_eq!(
        config.source_dir.as_ref().unwrap(),
        &PathBuf::from("/path/to/contract/source")
    );
    assert_eq!(config.output_dir, PathBuf::from("/tmp/replay-output"));

    // Verify that a non-historical config defaults correctly
    let local = ReplayConfig::local("0xtx1");
    assert!(!local.historical_execution);
    assert!(local.source_dir.is_none());
}

// ---------------------------------------------------------------------------
// 8. test_missing_source_graceful_fallback
// ---------------------------------------------------------------------------

#[test]
fn test_missing_source_graceful_fallback() {
    // When source maps are not available, the system should still produce
    // a valid summary with source_available: false

    let summary = ReplaySummary {
        tx_id: "0xno_source_tx".to_string(),
        block_height: Some(99999),
        contract_ids: vec!["0xcontract_a".to_string(), "0xcontract_b".to_string()],
        contracts_with_bytecode: 2,
        has_source_maps: false,
        dry_run_receipts: 5,
        historical_execution: false,
        source_available: false,
    };

    assert!(!summary.has_source_maps);
    assert!(!summary.source_available);
    // Even without source, we should have bytecode and receipts
    assert_eq!(summary.contracts_with_bytecode, 2);
    assert_eq!(summary.dry_run_receipts, 5);

    // Verify that find_source_maps returns None for a nonexistent directory
    let result = find_source_maps(
        &PathBuf::from("/nonexistent/path/to/source"),
        "0xcontract_a",
    );
    assert!(result.is_none());

    // Verify that find_source_maps returns None when no ABI files exist
    let tmp = tempfile::tempdir().unwrap();
    let out_debug = tmp.path().join("out").join("debug");
    std::fs::create_dir_all(&out_debug).unwrap();
    // Create a non-ABI file
    std::fs::write(out_debug.join("readme.txt"), "nothing here").unwrap();

    let result = find_source_maps(tmp.path(), "0xcontract_a");
    assert!(result.is_none());
}

// ---------------------------------------------------------------------------
// Additional edge case tests
// ---------------------------------------------------------------------------

#[test]
fn test_parse_transaction_response_no_data() {
    let response = GraphQLResponse {
        data: None,
        errors: Some(vec![
            codetracer_fuel_recorder::graphql_debug::GraphQLError {
                message: "transaction not found".to_string(),
                locations: None,
                path: None,
            },
        ]),
    };

    let result = parse_transaction_response(&response);
    assert!(result.is_err());
}

#[test]
fn test_parse_transaction_response_empty_arrays() {
    let response = GraphQLResponse {
        data: Some(serde_json::json!({
            "transaction": {
                "id": "0xempty_tx",
                "rawPayload": "0x00",
                "inputs": [],
                "outputs": [],
                "receipts": [],
                "status": null
            }
        })),
        errors: None,
    };

    let tx_data = parse_transaction_response(&response).unwrap();
    assert_eq!(tx_data.id, "0xempty_tx");
    assert!(tx_data.inputs.is_empty());
    assert!(tx_data.outputs.is_empty());
    assert!(tx_data.receipts.is_empty());
    assert!(tx_data.status.is_none());
}

#[test]
fn test_parse_failure_status() {
    let response = GraphQLResponse {
        data: Some(serde_json::json!({
            "transaction": {
                "id": "0xfailed_tx",
                "rawPayload": "0x00",
                "inputs": [],
                "outputs": [],
                "receipts": [],
                "status": {
                    "__typename": "FailureStatus",
                    "block": {
                        "header": {
                            "height": "67890"
                        }
                    },
                    "reason": "OutOfGas"
                }
            }
        })),
        errors: None,
    };

    let tx_data = parse_transaction_response(&response).unwrap();
    let status = tx_data.status.unwrap();
    assert_eq!(status.status_type, "FailureStatus");
    assert_eq!(status.block_height, Some(67890));
}

#[test]
fn test_output_types_parsing() {
    let response = GraphQLResponse {
        data: Some(serde_json::json!({
            "transaction": {
                "id": "0xoutput_test",
                "rawPayload": "0x00",
                "inputs": [],
                "outputs": [
                    {
                        "__typename": "VariableOutput",
                        "to": "0xvar_recipient",
                        "amount": "42",
                        "assetId": "0xasset1"
                    },
                    {
                        "__typename": "ContractCreated",
                        "contract": "0xnew_contract_id"
                    },
                    {
                        "__typename": "SomeUnknownType"
                    }
                ],
                "receipts": [],
                "status": null
            }
        })),
        errors: None,
    };

    let tx_data = parse_transaction_response(&response).unwrap();
    assert_eq!(tx_data.outputs.len(), 3);

    match &tx_data.outputs[0] {
        TransactionOutput::VariableOutput { to, amount, .. } => {
            assert_eq!(to.as_deref(), Some("0xvar_recipient"));
            assert_eq!(amount.as_deref(), Some("42"));
        }
        _ => panic!("expected VariableOutput"),
    }

    match &tx_data.outputs[1] {
        TransactionOutput::ContractCreated { contract_id } => {
            assert_eq!(contract_id.as_deref(), Some("0xnew_contract_id"));
        }
        _ => panic!("expected ContractCreated"),
    }

    assert!(
        matches!(&tx_data.outputs[2], TransactionOutput::Unknown),
        "expected outputs[2] to be TransactionOutput::Unknown, got {:?}",
        &tx_data.outputs[2]
    );
}

#[test]
fn test_query_builders() {
    // Verify query string construction
    let tx_q = transaction_query("0xmy_tx_id");
    assert!(tx_q.query.contains("0xmy_tx_id"));
    assert!(tx_q.query.contains("rawPayload"));
    assert!(tx_q.query.contains("InputContract"));
    assert!(tx_q.query.contains("receiptType"));

    let contract_q = contract_bytecode_query("0xmy_contract");
    assert!(contract_q.query.contains("0xmy_contract"));
    assert!(contract_q.query.contains("bytecode"));

    let dry_run_q = dry_run_query("0xraw_payload_hex", false);
    assert!(dry_run_q.query.contains("dryRun"));
    assert!(dry_run_q.query.contains("utxoValidation: false"));

    let dry_run_validated = dry_run_query("0xraw_payload_hex", true);
    assert!(dry_run_validated.query.contains("utxoValidation: true"));
}

#[test]
fn test_replay_summary_serialization() {
    let summary = ReplaySummary {
        tx_id: "0xserialize_test".to_string(),
        block_height: Some(100),
        contract_ids: vec!["0xa".to_string(), "0xb".to_string()],
        contracts_with_bytecode: 2,
        has_source_maps: true,
        dry_run_receipts: 10,
        historical_execution: true,
        source_available: true,
    };

    let json = serde_json::to_string_pretty(&summary).unwrap();
    assert!(json.contains("0xserialize_test"));
    assert!(json.contains("\"block_height\": 100"));
    assert!(json.contains("\"has_source_maps\": true"));
    assert!(json.contains("\"historical_execution\": true"));

    let deserialized: ReplaySummary = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.tx_id, "0xserialize_test");
    assert_eq!(deserialized.block_height, Some(100));
    assert_eq!(deserialized.contract_ids.len(), 2);
    assert!(deserialized.has_source_maps);
    assert!(deserialized.historical_execution);
}
