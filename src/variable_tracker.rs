//! Heuristic variable recovery from FuelVM register patterns.
//!
//! Since Sway does not yet emit DWARF-style debug info for local variables
//! (see sway#2055), this module infers variable names from instruction
//! patterns:
//!
//! - `MOVI rX, imm` — immediate load, likely a variable initialization
//! - `ADD rX, rY, rZ` — addition of two tracked registers
//! - `MUL rX, rY, rZ` — multiplication of two tracked registers
//! - `SUB rX, rY, rZ` — subtraction of two tracked registers
//! - `MULI rX, rY, imm` — multiply-immediate
//! - `ADDI rX, rY, imm` — add-immediate
//! - `SUBI rX, rY, imm` — subtract-immediate
//!
//! When an ABI is provided, the first N MOVI instructions into general-purpose
//! registers (r16+) are matched against function parameter names.

use std::collections::HashMap;

use fuel_asm::Instruction;

use crate::abi_decoder::AbiSchema;
use crate::interpreter::StepState;

/// A variable inferred from register tracking.
#[derive(Debug, Clone)]
pub struct TrackedVariable {
    /// Inferred variable name.
    pub name: String,
    /// Current value.
    pub value: u64,
    /// Register index (0-63).
    pub register: usize,
}

/// Tracks register assignments across steps to infer variable names.
pub struct VariableTracker {
    /// Map from register index to inferred name.
    register_names: HashMap<usize, String>,
    /// ABI parameter names for enrichment, keyed by parameter position.
    abi_params: Vec<(String, String)>,
    /// Count of MOVI instructions seen into general-purpose registers,
    /// used to match against ABI parameter positions.
    movi_count: usize,
}

impl VariableTracker {
    /// Create a new variable tracker with no ABI information.
    pub fn new() -> Self {
        Self {
            register_names: HashMap::new(),
            abi_params: Vec::new(),
            movi_count: 0,
        }
    }

    /// Enrich variable names using ABI function parameter information.
    ///
    /// Call this before processing steps. The parameter names from the
    /// specified function will be used to name the first N MOVI loads
    /// into general-purpose registers.
    pub fn set_abi(&mut self, abi: &AbiSchema, fn_name: &str) {
        self.abi_params = abi.function_params(fn_name);
    }

    /// Process a single execution step and return any tracked variables.
    ///
    /// Examines the current instruction to detect register assignments
    /// and returns the list of variables with inferred names.
    pub fn process_step(&mut self, step: &StepState) -> Vec<TrackedVariable> {
        let instr = match &step.instruction {
            Some(i) => i,
            None => return Vec::new(),
        };

        self.track_instruction(instr, &step.registers)
    }

    /// Analyze an instruction and update tracking state.
    fn track_instruction(
        &mut self,
        instr: &Instruction,
        registers: &[u64],
    ) -> Vec<TrackedVariable> {
        match instr {
            Instruction::MOVI(movi) => {
                let (dst, imm) = movi.unpack();
                let dst_idx = usize::from(dst);
                if dst_idx < 16 {
                    // Reserved registers, skip
                    return Vec::new();
                }
                let imm_val: u32 = imm.into();

                // Try to get name from ABI params
                let name = if self.movi_count < self.abi_params.len() {
                    self.abi_params[self.movi_count].0.clone()
                } else {
                    format!("imm_{}", imm_val)
                };
                self.movi_count += 1;

                self.register_names.insert(dst_idx, name.clone());
                vec![TrackedVariable {
                    name,
                    value: imm_val as u64,
                    register: dst_idx,
                }]
            }

            Instruction::ADD(add) => {
                let (dst, lhs, rhs) = add.unpack();
                let dst_idx = usize::from(dst);
                let lhs_idx = usize::from(lhs);
                let rhs_idx = usize::from(rhs);
                if dst_idx < 16 {
                    return Vec::new();
                }

                let lhs_name = self.register_name(lhs_idx);
                let rhs_name = self.register_name(rhs_idx);
                let name = format!("{}_plus_{}", lhs_name, rhs_name);

                self.register_names.insert(dst_idx, name.clone());
                let value = registers.get(dst_idx).copied().unwrap_or(0);
                vec![TrackedVariable {
                    name,
                    value,
                    register: dst_idx,
                }]
            }

            Instruction::SUB(sub) => {
                let (dst, lhs, rhs) = sub.unpack();
                let dst_idx = usize::from(dst);
                let lhs_idx = usize::from(lhs);
                let rhs_idx = usize::from(rhs);
                if dst_idx < 16 {
                    return Vec::new();
                }

                let lhs_name = self.register_name(lhs_idx);
                let rhs_name = self.register_name(rhs_idx);
                let name = format!("{}_minus_{}", lhs_name, rhs_name);

                self.register_names.insert(dst_idx, name.clone());
                let value = registers.get(dst_idx).copied().unwrap_or(0);
                vec![TrackedVariable {
                    name,
                    value,
                    register: dst_idx,
                }]
            }

            Instruction::MUL(mul) => {
                let (dst, lhs, rhs) = mul.unpack();
                let dst_idx = usize::from(dst);
                let lhs_idx = usize::from(lhs);
                let rhs_idx = usize::from(rhs);
                if dst_idx < 16 {
                    return Vec::new();
                }

                let lhs_name = self.register_name(lhs_idx);
                let rhs_name = self.register_name(rhs_idx);
                let name = format!("{}_times_{}", lhs_name, rhs_name);

                self.register_names.insert(dst_idx, name.clone());
                let value = registers.get(dst_idx).copied().unwrap_or(0);
                vec![TrackedVariable {
                    name,
                    value,
                    register: dst_idx,
                }]
            }

            Instruction::MULI(muli) => {
                let (dst, lhs, imm) = muli.unpack();
                let dst_idx = usize::from(dst);
                let lhs_idx = usize::from(lhs);
                if dst_idx < 16 {
                    return Vec::new();
                }
                let imm_val: u32 = imm.into();

                let lhs_name = self.register_name(lhs_idx);
                let name = format!("{}_times_{}", lhs_name, imm_val);

                self.register_names.insert(dst_idx, name.clone());
                let value = registers.get(dst_idx).copied().unwrap_or(0);
                vec![TrackedVariable {
                    name,
                    value,
                    register: dst_idx,
                }]
            }

            Instruction::ADDI(addi) => {
                let (dst, lhs, imm) = addi.unpack();
                let dst_idx = usize::from(dst);
                let lhs_idx = usize::from(lhs);
                if dst_idx < 16 {
                    return Vec::new();
                }
                let imm_val: u32 = imm.into();

                let lhs_name = self.register_name(lhs_idx);
                let name = format!("{}_plus_{}", lhs_name, imm_val);

                self.register_names.insert(dst_idx, name.clone());
                let value = registers.get(dst_idx).copied().unwrap_or(0);
                vec![TrackedVariable {
                    name,
                    value,
                    register: dst_idx,
                }]
            }

            Instruction::SUBI(subi) => {
                let (dst, lhs, imm) = subi.unpack();
                let dst_idx = usize::from(dst);
                let lhs_idx = usize::from(lhs);
                if dst_idx < 16 {
                    return Vec::new();
                }
                let imm_val: u32 = imm.into();

                let lhs_name = self.register_name(lhs_idx);
                let name = format!("{}_minus_{}", lhs_name, imm_val);

                self.register_names.insert(dst_idx, name.clone());
                let value = registers.get(dst_idx).copied().unwrap_or(0);
                vec![TrackedVariable {
                    name,
                    value,
                    register: dst_idx,
                }]
            }

            _ => Vec::new(),
        }
    }

    /// Get the inferred name for a register, or fall back to `rN`.
    fn register_name(&self, reg_idx: usize) -> String {
        self.register_names
            .get(&reg_idx)
            .cloned()
            .unwrap_or_else(|| format!("r{}", reg_idx))
    }

    /// Get the current inferred name for a register, if any.
    pub fn get_name(&self, reg_idx: usize) -> Option<&str> {
        self.register_names.get(&reg_idx).map(|s| s.as_str())
    }
}

impl Default for VariableTracker {
    fn default() -> Self {
        Self::new()
    }
}
