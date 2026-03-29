//! Sway ABI JSON decoder.
//!
//! Parses Sway ABI JSON files to extract function signatures and type
//! information. This is used by the variable tracker to enrich heuristic
//! variable names with actual parameter names from the ABI.

use eyre::Result;
use serde::Deserialize;

/// Top-level Sway ABI JSON schema.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbiSchema {
    /// The type of program (e.g. "script", "contract", "predicate").
    #[serde(default)]
    pub program_type: String,
    /// Function definitions.
    #[serde(default)]
    pub functions: Vec<AbiFunction>,
    /// Type definitions (not directly used yet, but preserved).
    #[serde(default)]
    pub types: Vec<serde_json::Value>,
}

/// A function defined in the ABI.
#[derive(Debug, Clone, Deserialize)]
pub struct AbiFunction {
    /// Function name.
    pub name: String,
    /// Input parameters.
    #[serde(default)]
    pub inputs: Vec<AbiParam>,
    /// Output type.
    pub output: Option<AbiParam>,
}

/// A parameter or output type in the ABI.
#[derive(Debug, Clone, Deserialize)]
pub struct AbiParam {
    /// Parameter name (empty string for outputs).
    #[serde(default)]
    pub name: String,
    /// Type name (e.g. "u64", "bool", "struct MyStruct").
    #[serde(rename = "type")]
    pub type_name: String,
}

impl AbiSchema {
    /// Parse an ABI schema from a JSON string.
    pub fn from_json(json: &str) -> Result<Self> {
        let schema: AbiSchema =
            serde_json::from_str(json).map_err(|e| eyre::eyre!("failed to parse ABI JSON: {e}"))?;
        Ok(schema)
    }

    /// Get the parameter names and types for a function.
    ///
    /// Returns a list of (name, type) pairs in parameter order.
    pub fn function_params(&self, fn_name: &str) -> Vec<(String, String)> {
        self.functions
            .iter()
            .find(|f| f.name == fn_name)
            .map(|f| {
                f.inputs
                    .iter()
                    .map(|p| (p.name.clone(), p.type_name.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all function names in the ABI.
    pub fn function_names(&self) -> Vec<&str> {
        self.functions.iter().map(|f| f.name.as_str()).collect()
    }
}
