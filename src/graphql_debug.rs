//! FuelVM GraphQL debug API client (remote tracing fallback).
//!
//! fuel-core exposes a GraphQL debug API when started with `--debug`. This
//! module provides a client that can be used as a fallback for tracing
//! transactions against a running fuel-core node.
//!
//! **Important:** This API requires one GraphQL round-trip per instruction,
//! making it impractical for bulk trace collection. It is provided as a
//! fallback for scenarios where embedded fuel-vm execution is not possible
//! (e.g. tracing against live state).
//!
//! ## API operations
//!
//! - `startSession` — create a new debug session
//! - `endSession(id)` — end a debug session
//! - `setSingleStepping(id, enable)` — enable/disable single-stepping
//! - `startTx(id, txJson)` — submit a transaction for debugging
//! - `continueTx(id)` — continue/step execution
//! - `register(id, register)` — read a register value
//! - `memory(id, start, size)` — read memory contents

use serde::{Deserialize, Serialize};

/// Configuration for connecting to a fuel-core GraphQL debug API endpoint.
#[derive(Debug, Clone)]
pub struct GraphQLDebugConfig {
    /// The URL of the fuel-core GraphQL endpoint (e.g. "http://localhost:4000/v1/graphql").
    pub endpoint: String,
}

impl GraphQLDebugConfig {
    /// Create a config for a local fuel-core node on the default port.
    pub fn local() -> Self {
        Self {
            endpoint: "http://localhost:4000/v1/graphql".to_string(),
        }
    }

    /// Create a config with a custom endpoint URL.
    pub fn with_endpoint(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
        }
    }
}

impl Default for GraphQLDebugConfig {
    fn default() -> Self {
        Self::local()
    }
}

// ---------------------------------------------------------------------------
// GraphQL request/response types
// ---------------------------------------------------------------------------

/// A GraphQL request body.
#[derive(Debug, Clone, Serialize)]
pub struct GraphQLRequest {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<serde_json::Value>,
}

/// A GraphQL response body.
#[derive(Debug, Clone, Deserialize)]
pub struct GraphQLResponse {
    pub data: Option<serde_json::Value>,
    pub errors: Option<Vec<GraphQLError>>,
}

/// A GraphQL error entry.
#[derive(Debug, Clone, Deserialize)]
pub struct GraphQLError {
    pub message: String,
    pub locations: Option<Vec<serde_json::Value>>,
    pub path: Option<Vec<serde_json::Value>>,
}

impl std::fmt::Display for GraphQLError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

// ---------------------------------------------------------------------------
// Debug session types
// ---------------------------------------------------------------------------

/// Result of a continue/step operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugContinueResult {
    /// Execution hit a breakpoint at the given PC.
    Breakpoint(u64),
    /// Execution completed.
    Completed,
}

/// A debug session ID returned by `startSession`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);

// ---------------------------------------------------------------------------
// Query builders
// ---------------------------------------------------------------------------

/// Build the GraphQL mutation for starting a debug session.
pub fn start_session_query() -> GraphQLRequest {
    GraphQLRequest {
        query: "mutation { startSession { id } }".to_string(),
        variables: None,
    }
}

/// Build the GraphQL mutation for ending a debug session.
pub fn end_session_query(session_id: &SessionId) -> GraphQLRequest {
    GraphQLRequest {
        query: format!(
            r#"mutation {{ endSession(id: "{}") }}"#,
            session_id.0
        ),
        variables: None,
    }
}

/// Build the GraphQL mutation for enabling/disabling single-stepping.
pub fn set_single_stepping_query(session_id: &SessionId, enable: bool) -> GraphQLRequest {
    GraphQLRequest {
        query: format!(
            r#"mutation {{ setSingleStepping(id: "{}", enable: {}) }}"#,
            session_id.0, enable
        ),
        variables: None,
    }
}

/// Build the GraphQL mutation for submitting a transaction.
pub fn start_tx_query(session_id: &SessionId, tx_json: &str) -> GraphQLRequest {
    // Escape the JSON string for embedding in GraphQL
    let escaped = tx_json.replace('\\', "\\\\").replace('"', "\\\"");
    GraphQLRequest {
        query: format!(
            r#"mutation {{ startTx(id: "{}", txJson: "{}") {{ state }} }}"#,
            session_id.0, escaped
        ),
        variables: None,
    }
}

/// Build the GraphQL mutation for continuing execution (single step).
pub fn continue_tx_query(session_id: &SessionId) -> GraphQLRequest {
    GraphQLRequest {
        query: format!(
            r#"mutation {{ continueTx(id: "{}") {{ state }} }}"#,
            session_id.0
        ),
        variables: None,
    }
}

/// Build the GraphQL query for reading a register value.
pub fn register_query(session_id: &SessionId, register: u32) -> GraphQLRequest {
    GraphQLRequest {
        query: format!(
            r#"query {{ register(id: "{}", register: {}) }}"#,
            session_id.0, register
        ),
        variables: None,
    }
}

/// Build the GraphQL query for reading memory contents.
pub fn memory_query(session_id: &SessionId, start: u64, size: u64) -> GraphQLRequest {
    GraphQLRequest {
        query: format!(
            r#"query {{ memory(id: "{}", start: "{}", size: "{}") }}"#,
            session_id.0, start, size
        ),
        variables: None,
    }
}

/// Parse a `startSession` response to extract the session ID.
pub fn parse_start_session_response(response: &GraphQLResponse) -> Option<SessionId> {
    response
        .data
        .as_ref()?
        .get("startSession")?
        .get("id")?
        .as_str()
        .map(|s| SessionId(s.to_string()))
}

/// Parse a `continueTx` response to determine the execution state.
pub fn parse_continue_response(response: &GraphQLResponse) -> Option<DebugContinueResult> {
    let state = response
        .data
        .as_ref()?
        .get("continueTx")?
        .get("state")?
        .as_str()?;

    match state {
        "COMPLETED" => Some(DebugContinueResult::Completed),
        s if s.starts_with("BREAKPOINT:") => {
            let pc_str = s.strip_prefix("BREAKPOINT:")?;
            let pc = pc_str.trim().parse::<u64>().ok()?;
            Some(DebugContinueResult::Breakpoint(pc))
        }
        _ => None,
    }
}

/// Parse a `register` query response to extract the register value.
pub fn parse_register_response(response: &GraphQLResponse) -> Option<u64> {
    let val = response.data.as_ref()?.get("register")?;
    // fuel-core returns register values as strings (decimal or hex)
    if let Some(s) = val.as_str() {
        if let Some(hex) = s.strip_prefix("0x") {
            u64::from_str_radix(hex, 16).ok()
        } else {
            s.parse::<u64>().ok()
        }
    } else {
        val.as_u64()
    }
}

/// Parse a `memory` query response to extract the memory bytes.
pub fn parse_memory_response(response: &GraphQLResponse) -> Option<Vec<u8>> {
    let val = response.data.as_ref()?.get("memory")?.as_str()?;
    // fuel-core returns memory as a hex string
    hex_decode(val)
}

/// Decode a hex string to bytes.
fn hex_decode(hex: &str) -> Option<Vec<u8>> {
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    if hex.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for i in (0..hex.len()).step_by(2) {
        let byte = u8::from_str_radix(&hex[i..i + 2], 16).ok()?;
        bytes.push(byte);
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_local() {
        let config = GraphQLDebugConfig::local();
        assert_eq!(config.endpoint, "http://localhost:4000/v1/graphql");
    }

    #[test]
    fn test_config_custom() {
        let config = GraphQLDebugConfig::with_endpoint("http://example.com/graphql");
        assert_eq!(config.endpoint, "http://example.com/graphql");
    }

    #[test]
    fn test_start_session_query() {
        let req = start_session_query();
        assert!(req.query.contains("startSession"));
        assert!(req.variables.is_none());
    }

    #[test]
    fn test_end_session_query() {
        let session = SessionId("test-session-123".to_string());
        let req = end_session_query(&session);
        assert!(req.query.contains("endSession"));
        assert!(req.query.contains("test-session-123"));
    }

    #[test]
    fn test_set_single_stepping_query() {
        let session = SessionId("sess-1".to_string());
        let req = set_single_stepping_query(&session, true);
        assert!(req.query.contains("setSingleStepping"));
        assert!(req.query.contains("true"));

        let req2 = set_single_stepping_query(&session, false);
        assert!(req2.query.contains("false"));
    }

    #[test]
    fn test_start_tx_query() {
        let session = SessionId("sess-1".to_string());
        let tx_json = r#"{"type":"Script","data":"0x1234"}"#;
        let req = start_tx_query(&session, tx_json);
        assert!(req.query.contains("startTx"));
        assert!(req.query.contains("sess-1"));
    }

    #[test]
    fn test_continue_tx_query() {
        let session = SessionId("sess-1".to_string());
        let req = continue_tx_query(&session);
        assert!(req.query.contains("continueTx"));
        assert!(req.query.contains("sess-1"));
    }

    #[test]
    fn test_register_query() {
        let session = SessionId("sess-1".to_string());
        let req = register_query(&session, 16);
        assert!(req.query.contains("register"));
        assert!(req.query.contains("16"));
    }

    #[test]
    fn test_memory_query() {
        let session = SessionId("sess-1".to_string());
        let req = memory_query(&session, 0x1000, 256);
        assert!(req.query.contains("memory"));
        assert!(req.query.contains("4096")); // 0x1000
        assert!(req.query.contains("256"));
    }

    #[test]
    fn test_parse_start_session_response() {
        let response = GraphQLResponse {
            data: Some(serde_json::json!({
                "startSession": {
                    "id": "session-abc-123"
                }
            })),
            errors: None,
        };
        let session = parse_start_session_response(&response).unwrap();
        assert_eq!(session.0, "session-abc-123");
    }

    #[test]
    fn test_parse_continue_response_breakpoint() {
        let response = GraphQLResponse {
            data: Some(serde_json::json!({
                "continueTx": {
                    "state": "BREAKPOINT:42"
                }
            })),
            errors: None,
        };
        let result = parse_continue_response(&response).unwrap();
        assert_eq!(result, DebugContinueResult::Breakpoint(42));
    }

    #[test]
    fn test_parse_continue_response_completed() {
        let response = GraphQLResponse {
            data: Some(serde_json::json!({
                "continueTx": {
                    "state": "COMPLETED"
                }
            })),
            errors: None,
        };
        let result = parse_continue_response(&response).unwrap();
        assert_eq!(result, DebugContinueResult::Completed);
    }

    #[test]
    fn test_parse_register_response_decimal() {
        let response = GraphQLResponse {
            data: Some(serde_json::json!({
                "register": "42"
            })),
            errors: None,
        };
        let val = parse_register_response(&response).unwrap();
        assert_eq!(val, 42);
    }

    #[test]
    fn test_parse_register_response_hex() {
        let response = GraphQLResponse {
            data: Some(serde_json::json!({
                "register": "0x2a"
            })),
            errors: None,
        };
        let val = parse_register_response(&response).unwrap();
        assert_eq!(val, 42);
    }

    #[test]
    fn test_parse_memory_response() {
        let response = GraphQLResponse {
            data: Some(serde_json::json!({
                "memory": "0xdeadbeef"
            })),
            errors: None,
        };
        let bytes = parse_memory_response(&response).unwrap();
        assert_eq!(bytes, vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn test_hex_decode() {
        assert_eq!(hex_decode("deadbeef"), Some(vec![0xde, 0xad, 0xbe, 0xef]));
        assert_eq!(hex_decode("0xdeadbeef"), Some(vec![0xde, 0xad, 0xbe, 0xef]));
        assert_eq!(hex_decode("00ff"), Some(vec![0x00, 0xff]));
        assert_eq!(hex_decode(""), Some(vec![]));
        assert_eq!(hex_decode("0"), None); // odd length
    }

    #[test]
    fn test_graphql_error_display() {
        let err = GraphQLError {
            message: "session not found".to_string(),
            locations: None,
            path: None,
        };
        assert_eq!(format!("{err}"), "session not found");
    }

    #[test]
    fn test_parse_error_response() {
        let response = GraphQLResponse {
            data: None,
            errors: Some(vec![GraphQLError {
                message: "not found".to_string(),
                locations: None,
                path: None,
            }]),
        };
        assert!(parse_start_session_response(&response).is_none());
    }
}
