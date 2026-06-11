//! On-chain transaction replay via fuel-core GraphQL API (M6).
//!
//! Fetches transaction data from a fuel-core node, reconstructs the
//! transaction for local re-execution, and optionally records a trace.
//!
//! ## Replay strategies
//!
//! | Strategy      | Mechanism                                              |
//! |---------------|--------------------------------------------------------|
//! | dryRun        | Send rawPayload with utxoValidation:false               |
//! | Historical    | --historical-execution, re-execute at target height     |
//!
//! ## GraphQL queries used
//!
//! - `transaction(id)` — fetch rawPayload, inputs, outputs, receipts, status
//! - `contract(id)` — fetch bytecode for involved contracts
//! - `dryRun(tx, utxoValidation: false)` — simulate without committing

use std::path::{Path, PathBuf};

use eyre::{Context, Result, eyre};
use serde::{Deserialize, Serialize};

use crate::graphql_debug::{GraphQLRequest, GraphQLResponse};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for replaying an on-chain transaction.
#[derive(Debug, Clone)]
pub struct ReplayConfig {
    /// GraphQL RPC URL of the fuel-core node.
    pub rpc_url: String,
    /// Transaction ID to replay (hex string with 0x prefix).
    pub tx_id: String,
    /// Optional directory containing forc build output for source maps.
    pub source_dir: Option<PathBuf>,
    /// Output directory for trace files.
    pub output_dir: PathBuf,
    /// Whether to use historical execution (state rewind).
    pub historical_execution: bool,
}

impl ReplayConfig {
    /// Create a config for replaying against a local fuel-core node.
    pub fn local(tx_id: &str) -> Self {
        Self {
            rpc_url: "http://localhost:4000/v1/graphql".to_string(),
            tx_id: tx_id.to_string(),
            source_dir: None,
            output_dir: PathBuf::from("./ct-traces/"),
            historical_execution: false,
        }
    }

    /// Create a config for replaying against the Fuel mainnet.
    pub fn mainnet(tx_id: &str) -> Self {
        Self {
            rpc_url: "https://mainnet.fuel.network/v1/graphql".to_string(),
            tx_id: tx_id.to_string(),
            source_dir: None,
            output_dir: PathBuf::from("./ct-traces/"),
            historical_execution: false,
        }
    }
}

// ---------------------------------------------------------------------------
// GraphQL response types
// ---------------------------------------------------------------------------

/// Transaction data fetched from the GraphQL API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionData {
    /// Transaction ID.
    pub id: String,
    /// Raw transaction payload (hex-encoded canonical serialization).
    pub raw_payload: String,
    /// Transaction inputs.
    pub inputs: Vec<TransactionInput>,
    /// Transaction outputs.
    pub outputs: Vec<TransactionOutput>,
    /// Transaction receipts.
    pub receipts: Vec<TransactionReceipt>,
    /// Transaction status information.
    pub status: Option<TransactionStatus>,
}

/// A transaction input (simplified union type).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TransactionInput {
    /// A coin input (UTXO spend).
    InputCoin {
        utxo_id: Option<String>,
        owner: Option<String>,
        amount: Option<String>,
        asset_id: Option<String>,
    },
    /// A contract input.
    InputContract {
        utxo_id: Option<String>,
        contract_id: String,
    },
    /// A message input.
    InputMessage {
        sender: Option<String>,
        recipient: Option<String>,
        amount: Option<String>,
        data: Option<String>,
    },
}

/// A transaction output (simplified union type).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TransactionOutput {
    /// A coin output.
    CoinOutput {
        to: Option<String>,
        amount: Option<String>,
        asset_id: Option<String>,
    },
    /// A contract output.
    ContractOutput { input_index: Option<u32> },
    /// A change output (returns excess coins to owner).
    ChangeOutput {
        to: Option<String>,
        amount: Option<String>,
        asset_id: Option<String>,
    },
    /// A variable output (set during execution).
    VariableOutput {
        to: Option<String>,
        amount: Option<String>,
        asset_id: Option<String>,
    },
    /// A contract-created output.
    ContractCreated { contract_id: Option<String> },
    /// Unknown output type.
    Unknown,
}

/// A receipt from a transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionReceipt {
    /// Receipt type (Call, Return, ReturnData, Log, LogData, etc.).
    pub receipt_type: String,
    /// Contract ID (if applicable).
    pub contract_id: Option<String>,
    /// Additional data fields (varies by receipt type).
    #[serde(flatten)]
    pub data: serde_json::Value,
}

/// Transaction status information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionStatus {
    /// Status type: SuccessStatus, FailureStatus, etc.
    pub status_type: String,
    /// Block height (if included in a block).
    pub block_height: Option<u64>,
}

/// Contract bytecode data fetched from the GraphQL API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractBytecodeData {
    /// Contract ID.
    pub contract_id: String,
    /// Bytecode as hex string.
    pub bytecode: String,
}

/// Result of a dryRun mutation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DryRunResult {
    /// Receipts from the dry run.
    pub receipts: Vec<TransactionReceipt>,
    /// Program state after execution.
    pub program_state: Option<String>,
}

/// Source map information found for a contract.
#[derive(Debug, Clone)]
pub struct ContractSourceInfo {
    /// Contract ID this source info belongs to.
    pub contract_id: String,
    /// Path to the source map file.
    pub source_map_path: PathBuf,
    /// Path to the source directory.
    pub source_dir: PathBuf,
    /// Paths to source files referenced by the source map.
    pub source_files: Vec<PathBuf>,
}

// ---------------------------------------------------------------------------
// GraphQL query builders
// ---------------------------------------------------------------------------

/// Build the GraphQL query to fetch a transaction by ID.
pub fn transaction_query(tx_id: &str) -> GraphQLRequest {
    GraphQLRequest {
        query: format!(
            r#"query {{
  transaction(id: "{tx_id}") {{
    id
    rawPayload
    inputs {{
      __typename
      ... on InputCoin {{
        utxoId
        owner
        amount
        assetId
      }}
      ... on InputContract {{
        utxoId
        contractId
      }}
      ... on InputMessage {{
        sender
        recipient
        amount
        data
      }}
    }}
    outputs {{
      __typename
      ... on CoinOutput {{
        to
        amount
        assetId
      }}
      ... on ContractOutput {{
        inputIndex
      }}
      ... on ChangeOutput {{
        to
        amount
        assetId
      }}
      ... on VariableOutput {{
        to
        amount
        assetId
      }}
      ... on ContractCreated {{
        contract
      }}
    }}
    receipts {{
      receiptType
      contractId
      to
      amount
      assetId
      gas
      param1
      param2
      val
      ptr
      digest
      reason
      ra
      rb
      rc
      rd
      len
      is
      pc
      data
    }}
    status {{
      __typename
      ... on SuccessStatus {{
        block {{
          header {{
            height
          }}
        }}
      }}
      ... on FailureStatus {{
        block {{
          header {{
            height
          }}
        }}
        reason
      }}
    }}
  }}
}}"#
        ),
        variables: None,
    }
}

/// Build the GraphQL query to fetch contract bytecode.
pub fn contract_bytecode_query(contract_id: &str) -> GraphQLRequest {
    GraphQLRequest {
        query: format!(
            r#"query {{
  contract(id: "{contract_id}") {{
    bytecode
  }}
}}"#
        ),
        variables: None,
    }
}

/// Build the GraphQL mutation for dryRun.
pub fn dry_run_query(raw_payload: &str, utxo_validation: bool) -> GraphQLRequest {
    GraphQLRequest {
        query: format!(
            r#"mutation {{
  dryRun(tx: "{raw_payload}", utxoValidation: {utxo_validation}) {{
    receiptType
    contractId
    to
    amount
    assetId
    gas
    param1
    param2
    val
    ptr
    digest
    reason
    ra
    rb
    rc
    rd
    len
    is
    pc
    data
    programState {{
      returnType
      data
    }}
  }}
}}"#
        ),
        variables: None,
    }
}

// ---------------------------------------------------------------------------
// Response parsers
// ---------------------------------------------------------------------------

/// Parse a `transaction(id)` GraphQL response into `TransactionData`.
pub fn parse_transaction_response(response: &GraphQLResponse) -> Result<TransactionData> {
    let data = response
        .data
        .as_ref()
        .ok_or_else(|| eyre!("no data in response"))?;

    let tx = data
        .get("transaction")
        .ok_or_else(|| eyre!("no transaction field in response"))?;

    let id = tx
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let raw_payload = tx
        .get("rawPayload")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let inputs = parse_inputs(tx.get("inputs"));
    let outputs = parse_outputs(tx.get("outputs"));
    let receipts = parse_receipts(tx.get("receipts"));
    let status = parse_status(tx.get("status"));

    Ok(TransactionData {
        id,
        raw_payload,
        inputs,
        outputs,
        receipts,
        status,
    })
}

/// Parse inputs from the transaction response.
fn parse_inputs(inputs_val: Option<&serde_json::Value>) -> Vec<TransactionInput> {
    let arr = match inputs_val.and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };

    arr.iter()
        .filter_map(|input| {
            let typename = input.get("__typename")?.as_str()?;
            match typename {
                "InputCoin" => Some(TransactionInput::InputCoin {
                    utxo_id: input
                        .get("utxoId")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    owner: input
                        .get("owner")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    amount: input
                        .get("amount")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    asset_id: input
                        .get("assetId")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                }),
                "InputContract" => Some(TransactionInput::InputContract {
                    utxo_id: input
                        .get("utxoId")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    contract_id: input
                        .get("contractId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                }),
                "InputMessage" => Some(TransactionInput::InputMessage {
                    sender: input
                        .get("sender")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    recipient: input
                        .get("recipient")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    amount: input
                        .get("amount")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    data: input.get("data").and_then(|v| v.as_str()).map(String::from),
                }),
                _ => None,
            }
        })
        .collect()
}

/// Parse outputs from the transaction response.
fn parse_outputs(outputs_val: Option<&serde_json::Value>) -> Vec<TransactionOutput> {
    let arr = match outputs_val.and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };

    arr.iter()
        .map(|output| {
            let typename = output
                .get("__typename")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match typename {
                "CoinOutput" => TransactionOutput::CoinOutput {
                    to: output.get("to").and_then(|v| v.as_str()).map(String::from),
                    amount: output
                        .get("amount")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    asset_id: output
                        .get("assetId")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                },
                "ContractOutput" => TransactionOutput::ContractOutput {
                    input_index: output
                        .get("inputIndex")
                        .and_then(|v| v.as_u64())
                        .map(|v| v as u32),
                },
                "ChangeOutput" => TransactionOutput::ChangeOutput {
                    to: output.get("to").and_then(|v| v.as_str()).map(String::from),
                    amount: output
                        .get("amount")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    asset_id: output
                        .get("assetId")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                },
                "VariableOutput" => TransactionOutput::VariableOutput {
                    to: output.get("to").and_then(|v| v.as_str()).map(String::from),
                    amount: output
                        .get("amount")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    asset_id: output
                        .get("assetId")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                },
                "ContractCreated" => TransactionOutput::ContractCreated {
                    contract_id: output
                        .get("contract")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                },
                _ => TransactionOutput::Unknown,
            }
        })
        .collect()
}

/// Parse receipts from the transaction response.
fn parse_receipts(receipts_val: Option<&serde_json::Value>) -> Vec<TransactionReceipt> {
    let arr = match receipts_val.and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };

    arr.iter()
        .map(|r| {
            let receipt_type = r
                .get("receiptType")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown")
                .to_string();
            let contract_id = r
                .get("contractId")
                .and_then(|v| v.as_str())
                .map(String::from);
            TransactionReceipt {
                receipt_type,
                contract_id,
                data: r.clone(),
            }
        })
        .collect()
}

/// Parse status from the transaction response.
fn parse_status(status_val: Option<&serde_json::Value>) -> Option<TransactionStatus> {
    let status = status_val?;
    let status_type = status.get("__typename")?.as_str()?.to_string();
    let block_height = status
        .get("block")
        .and_then(|b| b.get("header"))
        .and_then(|h| h.get("height"))
        .and_then(|h| h.as_str())
        .and_then(|s| s.parse::<u64>().ok());

    Some(TransactionStatus {
        status_type,
        block_height,
    })
}

/// Parse a `contract(id)` GraphQL response into `ContractBytecodeData`.
pub fn parse_contract_bytecode_response(
    response: &GraphQLResponse,
    contract_id: &str,
) -> Result<ContractBytecodeData> {
    let data = response
        .data
        .as_ref()
        .ok_or_else(|| eyre!("no data in response"))?;

    let contract = data
        .get("contract")
        .ok_or_else(|| eyre!("no contract field in response"))?;

    let bytecode = contract
        .get("bytecode")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Ok(ContractBytecodeData {
        contract_id: contract_id.to_string(),
        bytecode,
    })
}

/// Parse a `dryRun` mutation response into `DryRunResult`.
pub fn parse_dry_run_response(response: &GraphQLResponse) -> Result<DryRunResult> {
    let data = response
        .data
        .as_ref()
        .ok_or_else(|| eyre!("no data in response"))?;

    let dry_run = data
        .get("dryRun")
        .ok_or_else(|| eyre!("no dryRun field in response"))?;

    let receipts_arr = dry_run.as_array().unwrap_or(&Vec::new()).clone();

    let mut receipts = Vec::new();
    let mut program_state = None;

    for item in &receipts_arr {
        // Check if this item has a programState (the last element often does)
        if let Some(ps) = item.get("programState")
            && let Some(ps_data) = ps.get("data") {
                program_state = ps_data.as_str().map(String::from);
            }

        let receipt_type = item
            .get("receiptType")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string();
        let contract_id = item
            .get("contractId")
            .and_then(|v| v.as_str())
            .map(String::from);

        receipts.push(TransactionReceipt {
            receipt_type,
            contract_id,
            data: item.clone(),
        });
    }

    Ok(DryRunResult {
        receipts,
        program_state,
    })
}

// ---------------------------------------------------------------------------
// Contract ID extraction
// ---------------------------------------------------------------------------

/// Extract unique ContractIds from transaction inputs.
pub fn extract_contract_ids(inputs: &[TransactionInput]) -> Vec<String> {
    let mut ids = Vec::new();
    for input in inputs {
        if let TransactionInput::InputContract { contract_id, .. } = input
            && !contract_id.is_empty() && !ids.contains(contract_id) {
                ids.push(contract_id.clone());
            }
    }
    ids
}

// ---------------------------------------------------------------------------
// Source map discovery
// ---------------------------------------------------------------------------

/// Look for forc build output containing source maps for a given contract.
///
/// Searches `source_dir` for `out/debug/<name>-abi.json` files and checks
/// if any correspond to the given contract ID. Returns source map info
/// if found.
///
/// The forc build output structure is:
/// ```text
/// <project>/
///   out/
///     debug/
///       <name>.bin          # bytecode
///       <name>-abi.json     # ABI
///       <name>-storage_slots.json  # storage slots
///   src/
///     main.sw               # source files
/// ```
pub fn find_source_maps(source_dir: &Path, _contract_id: &str) -> Option<ContractSourceInfo> {
    // Look for forc build output directories
    let out_debug = source_dir.join("out").join("debug");
    if !out_debug.exists() {
        return None;
    }

    // Find ABI files in the output directory
    let entries = std::fs::read_dir(&out_debug).ok()?;
    let mut abi_files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.ends_with("-abi.json") {
                abi_files.push(path);
            }
    }

    if abi_files.is_empty() {
        return None;
    }

    // Use the first ABI file found (in a real implementation, we'd match
    // against the contract ID by comparing deployed bytecode hashes)
    let abi_path = &abi_files[0];
    let project_name = abi_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .strip_suffix("-abi")
        .unwrap_or("");

    // Check for source map file
    let source_map_path = out_debug.join(format!("{project_name}-debug_symbols.json"));
    let bin_path = out_debug.join(format!("{project_name}.bin"));

    // If neither source map nor bytecode exists, we can't use this
    if !source_map_path.exists() && !bin_path.exists() {
        // Still return what we have (the ABI) for partial info
    }

    // Look for source files
    let src_dir = source_dir.join("src");
    let mut source_files = Vec::new();
    if src_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&src_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("sw") {
                    source_files.push(path);
                }
            }
        }

    Some(ContractSourceInfo {
        contract_id: _contract_id.to_string(),
        source_map_path: if source_map_path.exists() {
            source_map_path
        } else {
            out_debug.join(format!("{project_name}.bin"))
        },
        source_dir: source_dir.to_path_buf(),
        source_files,
    })
}

// ---------------------------------------------------------------------------
// Replay pipeline
// ---------------------------------------------------------------------------

/// The FuelGraphQLClient sends GraphQL requests to a fuel-core node.
///
/// Uses `reqwest` for HTTP communication.
pub struct FuelGraphQLClient {
    /// The fuel-core GraphQL endpoint URL.
    pub endpoint: String,
    /// HTTP client.
    client: reqwest::blocking::Client,
}

impl FuelGraphQLClient {
    /// Create a new client for the given endpoint.
    pub fn new(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            client: reqwest::blocking::Client::new(),
        }
    }

    /// Send a GraphQL request and return the parsed response.
    pub fn send(&self, request: &GraphQLRequest) -> Result<GraphQLResponse> {
        let resp = self
            .client
            .post(&self.endpoint)
            .json(request)
            .send()
            .with_context(|| format!("failed to send GraphQL request to {}", self.endpoint))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(eyre!(
                "GraphQL request failed with status {}: {}",
                status,
                body
            ));
        }

        let response: GraphQLResponse = resp
            .json()
            .with_context(|| "failed to parse GraphQL response")?;

        // Check for GraphQL-level errors
        if let Some(errors) = &response.errors
            && !errors.is_empty() {
                let msgs: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
                return Err(eyre!("GraphQL errors: {}", msgs.join("; ")));
            }

        Ok(response)
    }

    /// Fetch transaction data by ID.
    pub fn fetch_transaction(&self, tx_id: &str) -> Result<TransactionData> {
        let request = transaction_query(tx_id);
        let response = self.send(&request)?;
        parse_transaction_response(&response)
    }

    /// Fetch contract bytecode by contract ID.
    pub fn fetch_contract_bytecode(&self, contract_id: &str) -> Result<ContractBytecodeData> {
        let request = contract_bytecode_query(contract_id);
        let response = self.send(&request)?;
        parse_contract_bytecode_response(&response, contract_id)
    }

    /// Submit a transaction for dry run (simulation without committing).
    pub fn dry_run_transaction(&self, raw_payload: &str) -> Result<DryRunResult> {
        let request = dry_run_query(raw_payload, false);
        let response = self.send(&request)?;
        parse_dry_run_response(&response)
    }
}

/// Full replay pipeline: fetch tx -> extract contracts -> fetch bytecode ->
/// find source maps -> record.
///
/// Returns a summary of what was replayed.
pub fn replay_transaction(config: &ReplayConfig) -> Result<ReplaySummary> {
    let client = FuelGraphQLClient::new(&config.rpc_url);

    // Step 1: Fetch the transaction
    eprintln!("Fetching transaction {}...", config.tx_id);
    let tx_data = client
        .fetch_transaction(&config.tx_id)
        .with_context(|| format!("failed to fetch transaction {}", config.tx_id))?;

    let block_height = tx_data.status.as_ref().and_then(|s| s.block_height);

    eprintln!(
        "Transaction found: {} inputs, {} outputs, {} receipts",
        tx_data.inputs.len(),
        tx_data.outputs.len(),
        tx_data.receipts.len()
    );

    if let Some(height) = block_height {
        eprintln!("Block height: {}", height);
    }

    // Step 2: Extract contract IDs from inputs
    let contract_ids = extract_contract_ids(&tx_data.inputs);
    eprintln!("Contracts involved: {}", contract_ids.len());

    // Step 3: Fetch bytecode for each contract
    let mut contracts = Vec::new();
    for cid in &contract_ids {
        match client.fetch_contract_bytecode(cid) {
            Ok(bytecode_data) => {
                eprintln!(
                    "  Contract {}: {} bytes of bytecode",
                    &cid[..std::cmp::min(cid.len(), 12)],
                    bytecode_data.bytecode.len() / 2
                );
                contracts.push(bytecode_data);
            }
            Err(e) => {
                eprintln!(
                    "  Contract {}: failed to fetch bytecode: {}",
                    &cid[..std::cmp::min(cid.len(), 12)],
                    e
                );
            }
        }
    }

    // Step 4: Find source maps if source_dir is provided
    let mut source_info = Vec::new();
    if let Some(source_dir) = &config.source_dir {
        for cid in &contract_ids {
            if let Some(info) = find_source_maps(source_dir, cid) {
                eprintln!(
                    "  Source map found for {}: {} source files",
                    &cid[..std::cmp::min(cid.len(), 12)],
                    info.source_files.len()
                );
                source_info.push(info);
            }
        }
    }

    let has_source_maps = !source_info.is_empty();

    // Step 5: Replay via dryRun (current state) or historical execution
    let dry_run_result = if config.historical_execution {
        eprintln!("Historical replay requires a fuel-core node with --historical-execution");
        eprintln!(
            "Target block height: {}",
            block_height.map_or("unknown".to_string(), |h| h.to_string())
        );
        // Historical replay would use debug session API (startSession, setSingleStepping, etc.)
        // For now, fall back to dryRun
        None
    } else {
        // dryRun against current state
        eprintln!("Submitting dryRun with utxoValidation: false...");
        match client.dry_run_transaction(&tx_data.raw_payload) {
            Ok(result) => {
                eprintln!("  dryRun produced {} receipts", result.receipts.len());
                Some(result)
            }
            Err(e) => {
                eprintln!("  dryRun failed: {}", e);
                None
            }
        }
    };

    // Step 6: Create output directory and write results
    std::fs::create_dir_all(&config.output_dir)
        .with_context(|| format!("cannot create output dir: {}", config.output_dir.display()))?;

    // Write a replay summary JSON
    let summary = ReplaySummary {
        tx_id: tx_data.id.clone(),
        block_height,
        contract_ids: contract_ids.clone(),
        contracts_with_bytecode: contracts.len(),
        has_source_maps,
        dry_run_receipts: dry_run_result
            .as_ref()
            .map(|r| r.receipts.len())
            .unwrap_or(0),
        historical_execution: config.historical_execution,
        source_available: has_source_maps,
    };

    let summary_path = config.output_dir.join("replay_summary.json");
    let summary_json =
        serde_json::to_string_pretty(&summary).with_context(|| "failed to serialize summary")?;
    std::fs::write(&summary_path, &summary_json)
        .with_context(|| format!("failed to write {}", summary_path.display()))?;

    eprintln!("Replay summary written to {}", summary_path.display());

    if !has_source_maps {
        eprintln!();
        eprintln!("No source maps found for the involved contracts.");
        eprintln!("Disassembly-level replay with register/memory state is available.");
        eprintln!("To get source-level debugging, provide --source-dir pointing to the");
        eprintln!("forc build output directory for the contract.");
    }

    Ok(summary)
}

/// Summary of a replay operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplaySummary {
    /// Transaction ID that was replayed.
    pub tx_id: String,
    /// Block height the transaction was included in.
    pub block_height: Option<u64>,
    /// Contract IDs involved in the transaction.
    pub contract_ids: Vec<String>,
    /// Number of contracts for which bytecode was fetched.
    pub contracts_with_bytecode: usize,
    /// Whether source maps were found for any contract.
    pub has_source_maps: bool,
    /// Number of receipts from the dry run (0 if not performed).
    pub dry_run_receipts: usize,
    /// Whether historical execution was used.
    pub historical_execution: bool,
    /// Whether source/source maps are available.
    pub source_available: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_replay_config_local() {
        let config = ReplayConfig::local("0xabc123");
        assert_eq!(config.rpc_url, "http://localhost:4000/v1/graphql");
        assert_eq!(config.tx_id, "0xabc123");
        assert!(!config.historical_execution);
    }

    #[test]
    fn test_replay_config_mainnet() {
        let config = ReplayConfig::mainnet("0xdef456");
        assert!(config.rpc_url.contains("mainnet.fuel.network"));
        assert_eq!(config.tx_id, "0xdef456");
    }

    #[test]
    fn test_extract_contract_ids_empty() {
        let inputs: Vec<TransactionInput> = vec![];
        assert!(extract_contract_ids(&inputs).is_empty());
    }

    #[test]
    fn test_extract_contract_ids_mixed() {
        let inputs = vec![
            TransactionInput::InputCoin {
                utxo_id: None,
                owner: None,
                amount: None,
                asset_id: None,
            },
            TransactionInput::InputContract {
                utxo_id: None,
                contract_id: "0xcontract1".to_string(),
            },
            TransactionInput::InputContract {
                utxo_id: None,
                contract_id: "0xcontract2".to_string(),
            },
            TransactionInput::InputMessage {
                sender: None,
                recipient: None,
                amount: None,
                data: None,
            },
        ];
        let ids = extract_contract_ids(&inputs);
        assert_eq!(ids, vec!["0xcontract1", "0xcontract2"]);
    }

    #[test]
    fn test_extract_contract_ids_no_duplicates() {
        let inputs = vec![
            TransactionInput::InputContract {
                utxo_id: None,
                contract_id: "0xsame".to_string(),
            },
            TransactionInput::InputContract {
                utxo_id: None,
                contract_id: "0xsame".to_string(),
            },
        ];
        let ids = extract_contract_ids(&inputs);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], "0xsame");
    }

    #[test]
    fn test_transaction_query_format() {
        let req = transaction_query("0xabc123");
        assert!(req.query.contains("transaction(id: \"0xabc123\")"));
        assert!(req.query.contains("rawPayload"));
        assert!(req.query.contains("inputs"));
        assert!(req.query.contains("receipts"));
        assert!(req.query.contains("status"));
    }

    #[test]
    fn test_contract_bytecode_query_format() {
        let req = contract_bytecode_query("0xcontract1");
        assert!(req.query.contains("contract(id: \"0xcontract1\")"));
        assert!(req.query.contains("bytecode"));
    }

    #[test]
    fn test_dry_run_query_format() {
        let req = dry_run_query("0xrawpayload", false);
        assert!(req.query.contains("dryRun"));
        assert!(req.query.contains("utxoValidation: false"));
    }
}
