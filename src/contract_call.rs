//! Cross-contract call tracking for FuelVM execution.
//!
//! Tracks ContractId context switches during execution, maintaining a call
//! stack of contract contexts. Each contract can have its own source map,
//! enabling correct source-level debugging across cross-contract calls.
//!
//! ## How it works
//!
//! FuelVM emits `Receipt::Call` when a contract calls another contract. The
//! receipt contains:
//! - `id`: the calling contract's ContractId
//! - `to`: the called contract's ContractId
//!
//! `Receipt::Return` / `Receipt::ReturnData` indicate returning from a call.
//!
//! This module maintains a stack of `ContractContext` entries. When a
//! `Receipt::Call` is seen, a new context is pushed. When `Return` /
//! `ReturnData` is seen, the top context is popped.
//!
//! ## Predicate execution
//!
//! Predicates run in a restricted context (no storage writes, no contract
//! calls). The tracker detects predicate execution via `Receipt::Call` where
//! the `id` field is the zero ContractId (predicates don't have a contract
//! ID) or via explicit `ProgramState::VerifyPredicate`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fuel_tx::Receipt;
use fuel_types::ContractId;

use crate::source_map::SwaySourceMap;

/// Execution context type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionContext {
    /// A script is the entry point transaction type.
    Script,
    /// Execution is inside a contract call.
    Contract,
    /// Predicate verification context (restricted instruction set).
    Predicate,
}

/// A single entry in the contract call stack.
#[derive(Debug, Clone)]
pub struct ContractContext {
    /// The ContractId of the currently executing contract.
    /// For scripts, this is the zero ContractId.
    pub contract_id: ContractId,
    /// The execution context type.
    pub context: ExecutionContext,
    /// Display name for this context (e.g. "script", "contract:0xabcd...").
    pub display_name: String,
}

/// Tracks cross-contract call transitions during FuelVM execution.
///
/// Maintains a call stack of contract contexts and maps each ContractId
/// to its associated source map for correct source-level debugging.
pub struct ContractCallTracker {
    /// Stack of contract contexts. The top of the stack is the current context.
    call_stack: Vec<ContractContext>,
    /// Source maps keyed by ContractId.
    source_maps: HashMap<ContractId, SwaySourceMap>,
    /// Source file paths keyed by ContractId.
    source_paths: HashMap<ContractId, PathBuf>,
    /// Number of receipts already processed (to detect new receipts).
    processed_receipt_count: usize,
    /// Whether we are currently in predicate execution.
    in_predicate: bool,
}

impl ContractCallTracker {
    /// Create a new tracker starting in script context.
    pub fn new() -> Self {
        let initial_context = ContractContext {
            contract_id: ContractId::zeroed(),
            context: ExecutionContext::Script,
            display_name: "script".to_string(),
        };
        Self {
            call_stack: vec![initial_context],
            source_maps: HashMap::new(),
            source_paths: HashMap::new(),
            processed_receipt_count: 0,
            in_predicate: false,
        }
    }

    /// Register a source map for a specific ContractId.
    ///
    /// When execution enters this contract, the tracker will use this
    /// source map for PC-to-source mapping.
    pub fn register_source_map(
        &mut self,
        contract_id: ContractId,
        source_map: SwaySourceMap,
        source_path: PathBuf,
    ) {
        self.source_maps.insert(contract_id, source_map);
        self.source_paths.insert(contract_id, source_path);
    }

    /// Get the current execution context.
    pub fn current_context(&self) -> &ContractContext {
        self.call_stack
            .last()
            .expect("call stack should never be empty")
    }

    /// Get the current ContractId.
    pub fn current_contract_id(&self) -> &ContractId {
        &self.current_context().contract_id
    }

    /// Get the current execution context type.
    pub fn current_execution_context(&self) -> ExecutionContext {
        self.current_context().context
    }

    /// Check if we are in predicate execution mode.
    pub fn is_predicate(&self) -> bool {
        self.in_predicate || self.current_context().context == ExecutionContext::Predicate
    }

    /// Get the depth of the call stack (1 = script level, 2+ = in contract calls).
    pub fn call_depth(&self) -> usize {
        self.call_stack.len()
    }

    /// Get the full call stack (bottom to top).
    pub fn call_stack(&self) -> &[ContractContext] {
        &self.call_stack
    }

    /// Look up a source location using the source map for the current contract.
    ///
    /// Falls back to the default source map if no contract-specific map is registered.
    pub fn lookup_source<'a>(
        &'a self,
        opcode_index: usize,
        default_source_map: &'a SwaySourceMap,
        default_source_path: &'a Path,
    ) -> (&'a Path, u32) {
        let contract_id = self.current_contract_id();

        // Try contract-specific source map first
        if let Some(source_map) = self.source_maps.get(contract_id)
            && let Some((path, line)) = source_map.lookup(opcode_index)
        {
            return (path, line);
        }

        // Fall back to default source map
        if let Some((path, line)) = default_source_map.lookup(opcode_index) {
            return (path, line);
        }

        // Last resort: use default path with opcode index as line
        (default_source_path, (opcode_index + 1) as u32)
    }

    /// Get the source path for the current contract context.
    pub fn current_source_path<'a>(&'a self, default_path: &'a Path) -> &'a Path {
        let contract_id = self.current_contract_id();
        self.source_paths
            .get(contract_id)
            .map(|p| p.as_path())
            .unwrap_or(default_path)
    }

    /// Enter predicate execution mode.
    pub fn enter_predicate(&mut self) {
        self.in_predicate = true;
        let ctx = ContractContext {
            contract_id: ContractId::zeroed(),
            context: ExecutionContext::Predicate,
            display_name: "predicate".to_string(),
        };
        self.call_stack.push(ctx);
    }

    /// Exit predicate execution mode.
    pub fn exit_predicate(&mut self) {
        self.in_predicate = false;
        if self
            .call_stack
            .last()
            .is_some_and(|ctx| ctx.context == ExecutionContext::Predicate)
        {
            self.call_stack.pop();
        }
    }

    /// Process new receipts from a step and return any contract switch events.
    ///
    /// Call this with the full receipts list at each step. It will only
    /// process receipts that haven't been seen before.
    ///
    /// Returns a list of `ContractSwitch` events describing context transitions.
    pub fn process_receipts(&mut self, receipts: &[Receipt]) -> Vec<ContractSwitch> {
        let mut switches = Vec::new();

        let new_receipts = &receipts[self.processed_receipt_count..];
        for receipt in new_receipts {
            match receipt {
                Receipt::Call { id, to, .. } => {
                    let from_id = *id;
                    let to_id = *to;
                    let display = format!("contract:{}", truncate_contract_id(&to_id));
                    let ctx = ContractContext {
                        contract_id: to_id,
                        context: ExecutionContext::Contract,
                        display_name: display,
                    };
                    self.call_stack.push(ctx);
                    switches.push(ContractSwitch::Enter {
                        from: from_id,
                        to: to_id,
                        depth: self.call_stack.len(),
                    });
                }
                Receipt::Return { id, .. } | Receipt::ReturnData { id, .. } => {
                    let returning_id = *id;
                    // Pop the call stack if we're returning from a contract call
                    if self.call_stack.len() > 1 {
                        self.call_stack.pop();
                        let current = self.current_contract_id().to_owned();
                        switches.push(ContractSwitch::Exit {
                            from: returning_id,
                            to: current,
                            depth: self.call_stack.len(),
                        });
                    }
                }
                _ => {}
            }
        }

        self.processed_receipt_count = receipts.len();
        switches
    }
}

impl Default for ContractCallTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// A contract context switch event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractSwitch {
    /// Entering a new contract context via a call.
    Enter {
        /// The calling contract's ID.
        from: ContractId,
        /// The called contract's ID.
        to: ContractId,
        /// New call depth after entering.
        depth: usize,
    },
    /// Exiting a contract context via return.
    Exit {
        /// The contract we're returning from.
        from: ContractId,
        /// The contract we're returning to.
        to: ContractId,
        /// New call depth after exiting.
        depth: usize,
    },
}

/// Truncate a ContractId to a short display string (first 8 hex chars).
fn truncate_contract_id(id: &ContractId) -> String {
    let hex = format!("{id:#x}");
    if hex.len() > 10 {
        format!("{}...", &hex[..10])
    } else {
        hex
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fuel_types::AssetId;

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

    #[test]
    fn test_initial_state() {
        let tracker = ContractCallTracker::new();
        assert_eq!(tracker.call_depth(), 1);
        assert_eq!(
            tracker.current_execution_context(),
            ExecutionContext::Script
        );
        assert_eq!(tracker.current_contract_id(), &ContractId::zeroed());
        assert!(!tracker.is_predicate());
    }

    #[test]
    fn test_single_contract_call() {
        let mut tracker = ContractCallTracker::new();
        let contract_a = make_contract_id(0xAA);

        let receipts = vec![make_call_receipt(ContractId::zeroed(), contract_a)];
        let switches = tracker.process_receipts(&receipts);

        assert_eq!(switches.len(), 1);
        assert_eq!(tracker.call_depth(), 2);
        assert_eq!(tracker.current_contract_id(), &contract_a);
        assert_eq!(
            tracker.current_execution_context(),
            ExecutionContext::Contract
        );

        match &switches[0] {
            ContractSwitch::Enter { from, to, depth } => {
                assert_eq!(from, &ContractId::zeroed());
                assert_eq!(to, &contract_a);
                assert_eq!(*depth, 2);
            }
            _ => panic!("expected Enter switch"),
        }
    }

    #[test]
    fn test_contract_return() {
        let mut tracker = ContractCallTracker::new();
        let contract_a = make_contract_id(0xAA);

        // Enter contract A
        let receipts = vec![make_call_receipt(ContractId::zeroed(), contract_a)];
        tracker.process_receipts(&receipts);
        assert_eq!(tracker.call_depth(), 2);

        // Return from contract A
        let mut receipts2 = receipts.clone();
        receipts2.push(make_return_receipt(contract_a));
        let switches = tracker.process_receipts(&receipts2);

        assert_eq!(switches.len(), 1);
        assert_eq!(tracker.call_depth(), 1);
        assert_eq!(tracker.current_contract_id(), &ContractId::zeroed());
        assert_eq!(
            tracker.current_execution_context(),
            ExecutionContext::Script
        );
    }

    #[test]
    fn test_nested_calls() {
        let mut tracker = ContractCallTracker::new();
        let contract_a = make_contract_id(0xAA);
        let contract_b = make_contract_id(0xBB);
        let contract_c = make_contract_id(0xCC);

        // Script -> A
        let mut receipts = vec![make_call_receipt(ContractId::zeroed(), contract_a)];
        tracker.process_receipts(&receipts);
        assert_eq!(tracker.call_depth(), 2);
        assert_eq!(tracker.current_contract_id(), &contract_a);

        // A -> B
        receipts.push(make_call_receipt(contract_a, contract_b));
        tracker.process_receipts(&receipts);
        assert_eq!(tracker.call_depth(), 3);
        assert_eq!(tracker.current_contract_id(), &contract_b);

        // B -> C
        receipts.push(make_call_receipt(contract_b, contract_c));
        tracker.process_receipts(&receipts);
        assert_eq!(tracker.call_depth(), 4);
        assert_eq!(tracker.current_contract_id(), &contract_c);

        // C returns -> back to B
        receipts.push(make_return_receipt(contract_c));
        let switches = tracker.process_receipts(&receipts);
        assert_eq!(switches.len(), 1);
        assert_eq!(tracker.call_depth(), 3);
        assert_eq!(tracker.current_contract_id(), &contract_b);

        // B returns -> back to A
        receipts.push(make_return_receipt(contract_b));
        tracker.process_receipts(&receipts);
        assert_eq!(tracker.call_depth(), 2);
        assert_eq!(tracker.current_contract_id(), &contract_a);

        // A returns -> back to script
        receipts.push(make_return_receipt(contract_a));
        tracker.process_receipts(&receipts);
        assert_eq!(tracker.call_depth(), 1);
        assert_eq!(tracker.current_contract_id(), &ContractId::zeroed());
    }

    #[test]
    fn test_predicate_context() {
        let mut tracker = ContractCallTracker::new();

        assert!(!tracker.is_predicate());

        tracker.enter_predicate();
        assert!(tracker.is_predicate());
        assert_eq!(
            tracker.current_execution_context(),
            ExecutionContext::Predicate
        );

        tracker.exit_predicate();
        assert!(!tracker.is_predicate());
        assert_eq!(
            tracker.current_execution_context(),
            ExecutionContext::Script
        );
    }

    #[test]
    fn test_source_map_per_contract() {
        let mut tracker = ContractCallTracker::new();
        let contract_a = make_contract_id(0xAA);

        // Register a source map for contract A
        let entries = vec![
            (0, PathBuf::from("contract_a.sw"), 10),
            (1, PathBuf::from("contract_a.sw"), 20),
        ];
        let source_map_a = SwaySourceMap::from_line_mapping(entries);
        tracker.register_source_map(contract_a, source_map_a, PathBuf::from("contract_a.sw"));

        // Default source map for the script
        let default_entries = vec![
            (0, PathBuf::from("script.sw"), 1),
            (1, PathBuf::from("script.sw"), 2),
        ];
        let default_map = SwaySourceMap::from_line_mapping(default_entries);
        let default_path = Path::new("script.sw");

        // Before entering contract A, should use default map
        let (path, line) = tracker.lookup_source(0, &default_map, default_path);
        assert_eq!(path, Path::new("script.sw"));
        assert_eq!(line, 1);

        // Enter contract A
        let receipts = vec![make_call_receipt(ContractId::zeroed(), contract_a)];
        tracker.process_receipts(&receipts);

        // Now should use contract A's source map
        let (path, line) = tracker.lookup_source(0, &default_map, default_path);
        assert_eq!(path, Path::new("contract_a.sw"));
        assert_eq!(line, 10);

        let (path, line) = tracker.lookup_source(1, &default_map, default_path);
        assert_eq!(path, Path::new("contract_a.sw"));
        assert_eq!(line, 20);
    }

    #[test]
    fn test_return_data_also_pops() {
        let mut tracker = ContractCallTracker::new();
        let contract_a = make_contract_id(0xAA);

        let mut receipts = vec![make_call_receipt(ContractId::zeroed(), contract_a)];
        tracker.process_receipts(&receipts);
        assert_eq!(tracker.call_depth(), 2);

        // Return with ReturnData instead of Return
        receipts.push(make_return_data_receipt(contract_a));
        tracker.process_receipts(&receipts);
        assert_eq!(tracker.call_depth(), 1);
    }
}
