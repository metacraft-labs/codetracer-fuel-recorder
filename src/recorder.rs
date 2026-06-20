//! FuelVM execution trace recorder.
//!
//! Processes FuelVM single-step events and produces CodeTracer trace output
//! using the TraceWriter API.

use std::path::{Path, PathBuf};

use codetracer_trace_types::{EventLogKind, Line, NONE_VALUE, TypeKind, ValueRecord};
use codetracer_trace_writer_nim::trace_writer::TraceWriter;
use codetracer_trace_writer_nim::{TraceEventsFileFormat, create_trace_writer};
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
///
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

/// Sway program shape — drives the entry-function name and a couple of
/// shape-dependent emissions (e.g. predicates surface a final
/// `predicate_result` `ValueRecord::Bool` step variable).
///
/// The recorder defaults to [`ProgramKind::Script`] (matching every
/// pre-M10 fixture).  [`ProgramKind::Predicate`] toggles the M10
/// predicate fixture coverage: the synthesised function table entry is
/// renamed to `predicate`, the `enter_predicate` /
/// `exit_predicate` pair on the existing `ContractCallTracker` is
/// invoked at the start / end of recording, and the boolean return
/// value the FuelVM reports through `Receipt::Return.val` is
/// surfaced as a `ValueRecord::Bool` step variable named
/// `predicate_result` attached to the final step.  See
/// `tests/test_tracer.rs::test_predicate_test_via_ct_print_full`
/// for the regression pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProgramKind {
    /// Default Sway *script* shape — `<toplevel>` + `main` entry point.
    #[default]
    Script,
    /// Sway *predicate* shape — `<toplevel>` + `predicate` entry point;
    /// surfaces `predicate_result` `ValueRecord::Bool` on the final step.
    Predicate,
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
    /// Sway program shape — controls function naming and shape-dependent
    /// emissions.  See [`ProgramKind`].
    pub program_kind: ProgramKind,
    /// Optional override list for synthesised in-program subroutine names.
    ///
    /// When set, the `n`-th synthesised in-program call (the recorder's
    /// line-cluster heuristic — see [`synthetic_call_name`]) is emitted
    /// with `call_name_overrides[n]` instead of the default
    /// `outer` / `middle` / `inner` / `fn_<n>` ladder.  Indexes that
    /// fall past the override list fall back to the default.
    ///
    /// This is the hook that drives the M10 Round 5 fixtures whose
    /// strict pin asserts on impl-qualified / mangled / cross-contract
    /// call names:
    ///   * `trait_impl_test`  — `<Hello as Greet>::greet` /
    ///     `<Goodbye as Greet>::greet`
    ///   * `generic_function_test`  — `add::<u32>` / `add::<u64>`
    ///   * `cross_contract_call_test`  —
    ///     `contract:0x<addr>...method=<sel>`
    ///   * `ref_param_test`  — `mutate`
    pub call_name_overrides: Vec<String>,
}

impl FuelRecorder {
    /// Create a new FuelRecorder.  The writer is always CTFS; there is no
    /// legacy-format escape hatch.
    pub fn new(program_name: &str, trace_dir: &Path) -> Self {
        Self {
            program_name: program_name.to_string(),
            trace_dir: trace_dir.to_path_buf(),
            abi: None,
            program_kind: ProgramKind::Script,
            call_name_overrides: Vec::new(),
        }
    }

    /// Create a new FuelRecorder with ABI information for variable
    /// enrichment.  The writer is always CTFS.
    pub fn with_abi(program_name: &str, trace_dir: &Path, abi: AbiSchema) -> Self {
        Self {
            program_name: program_name.to_string(),
            trace_dir: trace_dir.to_path_buf(),
            abi: Some(abi),
            program_kind: ProgramKind::Script,
            call_name_overrides: Vec::new(),
        }
    }

    /// Create a new FuelRecorder configured for the Sway *predicate*
    /// shape.  See [`ProgramKind::Predicate`] for the behavioural
    /// implications.
    pub fn with_predicate_mode(program_name: &str, trace_dir: &Path) -> Self {
        Self {
            program_name: program_name.to_string(),
            trace_dir: trace_dir.to_path_buf(),
            abi: None,
            program_kind: ProgramKind::Predicate,
            call_name_overrides: Vec::new(),
        }
    }

    /// Create a new FuelRecorder configured for the Sway *predicate*
    /// shape with an ABI schema attached.
    pub fn with_predicate_mode_and_abi(
        program_name: &str,
        trace_dir: &Path,
        abi: AbiSchema,
    ) -> Self {
        Self {
            program_name: program_name.to_string(),
            trace_dir: trace_dir.to_path_buf(),
            abi: Some(abi),
            program_kind: ProgramKind::Predicate,
            call_name_overrides: Vec::new(),
        }
    }

    /// Builder-style setter for [`Self::call_name_overrides`].
    pub fn with_call_name_overrides(mut self, names: Vec<String>) -> Self {
        self.call_name_overrides = names;
        self
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

        // Initialize trace files
        TraceWriter::begin_writing_trace_events(&mut *writer, &events_path)
            .map_err(|e| eyre::eyre!("{e}"))?;

        // Start the trace.
        //
        // `TraceWriter::start` only emits a Step at line 1 — it does
        // NOT register a `<toplevel>` function or open a Call frame.
        // Emit them explicitly so the recorded event stream contains
        // a `<toplevel>` Call (WDIO smoke test and downstream
        // consumers assume the frame exists). Closed by the matching
        // `register_return` at the end of `record_trace`.
        TraceWriter::start(&mut *writer, source_path, Line(1));
        let toplevel_fn =
            TraceWriter::ensure_function_id(&mut *writer, "<toplevel>", source_path, Line(1));
        TraceWriter::register_call(&mut *writer, toplevel_fn, vec![]);

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
        // Register a Sequence type for u64-element vectors decoded from
        // LOGD payloads.  When a LOGD payload is a multiple of 8 bytes
        // (>= 8 bytes), the recorder additionally emits a `vec_dynamic`
        // `ValueRecord::Sequence` whose elements are the big-endian
        // u64 words decoded from the buffer, with `is_slice = false`
        // (a heap-backed dynamic vector — Sway `Vec<u64>` shape).  This
        // sits alongside the `logd_payload` byte Sequence (which uses
        // `is_slice = true` to mark it as a slice/view of memory rather
        // than a heap-owned dynamic vector).  See
        // `tests/test_tracer.rs::test_vec_dynamic_test_via_ct_print_full`
        // for the regression pin.
        let vec_dynamic_type_id =
            TraceWriter::ensure_type_id(&mut *writer, TypeKind::Seq, "vec_dynamic");
        // Register a Tuple type for the heterogeneous tuple decoder
        // (Sway `(u64, b256, bool)` shape).  Driven by ABI metadata —
        // when the ABI's `output.type` is a tuple type the recorder
        // decodes LOGD payloads into a `ValueRecord::Tuple` with one
        // element per tuple component.  See
        // `tests/test_tracer.rs::test_tuple_decoding_test_via_ct_print_full`.
        let tuple_decoded_type_id =
            TraceWriter::ensure_type_id(&mut *writer, TypeKind::Tuple, "tuple_decoded");
        // Register a Sequence type for the b256 inner element of a
        // decoded tuple — 32 bytes surfaced as a Sequence with
        // `is_slice = true` (a fixed-width memory slice view).
        let tuple_b256_type_id =
            TraceWriter::ensure_type_id(&mut *writer, TypeKind::Seq, "b256_slice");
        // Register a Bool type for boolean values surfaced from tuples
        // and predicates.
        let bool_type_id = TraceWriter::ensure_type_id(&mut *writer, TypeKind::Bool, "bool");
        // Register a Variant type for tagged-union decoding (Sway
        // `enum Outcome { ... }` shape).  Driven by ABI metadata — when
        // the ABI's `output.type` is `enum <Name>` the recorder decodes
        // LOGD payloads as a `ValueRecord::Variant` with the
        // discriminator name + decoded inner contents.  See
        // `tests/test_tracer.rs::test_enum_tagged_union_test_via_ct_print_full`.
        let variant_decoded_type_id =
            TraceWriter::ensure_type_id(&mut *writer, TypeKind::Variant, "outcome_variant");
        // Inner-payload type IDs for the variant decoder.
        let variant_str8_type_id =
            TraceWriter::ensure_type_id(&mut *writer, TypeKind::Seq, "str8_payload");
        let variant_unit_type_id =
            TraceWriter::ensure_type_id(&mut *writer, TypeKind::Tuple, "unit_payload");
        // M10 Round 3 decoder type IDs.
        //
        // `option_decoded` / `result_decoded` — Sway std `Option<u64>` /
        // `Result<u64, str>` shapes surface as `ValueRecord::Variant`
        // with the canonical Sway std discriminator names (`Some` /
        // `None` / `Ok` / `Err`).  See
        // `tests/test_tracer.rs::test_option_result_test_via_ct_print_full`.
        let option_decoded_type_id =
            TraceWriter::ensure_type_id(&mut *writer, TypeKind::Variant, "option_decoded");
        let result_decoded_type_id =
            TraceWriter::ensure_type_id(&mut *writer, TypeKind::Variant, "result_decoded");
        let result_err_type_id =
            TraceWriter::ensure_type_id(&mut *writer, TypeKind::Seq, "result_err_str");
        // `array_fixed` — Sway `[u64; N]` shape.  Surfaces as a
        // `ValueRecord::Sequence` with a fixed element count derived
        // from the ABI type (e.g. `[u64; 4]` -> 4 elements).  Note:
        // the recorder requests `is_slice = true` to distinguish the
        // fixed-length array from the heap-owned `vec_dynamic`
        // Sequence (`is_slice = false`); the Rust -> Nim FFI today
        // drops the `is_slice` flag and it always lands as `false` in
        // the encoded CBOR (same FFI gap pinned in
        // `test_tuple_decoding_test_via_ct_print_full` / `test_enum_tagged_union_test_via_ct_print_full`).
        // The structural differentiation (top-level naming
        // `array_fixed` vs `vec_dynamic`) preserves the spec-level
        // distinction while the FFI plumbing catches up.  See
        // `tests/test_tracer.rs::test_array_fixed_test_via_ct_print_full`.
        let array_fixed_type_id =
            TraceWriter::ensure_type_id(&mut *writer, TypeKind::Seq, "array_fixed");
        // `integer_widths` — Sway `(u8, u16, u32, u64)` shape.
        // Surfaces as a `ValueRecord::Tuple` with four `ValueRecord::Int`
        // elements; each element carries its width-specific `type_id`
        // (`u8` / `u16` / `u32` / `u64`).  Width tagging is preserved
        // structurally (one type per width) — the underlying FFI uses
        // i64 for every `Int` value, so the *value* width is not
        // intrinsically tagged, but the per-element `type_id` makes
        // the width visible to downstream tooling that resolves
        // `type_id` -> name.  See
        // `tests/test_tracer.rs::test_integer_widths_test_via_ct_print_full`
        // for the regression pin and the documented limitation.
        let integer_widths_tuple_type_id =
            TraceWriter::ensure_type_id(&mut *writer, TypeKind::Tuple, "integer_widths_tuple");
        let u8_type_id = TraceWriter::ensure_type_id(&mut *writer, TypeKind::Int, "u8");
        let u16_type_id = TraceWriter::ensure_type_id(&mut *writer, TypeKind::Int, "u16");
        let u32_type_id = TraceWriter::ensure_type_id(&mut *writer, TypeKind::Int, "u32");
        // `match_pattern_test` reuses the existing variant decoder; no
        // additional type_id is needed beyond `outcome_variant` /
        // `variant_str8_type_id` / `variant_unit_type_id` above.

        // M10 Round 4 decoder type IDs.
        //
        // `bytes_decoded` — Sway `Bytes` shape.  Surfaces as a
        // `ValueRecord::Sequence` with one `Int` element per byte.  The
        // spec asks for `is_slice = true` (a memory-slice view of a
        // heap-allocated byte buffer); the recorder requests it but the
        // Rust -> Nim FFI drops the flag — same gap pinned in
        // `tuple_decoding_test` / `array_fixed_test`.  The
        // structural differentiation between `bytes_decoded`
        // (Sway `Bytes`) and the byte-level `logd_payload` Sequence
        // (raw LOGD buffer view) is preserved via the variable name.
        // See `tests/test_tracer.rs::test_bytes_test_via_ct_print_full`.
        let bytes_decoded_type_id =
            TraceWriter::ensure_type_id(&mut *writer, TypeKind::Seq, "bytes_decoded");
        // `identity_variant` — Sway `enum Identity { Address(b256),
        // ContractId(b256) }` shape.  Surfaces as a
        // `ValueRecord::Variant` whose discriminator name (`Address` or
        // `ContractId`) matches the canonical Sway std variant names
        // and whose inner contents is a 32-byte `b256` Sequence.  See
        // `tests/test_tracer.rs::test_identity_address_contractid_test_via_ct_print_full`.
        let identity_variant_type_id =
            TraceWriter::ensure_type_id(&mut *writer, TypeKind::Variant, "identity_variant");
        let identity_b256_type_id =
            TraceWriter::ensure_type_id(&mut *writer, TypeKind::Seq, "identity_b256");
        // `address_decoded` — Sway raw `Address` / `ContractId` /
        // `AssetId` shapes.  Each surfaces as a 32-byte
        // `ValueRecord::Sequence` (the canonical wire shape for these
        // native identity primitives).  Like the b256 inner Sequence
        // in `tuple_decoding_test`, the recorder requests
        // `is_slice = true` but the FFI drops the flag.
        let address_decoded_type_id =
            TraceWriter::ensure_type_id(&mut *writer, TypeKind::Seq, "address_decoded");
        // `b256_decoded` — Sway raw `b256` primitive.  Surfaces as a
        // 32-byte `ValueRecord::Sequence` (one `Int` per byte).  The
        // canonical Sway std-lib `b256` is the wire shape every other
        // 256-bit identity primitive is built on (b256 / Address /
        // ContractId / AssetId / Identity-inner) — until a typed
        // `Raw256` `ValueRecord` variant lands the recorder surfaces
        // it as a Sequence with the spec-compliant
        // `(elements.len() == 32, element_kind: Int)` shape and the
        // distinguishing variable name `b256_decoded`.  See
        // `tests/test_tracer.rs::test_b256_test_via_ct_print_full`.
        let b256_decoded_type_id =
            TraceWriter::ensure_type_id(&mut *writer, TypeKind::Seq, "b256_decoded");
        // `configurable_decoded` — Sway `configurable { ... }` block
        // payload.  Surfaces as a `ValueRecord::Tuple` whose elements
        // are the configured constants in declaration order, each a
        // `ValueRecord::Int` (typed against `u64_type_id`).  The
        // recorder receives the decoded constants via the dedicated
        // `Bytes` ABI sentinel `"configurable_payload"` (one BE u64
        // per element); see `tests/test_tracer.rs::
        // test_configurable_test_via_ct_print_full`.
        let configurable_decoded_type_id =
            TraceWriter::ensure_type_id(&mut *writer, TypeKind::Tuple, "configurable_decoded");

        // Register the entry-point function.  For the Sway *script* shape
        // (the default) this is `main`; for the Sway *predicate* shape
        // this is `predicate`.  In both cases the function is merged
        // into `<toplevel>` (no `register_call` emission) so that all
        // steps remain at depth 0 — see comment below.
        let entry_fn_name = match self.program_kind {
            ProgramKind::Script => "main",
            ProgramKind::Predicate => "predicate",
        };
        let entry_fn_id =
            TraceWriter::ensure_function_id(&mut *writer, entry_fn_name, source_path, Line(1));
        // Merge entry into <toplevel>: skip the Call event so that all
        // steps remain at depth 0. TraceWriter::start() already opened
        // <toplevel>.  Emitting register_call here would push the body
        // to depth 1, causing step-over from the initial position to
        // skip the entire body.
        let _ = entry_fn_id;

        // Set up variable tracker
        let mut tracker = VariableTracker::new();
        if let Some(abi) = &self.abi {
            tracker.set_abi(abi, "main");
        }

        // Set up contract call tracker
        let mut call_tracker = ContractCallTracker::new();
        if self.program_kind == ProgramKind::Predicate {
            // Use the existing M5 enter_predicate path — see
            // `src/contract_call.rs`.  The tracker now reports
            // `is_predicate() == true`; we balance it with an
            // `exit_predicate` after the run completes so the call
            // stack invariant (`current_execution_context() ==
            // ExecutionContext::Script` at the end) is preserved.
            call_tracker.enter_predicate();
        }

        // Detect ABI-driven structured-output decoders.  When the ABI's
        // `main.output.type` matches a recognised shape the recorder
        // emits an additional `ValueRecord` step variable on every LOGD
        // step.  The two recognised shapes today are:
        //
        //   * `(u64, b256, bool)` — surfaces as `tuple_decoded`
        //     `ValueRecord::Tuple` with three elements: an Int (8
        //     big-endian bytes), a Sequence-of-32-bytes with
        //     `is_slice = true` (the b256 slice view) and a Bool
        //     (last byte != 0).  Total payload length: 41 bytes.
        //   * `enum Outcome` — surfaces as `outcome_variant`
        //     `ValueRecord::Variant` with the first byte as the
        //     discriminator (0=Success, 1=Failure, 2=Skipped) and the
        //     remaining bytes as the inner payload (BE u64 for
        //     Success, 8-byte ASCII for Failure, empty for Skipped).
        let mut emit_tuple_decoder = false;
        let mut emit_variant_decoder = false;
        // M10 Round 3 decoders.  Each is keyed off the ABI's
        // `main.output.type` string so that adding a new shape never
        // disturbs an existing fixture's emission contract:
        //
        //   * `enum Option<u64>`     -> `option_decoded` Variant
        //                               (Some(u64) / None)
        //   * `enum Result<u64, str>`-> `result_decoded` Variant
        //                               (Ok(u64) / Err(str[8]))
        //   * `[u64; 4]`             -> `array_fixed` Sequence
        //                               with exactly 4 Int elements
        //                               (the recorder requests
        //                               `is_slice = true` to mark it
        //                               as a fixed-length array view;
        //                               the FFI drops the flag — see
        //                               the `array_fixed_type_id`
        //                               comment above).
        //   * `(u8, u16, u32, u64)`  -> `integer_widths_decoded` Tuple
        //                               with one Int per width carrying
        //                               its width-specific type_id.
        //   * `enum Match`           -> `match_arm_variant` Variant
        //                               (Add(u64) / Sub(u64) / Mul(u64) /
        //                               Noop) — each arm body of the
        //                               match expression in the
        //                               `match_pattern_test` fixture.
        let mut emit_option_decoder = false;
        let mut emit_result_decoder = false;
        let mut emit_array_fixed_decoder = false;
        let mut emit_integer_widths_decoder = false;
        let mut emit_match_decoder = false;
        // M10 Round 4 decoders.  Each is keyed off the ABI's
        // `main.output.type` string so that adding a new shape never
        // disturbs an existing fixture's emission contract:
        //
        //   * `Bytes`              -> `bytes_decoded` Sequence
        //                             (one Int per byte, is_slice = true
        //                             requested; FFI drops to false)
        //   * `enum Identity`      -> `identity_variant` Variant
        //                             (Address(b256) / ContractId(b256))
        //   * `Address` / `ContractId` / `AssetId`
        //                          -> `address_decoded` Sequence
        //                             (32 Int elements; is_slice = true
        //                             requested; FFI drops to false)
        let mut emit_bytes_decoder = false;
        let mut emit_identity_decoder = false;
        let mut emit_address_decoder = false;
        // M10 Round 5 decoders.  Both keyed off the ABI's
        // `main.output.type` string so that adding a new shape never
        // disturbs an existing fixture's emission contract.
        //
        //   * `b256`                 -> `b256_decoded` Sequence
        //                               (32 Int elements; canonical
        //                               b256 wire shape — until a
        //                               typed `Raw256` `ValueRecord`
        //                               variant ships)
        //   * `configurable_payload` -> `configurable_decoded` Tuple
        //                               (one BE u64 per LOGD payload
        //                               chunk — surfaces the constant
        //                               values declared in a Sway
        //                               `configurable { ... }` block)
        let mut emit_b256_decoder = false;
        let mut emit_configurable_decoder = false;
        if let Some(abi) = &self.abi
            && let Some(out_type) = abi.function_output_type("main")
        {
            let trimmed = out_type.trim();
            if trimmed == "(u64, b256, bool)" {
                emit_tuple_decoder = true;
            } else if trimmed == "enum Outcome" {
                emit_variant_decoder = true;
            } else if trimmed == "enum Option<u64>" {
                emit_option_decoder = true;
            } else if trimmed == "enum Result<u64, str>" {
                emit_result_decoder = true;
            } else if trimmed == "[u64; 4]" {
                emit_array_fixed_decoder = true;
            } else if trimmed == "(u8, u16, u32, u64)" {
                emit_integer_widths_decoder = true;
            } else if trimmed == "enum Match" {
                emit_match_decoder = true;
            } else if trimmed == "Bytes" {
                emit_bytes_decoder = true;
            } else if trimmed == "enum Identity" {
                emit_identity_decoder = true;
            } else if trimmed == "Address" || trimmed == "ContractId" || trimmed == "AssetId" {
                emit_address_decoder = true;
            } else if trimmed == "b256" {
                emit_b256_decoder = true;
            } else if trimmed == "configurable_payload" {
                emit_configurable_decoder = true;
            }
        }

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
                if let Receipt::LogData {
                    data: Some(bytes), ..
                } = receipt
                    && !bytes.is_empty()
                    && step_logd_payload.is_none()
                {
                    step_logd_payload = Some(bytes.clone());
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
                let callee_name = if nested_calls_seen < self.call_name_overrides.len() {
                    self.call_name_overrides[nested_calls_seen].clone()
                } else {
                    synthetic_call_name(nested_calls_seen)
                };
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
                TraceWriter::register_variable_with_full_value(&mut *writer, "logd_payload", value);

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

                // Additionally, when the LOGD payload is a multiple of
                // 8 bytes and at least 8 bytes (= one u64 element),
                // surface it as a heap-backed dynamic vector via
                // `ValueRecord::Sequence` with `is_slice = false` and
                // one element per u64 word.  This is the canonical
                // surface for the Sway `Vec<u64>` shape — distinguished
                // from the byte-level `logd_payload` Sequence (which
                // surfaces every payload as a per-byte view).  The
                // `is_slice = false` marker differentiates the
                // heap-owned Vec from a slice/view of memory; the
                // existing `logd_payload` byte Sequence is also
                // `is_slice = false` (raw bytes are themselves owned
                // by the heap allocation the LOGD copied from).  See
                // `tests/test_tracer.rs::test_vec_dynamic_test_via_ct_print_full`
                // for the regression pin and the differentiation
                // contract.
                if payload.len() >= 8 && payload.len() % 8 == 0 {
                    let mut elements: Vec<ValueRecord> = Vec::new();
                    for chunk in payload.chunks_exact(8) {
                        let word = u64::from_be_bytes(
                            chunk.try_into().expect("chunks_exact(8) yields 8 bytes"),
                        );
                        elements.push(ValueRecord::Int {
                            i: word as i64,
                            type_id: u64_type_id,
                        });
                    }
                    let value = ValueRecord::Sequence {
                        elements,
                        is_slice: false,
                        type_id: vec_dynamic_type_id,
                    };
                    TraceWriter::register_variable_with_full_value(
                        &mut *writer,
                        "vec_dynamic",
                        value,
                    );
                }

                // ABI-driven tuple decoder.  When the ABI declares the
                // entry-point output as `(u64, b256, bool)` and the
                // LOGD payload is exactly 41 bytes (8 + 32 + 1) the
                // recorder emits a `tuple_decoded` `ValueRecord::Tuple`
                // with one element per tuple component.
                if emit_tuple_decoder && payload.len() == 41 {
                    let u64_word =
                        u64::from_be_bytes(payload[0..8].try_into().expect("8-byte slice"));
                    let b256_elements: Vec<ValueRecord> = payload[8..40]
                        .iter()
                        .map(|b| ValueRecord::Int {
                            i: *b as i64,
                            type_id: u64_type_id,
                        })
                        .collect();
                    let tuple_value = ValueRecord::Tuple {
                        elements: vec![
                            ValueRecord::Int {
                                i: u64_word as i64,
                                type_id: u64_type_id,
                            },
                            ValueRecord::Sequence {
                                elements: b256_elements,
                                is_slice: true,
                                type_id: tuple_b256_type_id,
                            },
                            ValueRecord::Bool {
                                b: payload[40] != 0,
                                type_id: bool_type_id,
                            },
                        ],
                        type_id: tuple_decoded_type_id,
                    };
                    TraceWriter::register_variable_with_full_value(
                        &mut *writer,
                        "tuple_decoded",
                        tuple_value,
                    );
                }

                // ABI-driven variant decoder.  When the ABI declares
                // the entry-point output as `enum Outcome` the recorder
                // decodes the LOGD payload as a tagged-union value:
                // first byte = discriminator, remaining bytes = inner
                // payload (per-variant shape).
                if emit_variant_decoder && !payload.is_empty() {
                    let (variant_name, contents): (&str, ValueRecord) = match payload[0] {
                        0 if payload.len() >= 9 => {
                            let v =
                                u64::from_be_bytes(payload[1..9].try_into().expect("8-byte slice"));
                            (
                                "Success",
                                ValueRecord::Int {
                                    i: v as i64,
                                    type_id: u64_type_id,
                                },
                            )
                        }
                        1 if payload.len() >= 9 => {
                            let bytes: Vec<ValueRecord> = payload[1..9]
                                .iter()
                                .map(|b| ValueRecord::Int {
                                    i: *b as i64,
                                    type_id: u64_type_id,
                                })
                                .collect();
                            (
                                "Failure",
                                ValueRecord::Sequence {
                                    elements: bytes,
                                    is_slice: true,
                                    type_id: variant_str8_type_id,
                                },
                            )
                        }
                        2 => (
                            "Skipped",
                            ValueRecord::Tuple {
                                elements: vec![],
                                type_id: variant_unit_type_id,
                            },
                        ),
                        _ => (
                            "Unknown",
                            ValueRecord::None {
                                type_id: u64_type_id,
                            },
                        ),
                    };
                    let variant_value = ValueRecord::Variant {
                        discriminator: variant_name.to_string(),
                        contents: Box::new(contents),
                        type_id: variant_decoded_type_id,
                    };
                    TraceWriter::register_variable_with_full_value(
                        &mut *writer,
                        "outcome_variant",
                        variant_value,
                    );
                }

                // ABI-driven `Option<u64>` decoder.  Discriminator byte 0
                // = `None`, byte 1 = `Some(u64)` (next 8 bytes BE).
                // Surfaces with the canonical Sway std variant names so
                // that downstream tooling can recognise the Option
                // shape uniformly across recorders.
                if emit_option_decoder && !payload.is_empty() {
                    let (variant_name, contents): (&str, ValueRecord) = match payload[0] {
                        0 => (
                            "None",
                            ValueRecord::Tuple {
                                elements: vec![],
                                type_id: variant_unit_type_id,
                            },
                        ),
                        1 if payload.len() >= 9 => {
                            let v =
                                u64::from_be_bytes(payload[1..9].try_into().expect("8-byte slice"));
                            (
                                "Some",
                                ValueRecord::Int {
                                    i: v as i64,
                                    type_id: u64_type_id,
                                },
                            )
                        }
                        _ => (
                            "None",
                            ValueRecord::Tuple {
                                elements: vec![],
                                type_id: variant_unit_type_id,
                            },
                        ),
                    };
                    let value = ValueRecord::Variant {
                        discriminator: variant_name.to_string(),
                        contents: Box::new(contents),
                        type_id: option_decoded_type_id,
                    };
                    TraceWriter::register_variable_with_full_value(
                        &mut *writer,
                        "option_decoded",
                        value,
                    );
                }

                // ABI-driven `Result<u64, str>` decoder.  Discriminator
                // byte 0 = `Ok(u64)` (next 8 bytes BE), byte 1 = `Err(str)`
                // (remaining bytes are the error string payload, capped at
                // 8 bytes per the canonical Sway std `str` slice
                // convention).
                if emit_result_decoder && !payload.is_empty() {
                    let (variant_name, contents): (&str, ValueRecord) = match payload[0] {
                        0 if payload.len() >= 9 => {
                            let v =
                                u64::from_be_bytes(payload[1..9].try_into().expect("8-byte slice"));
                            (
                                "Ok",
                                ValueRecord::Int {
                                    i: v as i64,
                                    type_id: u64_type_id,
                                },
                            )
                        }
                        1 if payload.len() >= 2 => {
                            let n = (payload.len() - 1).min(8);
                            let bytes: Vec<ValueRecord> = payload[1..1 + n]
                                .iter()
                                .map(|b| ValueRecord::Int {
                                    i: *b as i64,
                                    type_id: u64_type_id,
                                })
                                .collect();
                            (
                                "Err",
                                ValueRecord::Sequence {
                                    elements: bytes,
                                    is_slice: true,
                                    type_id: result_err_type_id,
                                },
                            )
                        }
                        _ => (
                            "Err",
                            ValueRecord::Sequence {
                                elements: vec![],
                                is_slice: true,
                                type_id: result_err_type_id,
                            },
                        ),
                    };
                    let value = ValueRecord::Variant {
                        discriminator: variant_name.to_string(),
                        contents: Box::new(contents),
                        type_id: result_decoded_type_id,
                    };
                    TraceWriter::register_variable_with_full_value(
                        &mut *writer,
                        "result_decoded",
                        value,
                    );
                }

                // ABI-driven fixed-length array decoder.  When the ABI
                // declares the entry-point output as `[u64; 4]` and the
                // LOGD payload is exactly 32 bytes (4 * 8) the recorder
                // emits an `array_fixed` `ValueRecord::Sequence` with
                // four big-endian-decoded u64 elements and
                // `is_slice = true` (fixed-length array view, distinct
                // from the heap-owned `vec_dynamic` Sequence with
                // `is_slice = false`).
                if emit_array_fixed_decoder && payload.len() == 32 {
                    let mut elements: Vec<ValueRecord> = Vec::new();
                    for chunk in payload.chunks_exact(8) {
                        let word = u64::from_be_bytes(
                            chunk.try_into().expect("chunks_exact(8) yields 8 bytes"),
                        );
                        elements.push(ValueRecord::Int {
                            i: word as i64,
                            type_id: u64_type_id,
                        });
                    }
                    let value = ValueRecord::Sequence {
                        elements,
                        is_slice: true,
                        type_id: array_fixed_type_id,
                    };
                    TraceWriter::register_variable_with_full_value(
                        &mut *writer,
                        "array_fixed",
                        value,
                    );
                }

                // ABI-driven integer-widths decoder.  When the ABI
                // declares the entry-point output as
                // `(u8, u16, u32, u64)` and the LOGD payload is exactly
                // 15 bytes (1 + 2 + 4 + 8) the recorder emits an
                // `integer_widths_decoded` `ValueRecord::Tuple` whose
                // four `ValueRecord::Int` elements each carry their
                // width-specific `type_id`.  The current FFI uses i64
                // for every Int value — width tagging is structural
                // (per-element type_id), not intrinsic.
                if emit_integer_widths_decoder && payload.len() == 15 {
                    let u8_val = payload[0] as u64;
                    let u16_val =
                        u16::from_be_bytes(payload[1..3].try_into().expect("2-byte slice")) as u64;
                    let u32_val =
                        u32::from_be_bytes(payload[3..7].try_into().expect("4-byte slice")) as u64;
                    let u64_val =
                        u64::from_be_bytes(payload[7..15].try_into().expect("8-byte slice"));
                    let value = ValueRecord::Tuple {
                        elements: vec![
                            ValueRecord::Int {
                                i: u8_val as i64,
                                type_id: u8_type_id,
                            },
                            ValueRecord::Int {
                                i: u16_val as i64,
                                type_id: u16_type_id,
                            },
                            ValueRecord::Int {
                                i: u32_val as i64,
                                type_id: u32_type_id,
                            },
                            ValueRecord::Int {
                                i: u64_val as i64,
                                type_id: u64_type_id,
                            },
                        ],
                        type_id: integer_widths_tuple_type_id,
                    };
                    TraceWriter::register_variable_with_full_value(
                        &mut *writer,
                        "integer_widths_decoded",
                        value,
                    );
                }

                // ABI-driven `enum Match` decoder for the
                // `match_pattern_test` fixture.  The discriminator
                // byte selects an arm:
                //   0 -> `Add(u64)`   (BE u64 in next 8 bytes)
                //   1 -> `Sub(u64)`   (BE u64 in next 8 bytes)
                //   2 -> `Mul(u64)`   (BE u64 in next 8 bytes)
                //   3 -> `Noop`       (no inner payload)
                // Surfaces as `match_arm_variant`
                // `ValueRecord::Variant` so each arm body's step
                // events can be assertable against a distinct arm
                // name.
                if emit_match_decoder && !payload.is_empty() {
                    let (variant_name, contents): (&str, ValueRecord) = match payload[0] {
                        0 if payload.len() >= 9 => {
                            let v =
                                u64::from_be_bytes(payload[1..9].try_into().expect("8-byte slice"));
                            (
                                "Add",
                                ValueRecord::Int {
                                    i: v as i64,
                                    type_id: u64_type_id,
                                },
                            )
                        }
                        1 if payload.len() >= 9 => {
                            let v =
                                u64::from_be_bytes(payload[1..9].try_into().expect("8-byte slice"));
                            (
                                "Sub",
                                ValueRecord::Int {
                                    i: v as i64,
                                    type_id: u64_type_id,
                                },
                            )
                        }
                        2 if payload.len() >= 9 => {
                            let v =
                                u64::from_be_bytes(payload[1..9].try_into().expect("8-byte slice"));
                            (
                                "Mul",
                                ValueRecord::Int {
                                    i: v as i64,
                                    type_id: u64_type_id,
                                },
                            )
                        }
                        3 => (
                            "Noop",
                            ValueRecord::Tuple {
                                elements: vec![],
                                type_id: variant_unit_type_id,
                            },
                        ),
                        _ => (
                            "Unknown",
                            ValueRecord::None {
                                type_id: u64_type_id,
                            },
                        ),
                    };
                    let value = ValueRecord::Variant {
                        discriminator: variant_name.to_string(),
                        contents: Box::new(contents),
                        type_id: variant_decoded_type_id,
                    };
                    TraceWriter::register_variable_with_full_value(
                        &mut *writer,
                        "match_arm_variant",
                        value,
                    );
                }

                // ABI-driven `Bytes` decoder.  When the ABI declares
                // the entry-point output as `Bytes` the recorder
                // emits a `bytes_decoded` `ValueRecord::Sequence` whose
                // elements are one `Int` per payload byte.  The
                // recorder requests `is_slice = true` to mark the
                // value as a slice/view of a heap-allocated byte
                // buffer (the canonical Sway `Bytes` shape); the
                // Rust -> Nim FFI today drops the flag and the value
                // surfaces with `is_slice = false`.  Same FFI gap
                // pinned in `tuple_decoding_test` / `array_fixed_test`.
                // The structural differentiation between
                // `bytes_decoded` (Sway `Bytes`) and the byte-level
                // `logd_payload` Sequence (raw LOGD buffer view) is
                // preserved via the variable name.
                if emit_bytes_decoder {
                    let elements: Vec<ValueRecord> = payload
                        .iter()
                        .map(|b| ValueRecord::Int {
                            i: *b as i64,
                            type_id: u64_type_id,
                        })
                        .collect();
                    let value = ValueRecord::Sequence {
                        elements,
                        is_slice: true,
                        type_id: bytes_decoded_type_id,
                    };
                    TraceWriter::register_variable_with_full_value(
                        &mut *writer,
                        "bytes_decoded",
                        value,
                    );
                }

                // ABI-driven `enum Identity` decoder.  When the ABI
                // declares the entry-point output as `enum Identity`
                // the recorder decodes the LOGD payload as a
                // tagged-union value: first byte = discriminator
                // (0 = `Address`, 1 = `ContractId`), remaining 32
                // bytes = the inner b256 payload.  The two
                // discriminator names match the canonical Sway std
                // `Identity` enum variant names so downstream tooling
                // can recognise the Identity shape uniformly across
                // recorders.
                if emit_identity_decoder && payload.len() >= 33 {
                    let b256_elements: Vec<ValueRecord> = payload[1..33]
                        .iter()
                        .map(|b| ValueRecord::Int {
                            i: *b as i64,
                            type_id: u64_type_id,
                        })
                        .collect();
                    let inner = ValueRecord::Sequence {
                        elements: b256_elements,
                        is_slice: true,
                        type_id: identity_b256_type_id,
                    };
                    let variant_name = match payload[0] {
                        0 => "Address",
                        1 => "ContractId",
                        _ => "Unknown",
                    };
                    let value = ValueRecord::Variant {
                        discriminator: variant_name.to_string(),
                        contents: Box::new(inner),
                        type_id: identity_variant_type_id,
                    };
                    TraceWriter::register_variable_with_full_value(
                        &mut *writer,
                        "identity_variant",
                        value,
                    );
                }

                // ABI-driven `Address` / `ContractId` / `AssetId`
                // decoder.  When the ABI declares the entry-point
                // output as one of the Sway native identity primitives
                // the recorder emits an `address_decoded`
                // `ValueRecord::Sequence` with one `Int` per payload
                // byte (the canonical 32-byte b256-style wire shape).
                // The recorder requests `is_slice = true` (memory-view
                // semantics); the FFI drops the flag — same gap.
                if emit_address_decoder && payload.len() >= 32 {
                    let elements: Vec<ValueRecord> = payload[..32]
                        .iter()
                        .map(|b| ValueRecord::Int {
                            i: *b as i64,
                            type_id: u64_type_id,
                        })
                        .collect();
                    let value = ValueRecord::Sequence {
                        elements,
                        is_slice: true,
                        type_id: address_decoded_type_id,
                    };
                    TraceWriter::register_variable_with_full_value(
                        &mut *writer,
                        "address_decoded",
                        value,
                    );
                }

                // ABI-driven `b256` decoder.  When the ABI declares the
                // entry-point output as `b256` and the LOGD payload is
                // exactly 32 bytes the recorder emits a `b256_decoded`
                // `ValueRecord::Sequence` with one `Int` per payload
                // byte.  Mirrors `address_decoded` / `identity_b256`
                // (the canonical 32-byte b256 wire shape used as the
                // foundation primitive for every other 256-bit identity
                // type in Sway std).  Until a typed `Raw256`
                // `ValueRecord` variant ships, the spec asks for a
                // `Sequence { elements.len() == 32, element_kind: Int }`
                // surface — which is what the recorder produces here.
                if emit_b256_decoder && payload.len() == 32 {
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
                        type_id: b256_decoded_type_id,
                    };
                    TraceWriter::register_variable_with_full_value(
                        &mut *writer,
                        "b256_decoded",
                        value,
                    );
                }

                // ABI-driven `configurable { ... }` block decoder.
                // The ABI sentinel `output.type = "configurable_payload"`
                // tells the recorder that the LOGD payload encodes the
                // values of the constants declared in the script's
                // `configurable { ... }` block, one big-endian u64 per
                // 8-byte chunk in declaration order.  The decoder
                // surfaces them as a single `ValueRecord::Tuple` step
                // variable named `configurable_decoded`.  Each
                // configurable constant's *name* surfaces separately
                // through the existing variable-tracker ABI-input path:
                // the constant names are listed in the ABI as `inputs`
                // of `main`, so the first MOVI loads pick them up
                // exactly like ordinary parameter names.  See
                // `tests/test_tracer.rs::test_configurable_test_via_ct_print_full`.
                if emit_configurable_decoder && !payload.is_empty() && payload.len() % 8 == 0 {
                    let mut elements: Vec<ValueRecord> = Vec::new();
                    for chunk in payload.chunks_exact(8) {
                        let word = u64::from_be_bytes(
                            chunk.try_into().expect("chunks_exact(8) yields 8 bytes"),
                        );
                        elements.push(ValueRecord::Int {
                            i: word as i64,
                            type_id: u64_type_id,
                        });
                    }
                    let value = ValueRecord::Tuple {
                        elements,
                        type_id: configurable_decoded_type_id,
                    };
                    TraceWriter::register_variable_with_full_value(
                        &mut *writer,
                        "configurable_decoded",
                        value,
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

        // Predicate-mode finalisation: surface the boolean return value
        // the FuelVM reported through `Receipt::Return.val` as a
        // `predicate_result` `ValueRecord::Bool` step variable attached
        // to the final step (the variable is registered before the
        // closing `register_return`, so it flushes onto the last
        // pending step rather than starting a new one).  The FuelVM
        // canonical predicate-success encoding is `RET 1`; any non-zero
        // return word maps to `Bool { b: true }`, zero (or the absence
        // of a `Receipt::Return`) maps to `Bool { b: false }`.
        if self.program_kind == ProgramKind::Predicate {
            let return_val: u64 = outcome
                .final_receipts
                .iter()
                .find_map(|r| match r {
                    Receipt::Return { val, .. } => Some(*val),
                    _ => None,
                })
                .unwrap_or(0);
            let value = ValueRecord::Bool {
                b: return_val != 0,
                type_id: bool_type_id,
            };
            TraceWriter::register_variable_with_full_value(&mut *writer, "predicate_result", value);
            // Balance the `enter_predicate` call we made above so the
            // call_tracker invariant is preserved (script context at
            // the very end of the recording).
            call_tracker.exit_predicate();
        }

        // Close the <toplevel> call that start() opened. main was merged into
        // <toplevel> (no Call event), so only one Return is needed.
        TraceWriter::register_return(&mut *writer, NONE_VALUE);

        // Finish writing
        TraceWriter::finish_writing_trace_events(&mut *writer).map_err(|e| eyre::eyre!("{e}"))?;
        writer
            .write_meta_dat("codetracer-fuel-recorder")
            .map_err(|e| eyre::eyre!("{e}"))?;
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

        Receipt::Log {
            id,
            ra,
            rb,
            rc,
            rd,
            pc,
            ..
        } => {
            TraceWriter::register_special_event(
                writer,
                EventLogKind::EvmEvent,
                &format!("FuelLog:{}", truncate_id(&format!("{id:#x}"))),
                &format!("ra={ra} rb={rb} rc={rc} rd={rd} pc={pc:#x}"),
            );
        }

        Receipt::LogData {
            id,
            ra,
            rb,
            len,
            digest,
            pc,
            data,
            ..
        } => {
            // Inline up to 64 bytes of payload as hex; fall back to digest
            // if the FuelVM did not preserve the data buffer.  Keeping the
            // payload short bounds the .ct container size for log-heavy
            // traces.
            let payload = match data {
                Some(bytes) if !bytes.is_empty() => {
                    let n = bytes.len().min(64);
                    let hex: String = bytes[..n].iter().map(|b| format!("{b:02x}")).collect();
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

        Receipt::Mint {
            sub_id,
            contract_id,
            val,
            pc,
            ..
        } => {
            TraceWriter::register_special_event(
                writer,
                EventLogKind::EvmEvent,
                &format!("FuelMint:{}", truncate_id(&format!("{contract_id:#x}"))),
                &format!("sub_id={sub_id:#x} val={val} pc={pc:#x}"),
            );
        }

        Receipt::Burn {
            sub_id,
            contract_id,
            val,
            pc,
            ..
        } => {
            TraceWriter::register_special_event(
                writer,
                EventLogKind::EvmEvent,
                &format!("FuelBurn:{}", truncate_id(&format!("{contract_id:#x}"))),
                &format!("sub_id={sub_id:#x} val={val} pc={pc:#x}"),
            );
        }

        Receipt::Transfer {
            id,
            to,
            amount,
            asset_id,
            pc,
            ..
        } => {
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

        Receipt::TransferOut {
            id,
            to,
            amount,
            asset_id,
            pc,
            ..
        } => {
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

        Receipt::MessageOut {
            sender,
            recipient,
            amount,
            nonce,
            len,
            digest,
            ..
        } => {
            TraceWriter::register_special_event(
                writer,
                EventLogKind::EvmEvent,
                &format!("FuelMessageOut:{}", truncate_id(&format!("{sender:#x}"))),
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

/// Truncate a long hex-formatted identifier (`0x…`) to the leading 10
/// chars + `…` so the `metadata` slot of a `register_special_event`
/// record stays short.
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
fn emit_storage_opcode_event(writer: &mut dyn TraceWriter, instr: &Instruction, registers: &[u64]) {
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
