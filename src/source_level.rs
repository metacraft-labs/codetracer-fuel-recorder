//! Source-level recording of forc-built programs.
//!
//! When `forc`'s debug symbols are available (see [`crate::debug_info`]),
//! the trace is built from them rather than from machine state:
//!
//! * **Steps** are the source locations the compiler attributed to the
//!   executed instructions. A step is recorded when execution reaches an
//!   instruction mapped to a different location than the frame's previous
//!   step; instructions the compiler left unmapped (prologues, glue) do not
//!   move the current position.
//! * **Frames** follow the FuelVM call convention forc emits: a call is a
//!   `JAL` that saves a return address (`jal $$reta $pc imm`), and the frame
//!   ends when execution arrives at that return address. The callee's first
//!   instruction is mapped to its declaration, which names the frame.
//! * **Locals** are not recorded: forc's debug symbols carry no variable
//!   names or locations, and machine registers are not source variables.
//!
//! Receipts (logs, panics, reverts, ...) are routed to the event stream
//! exactly as in the instruction-level recorder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use codetracer_trace_types::{Line, NONE_VALUE};
use codetracer_trace_writer_nim::trace_writer::TraceWriter;
use codetracer_trace_writer_nim::{TraceEventsFileFormat, create_trace_writer};
use eyre::{Context, Result, eyre};
use fuel_asm::{Instruction, RawInstruction, RegId};
use fuel_tx::Receipt;

use crate::debug_info::{ForcDebugInfo, SourceLoc};
use crate::interpreter::FuelInterpreter;
use crate::recorder::{FuelRecorder, emit_receipt_special_event};

/// An open source-level call frame.
struct Frame {
    /// Absolute address execution returns to when the frame ends.
    return_addr: u64,
    /// Location of the frame's most recent step.
    last: Option<(PathBuf, u32)>,
}

/// Names functions from the declaration their entry instruction maps to.
#[derive(Default)]
struct FunctionNames {
    sources: HashMap<PathBuf, Option<Vec<String>>>,
}

impl FunctionNames {
    /// The name declared at `loc`, if `loc` is the start of a
    /// `[pub] fn <name>` item.
    fn declared_at(&mut self, loc: &SourceLoc) -> Option<String> {
        let lines = self
            .sources
            .entry(loc.path.clone())
            .or_insert_with(|| {
                std::fs::read_to_string(&loc.path)
                    .ok()
                    .map(|s| s.lines().map(str::to_string).collect())
            })
            .as_ref()?;
        let line = lines.get((loc.line as usize).checked_sub(1)?)?;
        let rest: String = line
            .chars()
            .skip((loc.column as usize).saturating_sub(1))
            .collect();
        let mut words = rest.split_whitespace();
        let mut word = words.next()?;
        if word == "pub" {
            word = words.next()?;
        }
        if word != "fn" {
            return None;
        }
        let name: String = words
            .next()?
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        (!name.is_empty()).then_some(name)
    }

    fn name_for(&mut self, loc: Option<&SourceLoc>, entry_index: usize) -> String {
        match loc {
            Some(loc) => self.declared_at(loc).unwrap_or_else(|| {
                format!(
                    "<fn at {}:{}>",
                    loc.path.file_name().unwrap_or_default().to_string_lossy(),
                    loc.line
                )
            }),
            None => format!("<fn at instruction {entry_index}>"),
        }
    }
}

impl FuelRecorder {
    /// Record `bytecode` at source level using forc's debug symbols.
    pub fn record_with_debug_info(&self, bytecode: Vec<u8>, debug: &ForcDebugInfo) -> Result<()> {
        let entry = debug
            .first()
            .cloned()
            .ok_or_else(|| eyre!("debug symbols map no instruction to source"))?;

        std::fs::create_dir_all(&self.trace_dir)
            .with_context(|| format!("cannot create output dir: {}", self.trace_dir.display()))?;
        let mut writer = create_trace_writer(&self.program_name, &[], TraceEventsFileFormat::Ctfs);
        let events_path = self.trace_dir.join("trace.bin");
        TraceWriter::begin_writing_trace_events(&mut *writer, &events_path)
            .map_err(|e| eyre!("{e}"))?;
        TraceWriter::start(&mut *writer, &entry.path, Line(entry.line as i64));

        let interp = FuelInterpreter::new(bytecode)?;
        let mut names = FunctionNames::default();
        let mut frames: Vec<Frame> = vec![Frame {
            // The top-level frame ends when the script does.
            return_addr: u64::MAX,
            last: Some((entry.path.clone(), entry.line)),
        }];
        // Return address of a call whose callee has not executed yet.
        let mut pending_call: Option<u64> = None;
        let mut prev_receipt_count = 0usize;

        let outcome = interp.run(|step| {
            let abs_pc = step.registers[RegId::PC.to_u8() as usize];
            let index = (step.pc / 4) as usize;

            for receipt in &step.receipts[prev_receipt_count..] {
                emit_receipt_special_event(&mut *writer, receipt);
            }
            prev_receipt_count = step.receipts.len();

            // Returns: execution arrived back at an open frame's return
            // address.
            while frames.len() > 1 && frames.last().is_some_and(|f| f.return_addr == abs_pc) {
                frames.pop();
                TraceWriter::register_return(&mut *writer, NONE_VALUE);
                if let Some(caller) = frames.last_mut() {
                    // The caller resumes: its next mapped instruction is a
                    // new step even on the line it left from.
                    caller.last = None;
                }
            }

            let loc = debug.lookup(index);

            // Calls: the previous instruction was a call, so this is the
            // callee's entry.
            if let Some(return_addr) = pending_call.take() {
                let name = names.name_for(loc, index);
                let (decl_path, decl_line) = match loc {
                    Some(l) => (l.path.as_path(), l.line),
                    None => (entry.path.as_path(), entry.line),
                };
                let fn_id = TraceWriter::ensure_function_id(
                    &mut *writer,
                    &name,
                    decl_path,
                    Line(decl_line as i64),
                );
                TraceWriter::register_call(&mut *writer, fn_id, vec![]);
                frames.push(Frame {
                    return_addr,
                    last: None,
                });
            }

            let top_return = frames.last().map(|f| f.return_addr).unwrap_or(u64::MAX);
            let leaves_frame = match &step.instruction {
                // `ret` / `retd` end the script.
                Some(Instruction::RET(_)) | Some(Instruction::RETD(_)) => true,
                // `jal $zero $$reta 0` is a function return.
                Some(Instruction::JAL(jal)) => {
                    let (ret_addr, target, offset) = jal.unpack();
                    if ret_addr == RegId::ZERO {
                        let dest = step.registers[target.to_u8() as usize]
                            .wrapping_add(u64::from(offset) * 4);
                        dest == top_return
                    } else {
                        pending_call = Some(abs_pc + 4);
                        false
                    }
                }
                _ => false,
            };

            // The instruction that transfers control out of a frame has no
            // source-level effect of its own; forc sometimes attributes it
            // to the next function's declaration.
            if !leaves_frame
                && let Some(loc) = loc
                && let Some(frame) = frames.last_mut()
                && frame.last.as_ref() != Some(&(loc.path.clone(), loc.line))
            {
                TraceWriter::register_step(&mut *writer, &loc.path, Line(loc.line as i64));
                frame.last = Some((loc.path.clone(), loc.line));
            }
        })?;

        for receipt in &outcome.final_receipts[prev_receipt_count..] {
            match receipt {
                // The script's own termination closes the top-level frame
                // below; `ScriptResult` repeats the outcome of the receipt
                // before it.
                Receipt::Return { .. }
                | Receipt::ReturnData { .. }
                | Receipt::ScriptResult { .. } => {}
                _ => emit_receipt_special_event(&mut *writer, receipt),
            }
        }

        // Frames still open when the script ends (a panic or revert inside
        // a function), then the top-level frame.
        for _ in 0..frames.len() {
            TraceWriter::register_return(&mut *writer, NONE_VALUE);
        }

        TraceWriter::finish_writing_trace_events(&mut *writer).map_err(|e| eyre!("{e}"))?;
        writer
            .write_meta_dat("codetracer-fuel-recorder")
            .map_err(|e| eyre!("{e}"))?;
        writer.close().map_err(|e| eyre!("{e}"))?;
        Ok(())
    }
}

/// Write a one-instruction-per-line disassembly of `bytecode` to `path`,
/// so that bytecode without debug symbols can be recorded against a file
/// that exists: line `n` is instruction `n - 1`.
pub fn write_disassembly(bytecode: &[u8], path: &Path) -> Result<()> {
    let mut text = String::new();
    for (i, word) in bytecode.chunks(4).enumerate() {
        let mut raw = [0u8; 4];
        raw[..word.len()].copy_from_slice(word);
        let rendered = match Instruction::try_from(RawInstruction::from_be_bytes(raw)) {
            Ok(instr) => format!("{instr:?}"),
            Err(_) => format!(".word 0x{}", hex(&raw)),
        };
        text.push_str(&format!("{:#06x}: {rendered}\n", i * 4));
    }
    std::fs::write(path, text)
        .with_context(|| format!("failed to write disassembly: {}", path.display()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
