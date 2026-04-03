//! FuelVM execution trace recorder.
//!
//! Processes FuelVM single-step events and produces CodeTracer trace output
//! using the TraceWriter API.

use std::path::{Path, PathBuf};

use codetracer_trace_types::{Line, TypeKind, ValueRecord, NONE_VALUE};
use codetracer_trace_writer::trace_writer::TraceWriter;
use codetracer_trace_writer::{TraceEventsFileFormat, create_trace_writer};
use eyre::{Context, Result};

use crate::abi_decoder::AbiSchema;
use crate::contract_call::ContractCallTracker;
use crate::interpreter::FuelInterpreter;
use crate::source_map::SwaySourceMap;
use crate::variable_tracker::VariableTracker;

/// The main recorder that processes FuelVM execution events into CodeTracer
/// trace format.
pub struct FuelRecorder {
    /// Name of the program being recorded.
    pub program_name: String,
    /// Output directory for trace files.
    pub trace_dir: PathBuf,
    /// Output format.
    pub format: TraceEventsFileFormat,
    /// Optional ABI schema for variable name enrichment.
    pub abi: Option<AbiSchema>,
}

impl FuelRecorder {
    /// Create a new FuelRecorder.
    pub fn new(program_name: &str, trace_dir: &Path, format: TraceEventsFileFormat) -> Self {
        Self {
            program_name: program_name.to_string(),
            trace_dir: trace_dir.to_path_buf(),
            format,
            abi: None,
        }
    }

    /// Create a new FuelRecorder with ABI information for variable enrichment.
    pub fn with_abi(
        program_name: &str,
        trace_dir: &Path,
        format: TraceEventsFileFormat,
        abi: AbiSchema,
    ) -> Self {
        Self {
            program_name: program_name.to_string(),
            trace_dir: trace_dir.to_path_buf(),
            format,
            abi: Some(abi),
        }
    }

    /// Record a FuelVM execution trace.
    ///
    /// Takes bytecode and an optional source map, executes the bytecode with
    /// single-stepping, and writes CodeTracer trace files.
    pub fn record(
        &self,
        bytecode: Vec<u8>,
        source_map: &SwaySourceMap,
        source_path: &Path,
    ) -> Result<()> {
        // Create trace writer
        let mut writer = create_trace_writer(&self.program_name, &[], self.format);

        // Create output directory
        std::fs::create_dir_all(&self.trace_dir)
            .with_context(|| format!("cannot create output dir: {}", self.trace_dir.display()))?;

        // Use the correct filename extension so that db-backend can infer
        // the format from the file extension (.json → JSON, .bin → Binary).
        let events_filename = match self.format {
            TraceEventsFileFormat::Json => "trace.json",
            TraceEventsFileFormat::Binary | TraceEventsFileFormat::BinaryV0 => "trace.bin",
        };
        let events_path = self.trace_dir.join(events_filename);
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
        let main_fn_id = TraceWriter::ensure_function_id(
            &mut *writer,
            "main",
            source_path,
            Line(1),
        );
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
                let name = if let Some(tracked) = tracked_vars.iter().find(|v| v.register == reg_idx) {
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
        TraceWriter::finish_writing_trace_events(&mut *writer)
            .map_err(|e| eyre::eyre!("{e}"))?;
        TraceWriter::finish_writing_trace_metadata(&mut *writer)
            .map_err(|e| eyre::eyre!("{e}"))?;
        TraceWriter::finish_writing_trace_paths(&mut *writer)
            .map_err(|e| eyre::eyre!("{e}"))?;

        Ok(())
    }
}
