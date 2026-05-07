//! FuelVM execution trace recorder.
//!
//! Processes FuelVM single-step events and produces CodeTracer trace output
//! using the TraceWriter API.

use std::path::{Path, PathBuf};

use codetracer_trace_types::{EventLogKind, Line, TypeKind, ValueRecord, NONE_VALUE};
use codetracer_trace_writer_nim::trace_writer::TraceWriter;
use codetracer_trace_writer_nim::{create_trace_writer, TraceEventsFileFormat};
use eyre::{Context, Result};
use fuel_tx::Receipt;

use crate::abi_decoder::AbiSchema;
use crate::contract_call::ContractCallTracker;
use crate::interpreter::FuelInterpreter;
use crate::source_map::SwaySourceMap;
use crate::variable_tracker::VariableTracker;

/// The main recorder that processes FuelVM execution events into CodeTracer
/// trace format.
///
/// The recorder is hard-pinned to the canonical CodeTracer multi-stream
/// CTFS container — see `Recorder-CLI-Conventions.md` §4 in
/// `codetracer-specs`.  Human-readable conversion is delegated to
/// `ct print` (shipped with `codetracer-trace-format-nim`).
pub struct FuelRecorder {
    /// Name of the program being recorded.
    pub program_name: String,
    /// Output directory for trace files.
    pub trace_dir: PathBuf,
    /// Optional ABI schema for variable name enrichment.
    pub abi: Option<AbiSchema>,
}

impl FuelRecorder {
    /// Create a new FuelRecorder.  The writer is always CTFS; there is no
    /// legacy-format escape hatch.
    pub fn new(program_name: &str, trace_dir: &Path) -> Self {
        Self {
            program_name: program_name.to_string(),
            trace_dir: trace_dir.to_path_buf(),
            abi: None,
        }
    }

    /// Create a new FuelRecorder with ABI information for variable
    /// enrichment.  The writer is always CTFS.
    pub fn with_abi(program_name: &str, trace_dir: &Path, abi: AbiSchema) -> Self {
        Self {
            program_name: program_name.to_string(),
            trace_dir: trace_dir.to_path_buf(),
            abi: Some(abi),
        }
    }

    /// Record a FuelVM execution trace.
    ///
    /// Takes bytecode and an optional source map, executes the bytecode with
    /// single-stepping, and writes a CodeTracer CTFS trace bundle.
    pub fn record(
        &self,
        bytecode: Vec<u8>,
        source_map: &SwaySourceMap,
        source_path: &Path,
    ) -> Result<()> {
        // Create trace writer (CTFS only — convention §4).
        let mut writer = create_trace_writer(&self.program_name, &[], TraceEventsFileFormat::Ctfs);

        // Create output directory
        std::fs::create_dir_all(&self.trace_dir)
            .with_context(|| format!("cannot create output dir: {}", self.trace_dir.display()))?;

        // CTFS multi-stream container — `db-backend` infers the format
        // from the `.bin` extension.  No JSON / legacy-binary alternative
        // is exposed.
        let events_path = self.trace_dir.join("trace.bin");
        let metadata_path = self.trace_dir.join("trace_metadata.json");
        let paths_path = self.trace_dir.join("trace_paths.json");

        // Initialize trace files
        TraceWriter::begin_writing_trace_events(&mut *writer, &events_path)
            .map_err(|e| eyre::eyre!("{e}"))?;
        TraceWriter::begin_writing_trace_metadata(&mut *writer, &metadata_path)
            .map_err(|e| eyre::eyre!("{e}"))?;
        TraceWriter::begin_writing_trace_paths(&mut *writer, &paths_path)
            .map_err(|e| eyre::eyre!("{e}"))?;

        // Start the trace
        TraceWriter::start(&mut *writer, source_path, Line(1));

        // Register the "u64" type (after start, so that "None" gets TypeId(0))
        let u64_type_id = TraceWriter::ensure_type_id(&mut *writer, TypeKind::Int, "u64");

        // Register a main function
        let main_fn_id =
            TraceWriter::ensure_function_id(&mut *writer, "main", source_path, Line(1));
        // Merge main into <toplevel>: skip the Call event so that all steps
        // remain at depth 0. TraceWriter::start() already opened <toplevel>.
        // Emitting register_call here would push the body to depth 1, causing
        // step-over from the initial position to skip the entire body.
        let _ = main_fn_id;

        // Set up variable tracker
        let mut tracker = VariableTracker::new();
        if let Some(abi) = &self.abi {
            tracker.set_abi(abi, "main");
        }

        // Set up contract call tracker
        let mut call_tracker = ContractCallTracker::new();

        // Create interpreter and run with single-stepping
        let interp = FuelInterpreter::new(bytecode)?;

        let mut prev_line: Option<u32> = None;
        // Index of the first receipt that has not yet been mirrored as a
        // structured `register_special_event` record.  The contract-call
        // tracker maintains its own counter for Call/Return detection; we
        // need a parallel one to route every other receipt kind (Log,
        // LogData, Mint, Burn, Transfer, TransferOut, MessageOut, Panic,
        // Revert, ScriptResult) into the canonical event stream.
        let mut prev_receipt_count: usize = 0;

        interp.run_with_callback(|step| {
            // Calculate the opcode index from PC (each instruction is 4 bytes)
            let opcode_index = (step.pc / 4) as usize;

            // Process receipts through contract call tracker to detect context switches
            let switches = call_tracker.process_receipts(&step.receipts);

            // Emit Call/Return events for contract switches
            for switch in &switches {
                match switch {
                    crate::contract_call::ContractSwitch::Enter { to, .. } => {
                        let contract_name = format!("contract:{}", to);
                        let switch_path = call_tracker.current_source_path(source_path);
                        let fn_id = TraceWriter::ensure_function_id(
                            &mut *writer,
                            &contract_name,
                            switch_path,
                            Line(1),
                        );
                        TraceWriter::register_call(&mut *writer, fn_id, vec![]);
                    }
                    crate::contract_call::ContractSwitch::Exit { .. } => {
                        TraceWriter::register_return(&mut *writer, NONE_VALUE);
                    }
                }
            }

            // Route every other new receipt into the structured event
            // stream via `register_special_event`.  The Call / Return /
            // ReturnData receipts are already covered by the
            // `ContractCallTracker` switch handling above; everything else
            // is mirrored here so that the FuelVM-level effects (LOG opcode
            // output, asset Mint/Burn/Transfer, cross-chain MessageOut,
            // Panic / Revert traps, final ScriptResult) survive into the
            // CodeTracer event log.  Mirrors the EVM-recorder LOG-opcode
            // routing (1.39), the Cairo StarknetEvent routing (1.50) and
            // the Flow Cadence resource-lifecycle routing (1.52).
            let new_receipts = &step.receipts[prev_receipt_count..];
            for receipt in new_receipts {
                emit_receipt_special_event(&mut *writer, receipt);
            }
            prev_receipt_count = step.receipts.len();

            // Look up source location using contract-aware tracker
            let (lookup_path, line) =
                call_tracker.lookup_source(opcode_index, source_map, source_path);
            let step_path = lookup_path.to_path_buf();

            // Emit step if line changed
            if prev_line != Some(line) {
                TraceWriter::register_step(&mut *writer, &step_path, Line(line as i64));
                prev_line = Some(line);
            }

            // Process step through variable tracker
            let tracked_vars = tracker.process_step(step);

            // Emit register values for the general-purpose registers r16-r23
            for reg_idx in 0x10..=0x17 {
                let reg_val = step.registers[reg_idx];

                // Use tracked variable name if available, otherwise fall
                // back to the raw register name.
                let name =
                    if let Some(tracked) = tracked_vars.iter().find(|v| v.register == reg_idx) {
                        tracked.name.clone()
                    } else if let Some(inferred) = tracker.get_name(reg_idx) {
                        inferred.to_string()
                    } else {
                        format!("r{}", reg_idx)
                    };

                let value = ValueRecord::Int {
                    i: reg_val as i64,
                    type_id: u64_type_id,
                };
                TraceWriter::register_variable_with_full_value(&mut *writer, &name, value);
            }
        })?;

        // Close the <toplevel> call that start() opened. main was merged into
        // <toplevel> (no Call event), so only one Return is needed.
        TraceWriter::register_return(&mut *writer, NONE_VALUE);

        // Finish writing
        TraceWriter::finish_writing_trace_events(&mut *writer).map_err(|e| eyre::eyre!("{e}"))?;
        TraceWriter::finish_writing_trace_metadata(&mut *writer).map_err(|e| eyre::eyre!("{e}"))?;
        TraceWriter::finish_writing_trace_paths(&mut *writer).map_err(|e| eyre::eyre!("{e}"))?;
        writer.close().map_err(|e| eyre::eyre!("{e}"))?;

        Ok(())
    }
}

/// Map a FuelVM `Receipt` into a single canonical
/// `register_special_event` record.
///
/// The receipt enum is the FuelVM equivalent of EVM logs / Cairo Starknet
/// events / Cadence resource events: it carries every observable side-effect
/// of script and contract execution.  Each variant is routed onto the
/// CodeTracer event stream:
///
/// - **`Log` / `LogData`** — the FuelVM `LOG` and `LOGD` opcodes.  Routed
///   through [`EventLogKind::EvmEvent`] so the multi-stream IO writer
///   surfaces them in the structured-event channel rather than mixing them
///   with stdout `Write` records.  Mirrors the EVM 1.39 LOG-opcode routing
///   and the Cairo 1.50 `StarknetEvent` routing.
///
/// - **`Mint` / `Burn`** — native asset supply changes.  Routed through
///   [`EventLogKind::EvmEvent`] (asset accounting is a structured chain
///   effect, not script-side stdout).
///
/// - **`Transfer` / `TransferOut`** — value transfers between contracts and
///   to off-chain addresses.  Routed through [`EventLogKind::EvmEvent`].
///
/// - **`MessageOut`** — cross-chain message emitted to layer-1.  Routed
///   through [`EventLogKind::EvmEvent`].
///
/// - **`Panic` / `Revert`** — script execution traps.  Routed through
///   [`EventLogKind::Error`] so the frontend's error channel surfaces them
///   as failures.  Mirror of the Cairo 1.50 `CairoPanic` and Cardano 1.48
///   `AikenUplcEvalError` routing.
///
/// - **`ScriptResult`** — final execution outcome.  Routed through
///   [`EventLogKind::TraceLogEvent`] (informational; Success cases should
///   not appear in the error channel).
///
/// `Call` / `Return` / `ReturnData` are intentionally NOT mirrored here —
/// they are already covered by the [`ContractCallTracker::process_receipts`]
/// path which emits canonical `register_call` / `register_return` records.
fn emit_receipt_special_event(writer: &mut dyn TraceWriter, receipt: &Receipt) {
    match receipt {
        // Already handled as register_call / register_return.
        Receipt::Call { .. } | Receipt::Return { .. } | Receipt::ReturnData { .. } => {}

        Receipt::Log { id, ra, rb, rc, rd, pc, .. } => {
            TraceWriter::register_special_event(
                writer,
                EventLogKind::EvmEvent,
                &format!("FuelLog:{}", truncate_id(&format!("{id:#x}"))),
                &format!("ra={ra} rb={rb} rc={rc} rd={rd} pc={pc:#x}"),
            );
        }

        Receipt::LogData { id, ra, rb, len, digest, pc, data, .. } => {
            // Inline up to 64 bytes of payload as hex; fall back to digest
            // if the FuelVM did not preserve the data buffer.  Keeping the
            // payload short bounds the .ct container size for log-heavy
            // traces.
            let payload = match data {
                Some(bytes) if !bytes.is_empty() => {
                    let n = bytes.len().min(64);
                    let hex: String =
                        bytes[..n].iter().map(|b| format!("{b:02x}")).collect();
                    if bytes.len() > n {
                        format!("data=0x{hex}... ({} bytes)", bytes.len())
                    } else {
                        format!("data=0x{hex}")
                    }
                }
                _ => format!("digest={digest:#x}"),
            };
            TraceWriter::register_special_event(
                writer,
                EventLogKind::EvmEvent,
                &format!("FuelLogData:{}", truncate_id(&format!("{id:#x}"))),
                &format!("ra={ra} rb={rb} len={len} pc={pc:#x} {payload}"),
            );
        }

        Receipt::Mint { sub_id, contract_id, val, pc, .. } => {
            TraceWriter::register_special_event(
                writer,
                EventLogKind::EvmEvent,
                &format!("FuelMint:{}", truncate_id(&format!("{contract_id:#x}"))),
                &format!("sub_id={sub_id:#x} val={val} pc={pc:#x}"),
            );
        }

        Receipt::Burn { sub_id, contract_id, val, pc, .. } => {
            TraceWriter::register_special_event(
                writer,
                EventLogKind::EvmEvent,
                &format!("FuelBurn:{}", truncate_id(&format!("{contract_id:#x}"))),
                &format!("sub_id={sub_id:#x} val={val} pc={pc:#x}"),
            );
        }

        Receipt::Transfer { id, to, amount, asset_id, pc, .. } => {
            TraceWriter::register_special_event(
                writer,
                EventLogKind::EvmEvent,
                &format!("FuelTransfer:{}", truncate_id(&format!("{id:#x}"))),
                &format!(
                    "to={} amount={amount} asset_id={asset_id:#x} pc={pc:#x}",
                    truncate_id(&format!("{to:#x}"))
                ),
            );
        }

        Receipt::TransferOut { id, to, amount, asset_id, pc, .. } => {
            TraceWriter::register_special_event(
                writer,
                EventLogKind::EvmEvent,
                &format!("FuelTransferOut:{}", truncate_id(&format!("{id:#x}"))),
                &format!(
                    "to={} amount={amount} asset_id={asset_id:#x} pc={pc:#x}",
                    truncate_id(&format!("{to:#x}"))
                ),
            );
        }

        Receipt::MessageOut { sender, recipient, amount, nonce, len, digest, .. } => {
            TraceWriter::register_special_event(
                writer,
                EventLogKind::EvmEvent,
                &format!(
                    "FuelMessageOut:{}",
                    truncate_id(&format!("{sender:#x}"))
                ),
                &format!(
                    "recipient={} amount={amount} nonce={nonce:#x} len={len} digest={digest:#x}",
                    truncate_id(&format!("{recipient:#x}"))
                ),
            );
        }

        Receipt::Panic { id, reason, pc, .. } => {
            TraceWriter::register_special_event(
                writer,
                EventLogKind::Error,
                "FuelPanic",
                &format!(
                    "contract={} reason={reason:?} pc={pc:#x}",
                    truncate_id(&format!("{id:#x}"))
                ),
            );
        }

        Receipt::Revert { id, ra, pc, .. } => {
            TraceWriter::register_special_event(
                writer,
                EventLogKind::Error,
                "FuelRevert",
                &format!(
                    "contract={} code={ra} pc={pc:#x}",
                    truncate_id(&format!("{id:#x}"))
                ),
            );
        }

        Receipt::ScriptResult { result, gas_used } => {
            TraceWriter::register_special_event(
                writer,
                EventLogKind::TraceLogEvent,
                "FuelScriptResult",
                &format!("result={result:?} gas_used={gas_used}"),
            );
        }
    }
}

/// Truncate a long hex-formatted identifier (`0x…`) to the leading 10 chars
/// + `…` so the `metadata` slot of a `register_special_event` record stays
/// short.
///
/// Mirrors the `truncate_contract_id` helper used elsewhere in the
/// recorder for switch-event display names.
fn truncate_id(hex: &str) -> String {
    if hex.len() > 10 {
        format!("{}...", &hex[..10])
    } else {
        hex.to_string()
    }
}
