//! FuelVM interpreter wrapper with single-stepping support.
//!
//! Wraps fuel-vm's `Interpreter` to provide single-step execution with
//! trace event collection via a callback.

use eyre::{Result, eyre};
use fuel_asm::{Instruction, RawInstruction, RegId};
use fuel_tx::{ConsensusParameters, Receipt, TransactionBuilder};
use fuel_vm::interpreter::{Interpreter, Memory, NotSupportedEcal};
use fuel_vm::prelude::*;

/// State captured at each single-step of FuelVM execution.
pub struct StepState {
    /// Program counter value (relative to script start).
    pub pc: u64,
    /// Copy of all 64 registers.
    pub registers: Vec<u64>,
    /// Receipts accumulated so far.
    pub receipts: Vec<Receipt>,
    /// The current instruction at this step (if readable).
    pub instruction: Option<Instruction>,
}

/// Wrapper around the FuelVM interpreter that supports single-stepping
/// execution and collecting trace events via a callback.
pub struct FuelInterpreter {
    /// The raw bytecode to execute as a script.
    bytecode: Vec<u8>,
}

/// Read the next instruction from the interpreter's memory at the current PC.
fn get_next_instruction<M, S, Tx>(
    vm: &Interpreter<M, S, Tx, NotSupportedEcal>,
) -> Option<Instruction>
where
    M: Memory,
{
    let pc = vm.registers()[RegId::PC];
    let raw = RawInstruction::from_be_bytes(vm.memory().read_bytes(pc).ok()?);
    Instruction::try_from(raw).ok()
}

impl FuelInterpreter {
    /// Create a new interpreter wrapper for the given program bytecode.
    pub fn new(bytecode: Vec<u8>) -> Result<Self> {
        if bytecode.is_empty() {
            return Err(eyre!("bytecode is empty"));
        }
        Ok(Self { bytecode })
    }

    /// Execute the program with single-stepping, calling `callback` at each step.
    ///
    /// The callback receives a `StepState` with the current execution state.
    /// Returns the total number of steps executed.
    pub fn run_with_callback<F>(&self, mut callback: F) -> Result<usize>
    where
        F: FnMut(&StepState),
    {
        let mut vm = Interpreter::<_, _, _, NotSupportedEcal>::with_memory_storage();
        vm.set_single_stepping(true);

        let consensus_params = ConsensusParameters::standard();
        let tx = TransactionBuilder::script(self.bytecode.clone(), vec![])
            .script_gas_limit(1_000_000)
            .maturity(Default::default())
            .add_fee_input()
            .finalize()
            .into_checked(Default::default(), &consensus_params)
            .map_err(|e| eyre!("failed to check tx: {e:?}"))?
            .into_ready(
                0,
                consensus_params.gas_costs(),
                consensus_params.fee_params(),
            )
            .map_err(|e| eyre!("failed to finalize tx: {e:?}"))?;

        let mut state = *vm
            .transact(tx)
            .map_err(|e| eyre!("transact failed: {e:?}"))?
            .state();

        let mut step_count = 0;

        loop {
            match state {
                ProgramState::Return(_)
                | ProgramState::ReturnData(_)
                | ProgramState::Revert(_) => {
                    break;
                }
                ProgramState::RunProgram(debug_eval) => {
                    if let DebugEval::Breakpoint(bp) = debug_eval {
                        let instruction = get_next_instruction(&vm);
                        let registers = vm.registers().to_vec();
                        let receipts = vm.receipts().to_vec();

                        let step = StepState {
                            pc: bp.pc(),
                            registers,
                            receipts,
                            instruction,
                        };

                        callback(&step);
                        step_count += 1;
                    }
                    state = vm.resume().map_err(|e| eyre!("resume failed: {e:?}"))?;
                }
                ProgramState::VerifyPredicate(_) => {
                    state = vm.resume().map_err(|e| eyre!("resume failed: {e:?}"))?;
                }
            }
        }

        Ok(step_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fuel_asm::op;

    fn simple_arithmetic_bytecode() -> Vec<u8> {
        vec![
            op::movi(0x10, 10),
            op::movi(0x11, 32),
            op::add(0x12, 0x10, 0x11),
            op::muli(0x13, 0x12, 2),
            op::add(0x14, 0x13, 0x10),
            op::log(0x14, 0x00, 0x00, 0x00),
            op::ret(RegId::ONE),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn test_basic_execution() {
        let interp = FuelInterpreter::new(simple_arithmetic_bytecode()).unwrap();
        let mut steps = Vec::new();
        let count = interp
            .run_with_callback(|step| {
                steps.push(step.pc);
            })
            .unwrap();
        assert!(count > 0, "should have at least one step");
        assert_eq!(count, steps.len());
    }

    #[test]
    fn test_register_values() {
        let interp = FuelInterpreter::new(simple_arithmetic_bytecode()).unwrap();
        let mut final_registers = Vec::new();
        interp
            .run_with_callback(|step| {
                final_registers = step.registers.clone();
            })
            .unwrap();
        // After the last step (ret), the registers should have been set.
        // r16 = 10, r17 = 32, r18 = 42, r19 = 84, r20 = 94
        assert!(
            !final_registers.is_empty(),
            "final_registers should be non-empty after execution"
        );
        assert_eq!(
            final_registers[0x10], 10,
            "r16 should be 10 (movi 0x10, 10)"
        );
        assert_eq!(
            final_registers[0x11], 32,
            "r17 should be 32 (movi 0x11, 32)"
        );
        assert_eq!(
            final_registers[0x12], 42,
            "r18 should be 42 (add 0x12, 0x10, 0x11 = 10 + 32)"
        );
        assert_eq!(
            final_registers[0x13], 84,
            "r19 should be 84 (muli 0x13, 0x12, 2 = 42 * 2)"
        );
        assert_eq!(
            final_registers[0x14], 94,
            "r20 should be 94 (add 0x14, 0x13, 0x10 = 84 + 10)"
        );
    }

    #[test]
    fn test_empty_bytecode_fails() {
        let result = FuelInterpreter::new(vec![]);
        assert!(result.is_err());
    }
}
