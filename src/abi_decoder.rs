//! Sway ABI JSON decoder.
//!
//! Parses Sway ABI JSON files to extract function signatures and type
//! information. This is used by the variable tracker to enrich heuristic
//! variable names with actual parameter names from the ABI.

use eyre::Result;

/// Top-level Sway ABI JSON schema.
#[derive(Debug, Clone)]
pub struct AbiSchema {
    /// The type of program (e.g. "script", "contract", "predicate").
    pub program_type: String,
    /// Function definitions.
    pub functions: Vec<AbiFunction>,
    /// Type definitions (not directly used yet, but preserved).
    pub types: Vec<serde_json::Value>,
}

/// A function defined in the ABI.
#[derive(Debug, Clone)]
pub struct AbiFunction {
    /// Function name.
    pub name: String,
    /// Input parameters.
    pub inputs: Vec<AbiParam>,
    /// Output type.
    pub output: Option<AbiParam>,
}

/// A parameter or output type in the ABI.
#[derive(Debug, Clone)]
pub struct AbiParam {
    /// Parameter name (empty string for outputs).
    pub name: String,
    /// Type name (e.g. "u64", "bool", "struct MyStruct").
    pub type_name: String,
}

impl AbiSchema {
    /// Parse an ABI schema from a JSON string.
    ///
    /// Accepts both the ABI JSON forc 0.70 writes (spec version 1.x, where
    /// parameter and output types are `concreteTypeId` references into
    /// `concreteTypes`) and the older shape in which each parameter and the
    /// output carry a `type` of their own (a type name, or an index into
    /// `types`). Either way, types resolve to their Sway spelling
    /// (e.g. `u64`, `struct Point`).
    pub fn from_json(json: &str) -> Result<Self> {
        let doc: serde_json::Value =
            serde_json::from_str(json).map_err(|e| eyre::eyre!("failed to parse ABI JSON: {e}"))?;
        let concrete: std::collections::HashMap<&str, &str> = doc["concreteTypes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| Some((t["concreteTypeId"].as_str()?, t["type"].as_str()?)))
            .collect();
        let indexed: std::collections::HashMap<u64, &str> = doc["types"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| Some((t["typeId"].as_u64()?, t["type"].as_str()?)))
            .collect();

        let type_name = |v: &serde_json::Value, what: &str| -> Result<String> {
            if let Some(id) = v.as_str().filter(|id| concrete.contains_key(id)) {
                return Ok(concrete[id].to_string());
            }
            if let Some(id) = v["concreteTypeId"].as_str() {
                return concrete
                    .get(id)
                    .map(|t| t.to_string())
                    .ok_or_else(|| eyre::eyre!("{what}: unknown concreteTypeId {id}"));
            }
            match &v["type"] {
                serde_json::Value::String(t) => Ok(t.clone()),
                serde_json::Value::Number(n) => n
                    .as_u64()
                    .and_then(|i| indexed.get(&i))
                    .map(|t| t.to_string())
                    .ok_or_else(|| eyre::eyre!("{what}: unknown type index {n}")),
                _ => Err(eyre::eyre!("{what}: no type in {v}")),
            }
        };

        let mut functions = Vec::new();
        for f in doc["functions"].as_array().into_iter().flatten() {
            let name = f["name"]
                .as_str()
                .ok_or_else(|| eyre::eyre!("failed to parse ABI JSON: function without a name"))?
                .to_string();
            let mut inputs = Vec::new();
            for input in f["inputs"].as_array().into_iter().flatten() {
                inputs.push(AbiParam {
                    name: input["name"].as_str().unwrap_or_default().to_string(),
                    type_name: type_name(input, &format!("input of `{name}`"))?,
                });
            }
            let output = match &f["output"] {
                serde_json::Value::Null => None,
                out => Some(AbiParam {
                    name: out["name"].as_str().unwrap_or_default().to_string(),
                    type_name: type_name(out, &format!("output of `{name}`"))?,
                }),
            };
            functions.push(AbiFunction {
                name,
                inputs,
                output,
            });
        }

        Ok(AbiSchema {
            program_type: doc["programType"].as_str().unwrap_or_default().to_string(),
            functions,
            types: doc["types"]
                .as_array()
                .or_else(|| doc["concreteTypes"].as_array())
                .cloned()
                .unwrap_or_default(),
        })
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

    /// Get the output type name for a function, if defined.
    ///
    /// Used by the recorder to drive ABI-aware output decoders — e.g.
    /// when `output.type` is `(u64, b256, bool)` the recorder emits a
    /// `ValueRecord::Tuple` step variable on every LOGD step; when it
    /// is `enum Outcome` the recorder emits a `ValueRecord::Variant`
    /// with the discriminator + decoded inner contents.  See
    /// `src/recorder.rs::record` for the dispatch logic and
    /// `tests/test_tracer.rs::test_tuple_decoding_test_via_ct_print_full`
    /// /
    /// `tests/test_tracer.rs::test_enum_tagged_union_test_via_ct_print_full`
    /// for the regression pins.
    pub fn function_output_type(&self, fn_name: &str) -> Option<&str> {
        self.functions
            .iter()
            .find(|f| f.name == fn_name)
            .and_then(|f| f.output.as_ref())
            .map(|o| o.type_name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ABI JSON forc 0.70.3 writes for a script `fn main(x: u64) -> u64`.
    const FORC_0_70_ABI: &str = r#"{
      "programType": "script",
      "specVersion": "1.2",
      "encodingVersion": "1",
      "concreteTypes": [
        {"type": "()", "concreteTypeId": "2e38e77b22c314a449e91fafed92a43826ac6aa403ae6a8acb6cf58239fbaf5d"},
        {"type": "u64", "concreteTypeId": "1506e6f44c1d6291cdf46395a8e573276a4fa79e8ace3fc891e092ef32d1b0a0"}
      ],
      "metadataTypes": [],
      "functions": [
        {"name": "main",
         "inputs": [{"name": "x", "concreteTypeId": "1506e6f44c1d6291cdf46395a8e573276a4fa79e8ace3fc891e092ef32d1b0a0"}],
         "output": "1506e6f44c1d6291cdf46395a8e573276a4fa79e8ace3fc891e092ef32d1b0a0",
         "attributes": null}
      ],
      "loggedTypes": [],
      "messagesTypes": [],
      "configurables": []
    }"#;

    #[test]
    fn parses_forc_0_70_abi() {
        let abi = AbiSchema::from_json(FORC_0_70_ABI).expect("forc 0.70 ABI parses");
        assert_eq!(abi.program_type, "script");
        assert_eq!(abi.function_names(), vec!["main"]);
        assert_eq!(
            abi.function_params("main"),
            vec![("x".to_string(), "u64".to_string())]
        );
        assert_eq!(abi.function_output_type("main"), Some("u64"));
    }

    #[test]
    fn parses_type_name_abi() {
        let json = r#"{"programType": "script", "functions": [{"name": "main",
            "inputs": [{"name": "a", "type": "u64"}], "output": {"name": "", "type": "(u64, b256, bool)"}}]}"#;
        let abi = AbiSchema::from_json(json).unwrap();
        assert_eq!(
            abi.function_params("main"),
            vec![("a".to_string(), "u64".to_string())]
        );
        assert_eq!(abi.function_output_type("main"), Some("(u64, b256, bool)"));
    }

    #[test]
    fn parses_type_index_abi() {
        let json = r#"{"types": [{"typeId": 0, "type": "bool"}, {"typeId": 1, "type": "u64"}],
            "functions": [{"name": "main", "inputs": [{"name": "n", "type": 1}], "output": {"name": "", "type": 0}}]}"#;
        let abi = AbiSchema::from_json(json).unwrap();
        assert_eq!(
            abi.function_params("main"),
            vec![("n".to_string(), "u64".to_string())]
        );
        assert_eq!(abi.function_output_type("main"), Some("bool"));
    }

    #[test]
    fn rejects_unknown_type_reference() {
        let json = r#"{"concreteTypes": [], "functions": [{"name": "main", "inputs": [],
            "output": {"concreteTypeId": "dead"}}]}"#;
        assert!(AbiSchema::from_json(json).is_err());
    }
}
