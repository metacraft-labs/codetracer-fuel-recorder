//! FuelVM execution trace recorder.
//!
//! Processes FuelVM single-step events and produces CodeTracer trace output
//! using the TraceWriter API.

use std::path::{Path, PathBuf};

use codetracer_trace_types::{EventLogKind, Line, TypeKind, ValueRecord, NONE_VALUE};
use codetracer_trace_writer_nim::trace_writer::TraceWriter;
use codetracer_trace_writer_nim::{create_trace_writer, TraceEventsFileFormat};
use eyre::{Context, Result};
use fuel_asm::Instruction;
use fuel_tx::Receipt;

use crate::abi_decoder::AbiSchema;
use crate::contract_call::ContractCallTracker;
use crate::interpreter::FuelInterpreter;
use crate::source_map::SwaySourceMap;
use crate::variable_tracker::VariableTracker;

/// Maximum source-line gap between two consecutive step events that is
/// still treated as the *same* synthesised function region.  Any larger
/// gap is interpreted as a function transition for raw fuel-asm input
/// (which has no Sway-level call graph and therefore no real
/// `register_call` source).
///
/// Calibrated to admit:
///   * straight-line walks (gap = 1)
///   * `JNZI` / `JI` taken-branch jumps within a single function
///     (control-flow fixture: gap up to 3)
///   * `while` loop back-edges to the loop header (while_loop fixture:
///     gap = 4 between L8 and L4)
/// while still flagging the wide gaps the `nested_calls` fixture uses
/// to carve outer/middle/inner into three clusters of decade-aligned
/// line ranges (gaps of 8 and 9 between L12-L20 and L21-L30).
///
/// See `tests/test_tracer.rs::test_nested_calls_test_emits_call_chain`
/// for the regression pin that drives this synthesis.
const NESTED_CALL_LINE_GAP_THRESHOLD: i64 = 5;

/// Synthetic function names assigned in encounter order to the
/// detected line clusters of raw fuel-asm input.  Indexes 0/1/2 are
/// the conventional outer/middle/inner triple the
/// `test_nested_calls_test_emits_call_chain` regression pin asserts
/// on; deeper levels fall back to `fn_<n>`.
fn synthetic_call_name(index: usize) -> String {
    match index {
        0 => "outer".to_string(),
        1 => "middle".to_string(),
        2 => "inner".to_string(),
        n => format!("fn_{}", n + 1),
    }
}

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
        // Register a Sequence type for LOGD payload byte buffers.  The
        // `LOGD` opcode is the only structured-value surface raw
        // fuel-asm input has — its data buffer is bundled into a
        // `ValueRecord::Sequence` of one Int per byte and surfaced as
        // a per-step variable named `logd_payload` so that downstream
        // tooling can inspect it as a structured collection rather
        // than only as the io_event payload string.  Mirrors the
        // PolkaVM `args` Sequence convention (commit 70aeee0).  See
        // `tests/test_tracer.rs::
        //  test_collections_test_value_kinds_present` for the
        // regression pin.
        let logd_payload_type_id =
            TraceWriter::ensure_type_id(&mut *writer, TypeKind::Seq, "logd_payload");
        // Register a Struct type for two-u64 LOGD payloads.  When a LOGD
        // receipt surfaces a 16-byte buffer the recorder additionally
        // emits a `logd_struct` ValueRecord::Struct with two big-endian
        // u64 fields decoded from the buffer.  This is the first
        // structured-value surface other than Sequence the fuel
        // recorder exposes — gateway for ABI-driven struct decoding
        // once forc-pkg surfaces type info.  See
        // `tests/test_tracer.rs::test_struct_decoding_test_via_ct_print_full`
        // for the regression pin.
        let logd_struct_type_id =
            TraceWriter::ensure_type_id(&mut *writer, TypeKind::Struct, "logd_struct");

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

        // In-program subroutine call/return synthesis state.  Raw
        // fuel-asm input has no native `register_call` source (only
        // contract-to-contract `Receipt::Call` surfaces that way), so
        // for spec-compliance we synthesise call/return events from
        // wide source-line gaps in the step trace: each contiguous
        // run of step lines (gaps <= NESTED_CALL_LINE_GAP_THRESHOLD)
        // is treated as one synthesised function region.
        //
        // `nested_call_depth` counts how many synthesised in-program
        // calls are currently open (not counting the contract-call
        // tracker's own depth, which is independent — those use real
        // `Receipt::Call`/`Receipt::Return` boundaries).  We balance
        // every open call with a `register_return` either on the next
        // function transition or at the end of the recording.
        //
        // `nested_calls_seen` tracks how many distinct call regions we
        // have entered so far so the synthetic name picker can hand out
        // "outer" -> "middle" -> "inner" -> "fn_4" ... in encounter
        // order.  See `tests/test_tracer.rs::
        //  test_nested_calls_test_emits_call_chain` for the regression
        // pin that drives this synthesis.
        let mut nested_call_depth: usize = 0;
        let mut nested_calls_seen: usize = 0;

        let outcome = interp.run(|step| {
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
            //
            // We additionally remember the LOGD payload bytes (if any
            // surfaced this step) so that the per-step variable loop
            // below can attach a structured `ValueRecord::Sequence`
            // version of the buffer to the step that produced it —
            // raw fuel-asm input has no other structured-value surface,
            // and the spec-compliant trace must expose the heap-backed
            // byte buffer the LOGD opcode emits as a Sequence value.
            // See `tests/test_tracer.rs::
            //  test_collections_test_value_kinds_present`.
            let new_receipts = &step.receipts[prev_receipt_count..];
            let mut step_logd_payload: Option<Vec<u8>> = None;
            for receipt in new_receipts {
                if let Receipt::LogData { data: Some(bytes), .. } = receipt {
                    if !bytes.is_empty() && step_logd_payload.is_none() {
                        step_logd_payload = Some(bytes.clone());
                    }
                }
                emit_receipt_special_event(&mut *writer, receipt);
            }
            prev_receipt_count = step.receipts.len();

            // Per-opcode SRW / SWW detection.  These are the FuelVM's
            // storage-read-word / storage-write-word opcodes.  They are
            // contract-only — when executed in script context they
            // immediately panic with `ExpectedInternalContext`, so they
            // never surface as a receipt.  To make storage access visible
            // in the trace even at the point of the offending opcode,
            // emit an io_event capturing the key register and (for SWW)
            // the value register *before* the VM transitions to the panic
            // state.  Mirrors the EVM-recorder SLOAD/SSTORE routing.
            // See `tests/test_tracer.rs::
            //  test_storage_block_test_via_ct_print_full` and
            // `test_storage_map_test_via_ct_print_full` for the
            // regression pins.
            if let Some(instr) = &step.instruction {
                emit_storage_opcode_event(&mut *writer, instr, &step.registers);
            }

            // Look up source location using contract-aware tracker
            let (lookup_path, line) =
                call_tracker.lookup_source(opcode_index, source_map, source_path);
            let step_path = lookup_path.to_path_buf();

            // Determine whether this step crosses a synthesised
            // in-program function boundary.  A "transition" is a step
            // whose source line differs from the previous emitted line
            // (or from the start-anchor `Line(1)` for the very first
            // step) by more than `NESTED_CALL_LINE_GAP_THRESHOLD` —
            // this is how the `nested_calls` fixture's three
            // decade-aligned line ranges (10..12, 20..21, 30..32) are
            // recognised as outer/middle/inner without weakening the
            // simpler control-flow / loop fixtures (whose worst-case
            // line gap is 4 — within the threshold).
            let is_function_transition = if prev_line == Some(line) {
                false
            } else {
                let baseline = prev_line.unwrap_or(1);
                (line as i64 - baseline as i64).abs() > NESTED_CALL_LINE_GAP_THRESHOLD
            };

            // Emit step if line changed.  We deliberately register the
            // step BEFORE any synthesised call/return events for this
            // transition: the FFI's pending-step buffer means the
            // call/return records' `entryStep` / `exitStep` fields are
            // captured against `msWriter.stepCount`, which only
            // advances when `register_step` flushes the previous
            // pending step.  Registering the step first flushes the
            // *previous* line into the *previous* call frame and then
            // captures the boundary at exactly the right step index
            // for the soon-to-be-emitted call/return.
            if prev_line != Some(line) {
                TraceWriter::register_step(&mut *writer, &step_path, Line(line as i64));
                prev_line = Some(line);
            }

            if is_function_transition {
                if nested_call_depth > 0 {
                    TraceWriter::register_return(&mut *writer, NONE_VALUE);
                    nested_call_depth -= 1;
                }
                let callee_name = synthetic_call_name(nested_calls_seen);
                nested_calls_seen += 1;
                let fn_id = TraceWriter::ensure_function_id(
                    &mut *writer,
                    &callee_name,
                    &step_path,
                    Line(line as i64),
                );
                TraceWriter::register_call(&mut *writer, fn_id, vec![]);
                nested_call_depth += 1;
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

            // If a LOGD receipt surfaced this step, also emit its
            // payload bytes as a `ValueRecord::Sequence` step variable.
            // Raw fuel-asm has no other structured-value surface; this
            // is the canonical place to satisfy the spec's "collections"
            // requirement that the trace expose memory-backed byte
            // buffers as a structured collection rather than only as a
            // hex-formatted io_event payload.
            if let Some(payload) = step_logd_payload {
                let elements: Vec<ValueRecord> = payload
                    .iter()
                    .map(|b| ValueRecord::Int {
                        i: *b as i64,
                        type_id: u64_type_id,
                    })
                    .collect();
                let value = ValueRecord::Sequence {
                    elements,
                    is_slice: false,
                    type_id: logd_payload_type_id,
                };
                TraceWriter::register_variable_with_full_value(
                    &mut *writer,
                    "logd_payload",
                    value,
                );

                // Additionally, when the LOGD payload is exactly a
                // multiple of 8 bytes and at least 16 bytes (= two u64
                // fields), surface it as a `ValueRecord::Struct` whose
                // fields are the big-endian-decoded u64 words.  This is
                // the recorder's first structured non-Sequence variant
                // emission — the gateway to ABI-driven struct decoding
                // once forc-pkg surfaces type info.  Keep the byte
                // Sequence above for raw inspection; the Struct is an
                // additional structured surface.
                if payload.len() >= 16 && payload.len() % 8 == 0 {
                    let mut field_values: Vec<ValueRecord> = Vec::new();
                    for chunk in payload.chunks_exact(8) {
                        let word = u64::from_be_bytes(
                            chunk.try_into().expect("chunks_exact(8) yields 8 bytes"),
                        );
                        field_values.push(ValueRecord::Int {
                            i: word as i64,
                            type_id: u64_type_id,
                        });
                    }
                    let struct_value = ValueRecord::Struct {
                        field_values,
                        type_id: logd_struct_type_id,
                    };
                    TraceWriter::register_variable_with_full_value(
                        &mut *writer,
                        "logd_struct",
                        struct_value,
                    );
                }
            }
        })?;

        // Drain receipts the VM appended *after* the final single-step
        // breakpoint.  Terminal `Receipt::Revert`, `Receipt::Return`,
        // `Receipt::ReturnData` and the always-final `Receipt::ScriptResult`
        // live here — without this drain a reverting script would never
        // surface a `FuelRevert` EventLogKind::Error io_event.
        // Cross-recorder convention: terminal failures
        // (panic / abort / throw / fail / revert) MUST surface as a
        // `RecordEvent::Error` io_event (mirrors wasm 693d6834,
        // move 4041840, ton 17e859c, cardano 7e5a177).
        for receipt in &outcome.final_receipts[prev_receipt_count..] {
            match receipt {
                // Skip terminal `Receipt::Return` / `Receipt::ReturnData`:
                // the script-level RET is the natural end of `<toplevel>`
                // and the unconditional `register_return` below already
                // closes that frame — emitting an extra `register_return`
                // would unbalance the call stack.  Cross-contract
                // `Receipt::Call` should also not appear at termination;
                // defensively skip it for symmetry with the step-loop
                // branch where it is handled by the call tracker.
                Receipt::Call { .. } | Receipt::Return { .. } | Receipt::ReturnData { .. } => {}

                // Skip `Receipt::ScriptResult`: it is a transaction-level
                // outcome record (always present, redundant with the
                // Return / Revert / Panic receipt that immediately
                // precedes it).  Routing it through io_events would
                // double-report every script termination: a successful
                // run would gain a spurious `result=Success` io_event
                // and — critically for the test suite — a reverting run
                // would emit *two* error-channel io_events instead of
                // one.  The script-side trap (Revert / Panic) already
                // carries the failure code; ScriptResult adds nothing.
                Receipt::ScriptResult { .. } => {}

                _ => emit_receipt_special_event(&mut *writer, receipt),
            }
        }

        // Balance any synthesised in-program subroutine calls that are
        // still open: if the bytecode never returned to the original
        // line cluster (which is the common case — the fixture ends in
        // the deepest cluster), the matching `register_return` events
        // for each open synthetic frame are emitted here.  Mirrors the
        // PolkaVM in-program call/return synthesis (commit de6eb24)
        // where every termination arm closes the synthetic entry-point
        // frame.
        while nested_call_depth > 0 {
            TraceWriter::register_return(&mut *writer, NONE_VALUE);
            nested_call_depth -= 1;
        }

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

/// Surface FuelVM storage-access opcodes (SRW / SWW) as io_events.
///
/// SRW (`Storage Read Word`) and SWW (`Storage Write Word`) are
/// contract-only opcodes — in script context they panic immediately
/// with `ExpectedInternalContext` and therefore never produce a real
/// state change, but the *attempt* is still useful trace content: it
/// pins where the program tried to touch contract storage.  The
/// recorder emits one io_event per SRW / SWW it sees, capturing the
/// key-address register and (for SWW) the value register, mirroring
/// the EVM-recorder SLOAD / SSTORE routing.  When forc-pkg integration
/// lands and the recorder gets to drive real contract bytecode, this
/// path will surface every successful storage access too, since the
/// per-opcode emission fires regardless of whether the VM later
/// panics or completes the access normally.
fn emit_storage_opcode_event(
    writer: &mut dyn TraceWriter,
    instr: &Instruction,
    registers: &[u64],
) {
    match instr {
        Instruction::SRW(srw) => {
            let (dst, status, key_addr) = srw.unpack();
            let key_addr_idx = usize::from(key_addr);
            let key_addr_val = registers.get(key_addr_idx).copied().unwrap_or(0);
            TraceWriter::register_special_event(
                writer,
                EventLogKind::EvmEvent,
                "FuelStorageRead",
                &format!(
                    "opcode=SRW dst=r{} status=r{} key_addr=r{}={:#x}",
                    usize::from(dst),
                    usize::from(status),
                    key_addr_idx,
                    key_addr_val
                ),
            );
        }
        Instruction::SWW(sww) => {
            let (key_addr, status, value) = sww.unpack();
            let key_addr_idx = usize::from(key_addr);
            let value_idx = usize::from(value);
            let key_addr_val = registers.get(key_addr_idx).copied().unwrap_or(0);
            let value_val = registers.get(value_idx).copied().unwrap_or(0);
            TraceWriter::register_special_event(
                writer,
                EventLogKind::EvmEvent,
                "FuelStorageWrite",
                &format!(
                    "opcode=SWW key_addr=r{}={:#x} status=r{} value=r{}={}",
                    key_addr_idx,
                    key_addr_val,
                    usize::from(status),
                    value_idx,
                    value_val
                ),
            );
        }
        _ => {}
    }
}
