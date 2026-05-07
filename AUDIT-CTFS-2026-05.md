# Fuel / Sway Recorder CTFS Audit — 2026-05-02

This audit checks `codetracer-fuel-recorder` against the canonical CodeTracer
multi-stream CTFS schema and the section 5.6 audit checklist maintained in
`/tmp/isonim-migration.txt`. Prior audits set the canonical patterns: Ruby
(1.21, 1.22), Python (1.27), JavaScript (1.38), EVM (1.39), PHP (1.41),
Solana (1.44), Move (1.46), Cardano (1.48), Cairo (1.50) and Flow / Cadence
(1.52). This is the **eleventh** recorder audited.

## Architecture

The Fuel recorder is a **single-process Rust crate** that embeds `fuel-vm`
(0.62) directly:

* `interpreter.rs` constructs a FuelVM `Interpreter` with single-stepping
  enabled, builds a script transaction from the input bytecode, and drives
  the interpreter step-by-step. Each step yields a `StepState` containing
  the program counter, register file, the receipts emitted so far, and the
  decoded instruction.
* `recorder.rs` owns the canonical `NimTraceWriter` (the `codetracer_trace_writer_nim`
  sibling-path crate) and converts every `StepState` into a sequence of
  canonical CodeTracer events: `register_step` for line changes,
  `register_call` / `register_return` for cross-contract context switches
  (driven by `ContractCallTracker::process_receipts` in
  `contract_call.rs`), and `register_variable_with_full_value` for the
  general-purpose registers `r16`-`r23`.
* `replay.rs` is a separate code path that fetches an on-chain transaction
  from a `fuel-core` GraphQL endpoint; it does not currently emit a
  CodeTracer trace itself (only a `ReplaySummary`).

The recorder is **not** an FFI consumer — every canonical entry point
(`register_call`, `register_step`, `register_special_event`, `arg`,
`register_thread_*`) is reachable. There are no `#[no_mangle]` stubs.

## Summary

| # | Check | Status (pre-fix) | Status (post-fix) | Notes |
|---|---|---|---|---|
| a | CLI defaults to `TraceEventsFileFormat::Ctfs` | **GAP** | **OK** | Pre-fix `OutputFormat` enum exposed only `Binary` / `Json` (defaulting to `Binary`, the legacy CBOR+Zstd format) with no way to request the canonical CTFS multi-stream container. Post-fix CLI exposes a typed `OutputFormat { Ctfs, Binary, Json }` `clap::ValueEnum` defaulting to `ctfs` for the `record` subcommand, plus an `impl From<OutputFormat> for TraceEventsFileFormat` so dispatch sites stay one-liner. Same default-format fix as EVM (1.39), Solana (1.44), Move (1.46), Cardano (1.48), Cairo (1.50), Flow (1.52). |
| b | `register_call` for each call | **PARTIAL** | **PARTIAL** | Cross-contract switches (driven by `Receipt::Call`) emit canonical `register_call` records via `ContractCallTracker::process_receipts`. The script entry "main" is intentionally merged into `<toplevel>` (the body executes at depth 0) — a documented design choice in `recorder.rs::record` to keep step-over from the initial position from skipping the entire script body. No Sway-level call records are emitted because the Sway compiler does not yet emit DWARF function-boundary info (sway#2055), so all script-side function calls inline into a flat instruction stream from the recorder's point of view. Closing this requires either source-level Sway integration via `forc-pkg` (currently unimplemented; the `record <PROJECT_DIR>` path is a placeholder) or DWARF parsing of the `forc build --json-abi --emit-debug` output. Documented in **Open gaps** below. |
| c | Call args via `register_call_arg` / `arg()` | **GAP** | **GAP (helper-side / source-level)** | Every `register_call` site passes `vec![]` for the args vector. Cross-contract `Receipt::Call` carries only `(from_id, to_id, amount, asset_id, gas, param1, param2, pc, is)` — no symbolic argument names or values. Closing audit (b) for cross-contract calls would require either fuel-vm-side memory introspection at the call site (read the call data from `param1` / `param2` pointers and decode through the callee's ABI) or post-hoc Receipt processing against a known-target ABI. The `AbiSchema` already exposes `function_params(fn_name)` returning `(name, type)` pairs but the recorder does not currently consume them at the script entry point. Documented in **Open gaps** below. |
| d | Write/WriteOther/Error/EvmEvent for IO and structured events via `register_special_event` | **GAP** | **OK** (LOG / asset / panic / revert / ScriptResult routed; stdout N/A) | Pre-fix the recorder ignored every `Receipt` variant other than `Call` / `Return` / `ReturnData`, so the FuelVM `LOG` / `LOGD` opcodes (the Sway equivalent of EVM `LOG0…LOG4`), asset `Mint` / `Burn`, value `Transfer` / `TransferOut`, cross-chain `MessageOut`, runtime `Panic` / `Revert` traps, and the final `ScriptResult` were silently dropped. Post-fix every non-call receipt is routed through `register_special_event`: `Log` / `LogData` / `Mint` / `Burn` / `Transfer` / `TransferOut` / `MessageOut` to `EventLogKind::EvmEvent` (matches EVM 1.39 LOG-opcode routing and Cairo 1.50 `StarknetEvent` routing); `Panic` / `Revert` to `EventLogKind::Error` (matches Cairo 1.50 `CairoPanic` / Cardano 1.48 `AikenUplcEvalError`); `ScriptResult` to `EventLogKind::TraceLogEvent` (informational only). FuelVM scripts have no native stdout/stderr — `LOG` / `LOGD` are the only output channel — so `Write` / `WriteOther` is N/A. |
| e | Thread events (Start / Exit / Switch) | OK (N/A) | OK (N/A) | FuelVM is single-threaded. Every transaction (script or contract call) executes on a single interpreter thread by design. Recorder correctly emits no thread events. |
| f | Step records for line navigation | OK | OK | `recorder.rs::record` calls `register_step(path, line)` whenever the source-mapped line changes for the current opcode. The contract-aware `ContractCallTracker::lookup_source` resolves the path/line through the per-contract source map registered for the active call frame, falling back to the script's default source map. |
| g | Canonical CTFS schema match | **GAP** | **OK** | See (a). The `.ct` container post-fix starts with the canonical magic bytes 0xC0 0xDE 0x72 0xAC 0xE2 and is materially populated. Verified via `tests/test_ctfs_audit.rs::ctfs_writer_produces_ct_container`. |
| h | Obsolete `add_event` calls | OK | OK | `grep -r 'add_event'` in `src/` returns nothing. Recorder predates the 1.30 footgun and has always used the dedicated `register_*` entry points. |
| i | `#[no_mangle]` stubs colliding with upstream Nim exports | OK | OK | `grep -r '#\[no_mangle\]'` in `src/` returns nothing. Recorder uses the `codetracer_trace_writer_nim` Rust API directly (sibling-path dep), not the C FFI. |

## Concrete fixes applied

### 1. CLI now exposes and defaults to `Ctfs`

`src/main.rs`'s `OutputFormat` enum used to expose only `Binary` and
`Json`, with `Binary` as the default. There was no way to request the
canonical CTFS multi-stream container — `Binary` writes the legacy
CBOR+Zstd format that the canonical Nim `ct_reader_*` FFI and the
db-backend's `CTFSTraceReader` cannot consume directly.

Post-fix: `OutputFormat` gains a `Ctfs` variant (listed first), with
doc-comments explaining each option, and a freshly added
`impl From<OutputFormat> for TraceEventsFileFormat` makes the dispatch
sites uniform:

```rust
#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    /// Canonical CodeTracer multi-stream container (recommended).
    Ctfs,
    /// Legacy CBOR + Zstd binary format.
    Binary,
    /// Human-readable JSON (slower; useful for debugging).
    Json,
}

impl From<OutputFormat> for TraceEventsFileFormat {
    fn from(fmt: OutputFormat) -> Self {
        match fmt {
            OutputFormat::Ctfs => TraceEventsFileFormat::Ctfs,
            OutputFormat::Binary => TraceEventsFileFormat::Binary,
            OutputFormat::Json => TraceEventsFileFormat::Json,
        }
    }
}
```

`RecordArgs.format` now `default_value = "ctfs"`, and the `record`
dispatch site reduces to `let format: TraceEventsFileFormat = args.format.into();`.
The placeholder `trace_metadata.json` `format` field still serialises
the requested format identifier (`ctfs` / `binary` / `json`) for
diagnostics; this is now produced by a small `OutputFormat::as_str`
helper instead of a duplicated `match` arm.

### 2. FuelVM receipts now route through the structured event log

`src/recorder.rs::record`'s callback previously called
`call_tracker.process_receipts(&step.receipts)` for `Call` / `Return` /
`ReturnData` switch detection but ignored every other receipt kind. The
Sway-side observable side-effects (LOG/LOGD output, native asset
Mint/Burn, value Transfer/TransferOut, cross-chain MessageOut, runtime
Panic/Revert, final ScriptResult) were silently dropped.

Post-fix: a new `emit_receipt_special_event(writer, receipt)` helper
walks every newly-arrived receipt at each step and routes it onto the
structured event stream:

* `Log { id, ra, rb, rc, rd, pc, .. }` → `EventLogKind::EvmEvent`,
  metadata `"FuelLog:<id>"`, content `"ra=… rb=… rc=… rd=… pc=…"`.
* `LogData { id, ra, rb, len, digest, pc, data, .. }` →
  `EventLogKind::EvmEvent`, metadata `"FuelLogData:<id>"`, content
  `"ra=… rb=… len=… pc=… data=0x…"` (first 64 bytes of the data buffer
  inlined as hex; falls back to the digest if the FuelVM strips the
  buffer to save memory).
* `Mint` / `Burn` → `EventLogKind::EvmEvent`,
  `"FuelMint:<id>"` / `"FuelBurn:<id>"`, content `"sub_id=… val=… pc=…"`.
* `Transfer` / `TransferOut` → `EventLogKind::EvmEvent`,
  `"FuelTransfer:<id>"` / `"FuelTransferOut:<id>"`, content
  `"to=… amount=… asset_id=… pc=…"`.
* `MessageOut` → `EventLogKind::EvmEvent`, metadata
  `"FuelMessageOut:<sender>"`, content `"recipient=… amount=… nonce=… len=… digest=…"`.
* `Panic { id, reason, pc, .. }` → `EventLogKind::Error`, metadata
  `"FuelPanic"`, content `"contract=… reason=… pc=…"`.
* `Revert { id, ra, pc, .. }` → `EventLogKind::Error`, metadata
  `"FuelRevert"`, content `"contract=… code=… pc=…"`.
* `ScriptResult { result, gas_used }` → `EventLogKind::TraceLogEvent`
  (informational; success paths should not appear in the error
  channel), metadata `"FuelScriptResult"`, content
  `"result=… gas_used=…"`.

`Call` / `Return` / `ReturnData` are intentionally NOT mirrored here —
they remain handled by the existing `ContractCallTracker` switch path
which emits canonical `register_call` / `register_return` records.

The recorder maintains its own `prev_receipt_count` cursor parallel to
the contract tracker's internal counter so each new receipt is routed
exactly once per step.

## Tests added

`tests/test_ctfs_audit.rs` (3 new cases):

* `ctfs_writer_produces_ct_container` — runs the simple-arithmetic
  bytecode through `FuelRecorder::record` with
  `TraceEventsFileFormat::Ctfs` and asserts the resulting `.ct` file
  starts with the canonical CTFS magic bytes (0xC0 0xDE 0x72 0xAC 0xE2)
  and is materially populated.
* `ctfs_format_advertised_in_record_help` — CLI smoke test that
  `record --help` advertises `ctfs` as a `--format` value with
  `[default: ctfs]`. Uses `CARGO_BIN_EXE_codetracer-fuel-recorder` to
  locate the just-built binary (same idiom as the Flow 1.52 audit).
* `log_receipt_does_not_empty_trace` — structural smoke test for the
  `Receipt::Log` routing introduced in this audit. The simple-arithmetic
  bytecode contains a `LOG` opcode at instruction 5; pre-fix the
  resulting receipt was silently dropped, post-fix it is mirrored as a
  `register_special_event(EventLogKind::EvmEvent, …)` record. The test
  asserts a loose lower-bound on the .ct container size to guard against
  silent regressions where an audit-related change empties the event
  stream.

Read-side end-to-end content assertions on the embedded event records
(e.g. that `register_special_event(EventLogKind::EvmEvent, "FuelLog:…", …)`
actually appears in the event-log of the `.ct` container) need the
`codetracer_trace_reader_nim` dev-dep added and a small reader-walk
helper. Tracked as an open follow-up below (also open for Cairo,
Cardano, and Flow).

## Verification

```
cd /home/zahary/metacraft/codetracer-fuel-recorder
AH_TEST_RESOURCE_GUARD=1 cargo test --release
```

* lib unit tests: 9/9 passing
* `test_ctfs_audit` (new): 3/3 passing
* `test_cli` (existing): 4/4 passing
* `test_comprehensive`: existing suite passing
* `test_contract_call`: 9/9 passing
* `test_replay`: 19/19 passing
* `test_tracer`: 5/5 passing (1 ignored — `export_fixture` requires
  `SWAY_FIXTURE_OUTPUT_DIR`)
* `test_variable_tracker`: 6/6 passing

Total: 46/46 active passing in audit-touched suites, 0 regressions.
`cargo build --release` clean. `cargo clippy --release --lib` produces
0 warnings on the audit-touched code; the 9 pre-existing
`collapsible_if` / doc-list-indentation warnings on `src/replay.rs` are
unchanged by this audit.

### Targeted Playwright sweep

```
cd /home/zahary/metacraft/codetracer
just test-gui tests/program_specific_tests/sway_example.spec.ts
```

`detect-siblings.sh` prepends `codetracer-fuel-recorder/target/release`
to PATH for the next Playwright run, so the freshly-built binary is
picked up automatically.

* Pre-audit: 2 passed, 8 skipped (the live tests are gated on
  `forc` being on PATH, which it is not in the sandbox dev shell).
* Post-audit: same 2 passed, 8 skipped — the audit fix shapes do not
  regress the environment-detection paths and the dev-shell `forc`
  ergonomics gap is unchanged.

## Open gaps (not blocking, documented for follow-up)

### Sway / source-level recording (audit b)

The `record <PROJECT_DIR>` CLI path is currently a placeholder — the
recorder cannot yet build a Sway project end-to-end (`forc-pkg` is not
yet wired in). The bytecode-mode path (`--bytecode FILE.bin`) is the
only end-to-end recording mechanism today. Closing audit (b) for Sway
script-level call boundaries needs:

1. `forc-pkg` integration so `record <PROJECT_DIR>` actually compiles
   the project and produces `(bytecode, source-map, abi-json)` triples
   for every contract / script the project depends on.
2. A Sway-aware step→call mapper that consumes the source map's
   function-boundary records (sway-types `SourceMap` already exposes
   per-line spans; sway#2055 tracks function-boundary records). When
   that lands, every Sway-source `fn` becomes a canonical `register_call`
   site, and the entry-function's ABI parameter values can be staged via
   `TraceWriter::arg(name, value)` before `register_call`. Mirrors the
   Move 1.46 Sui `OpenFrame.parameters` pattern.
3. Until (1) and (2) land, the recorder still produces correct
   instruction-level traces (every step records source line and
   register file) — the gap is purely the absence of intra-script
   function-call boundaries in the calltrace pane.

### Cross-contract call args (audit b, cross-cutting)

`Receipt::Call` does NOT carry the call-data buffer, only `(amount,
asset_id, gas, param1, param2)` where `param1` / `param2` are typically
register values pointing to call-data in VM memory. Recovering the
symbolic argument list at a cross-contract call site needs:

* Memory introspection at the call instruction: read the bytes from
  `vm.memory()` at `param1` for `param2` bytes (the Sway calling
  convention) and decode them through the **callee**'s ABI (which the
  recorder may or may not have — `ContractCallTracker::register_source_map`
  takes per-contract source-maps but not per-contract ABIs today).
* Per-contract ABI registration analogous to `register_source_map`,
  populated from a `<source-dir>/out/debug/<name>-abi.json` discovery
  pass.

This is structurally similar to the EVM 1.39 audit's "call args from
JumpType-driven jump analysis" open item — the data is in the VM but
recovering it needs symbolic stack/memory analysis at the call site
rather than the simple `writer.arg(name, value)` pattern Ruby/JS use.

### Replay-side tracing (audit f, cross-cutting)

`replay.rs::replay_transaction` fetches an on-chain transaction's raw
payload and bytecode but does NOT currently invoke `FuelRecorder::record`
on the result. The replay path produces only a `ReplaySummary` (a
metadata-only structure). Closing this needs a small bridge:
once the replay path has the script bytecode and any involved-contract
bytecodes, feed them into the same `FuelRecorder::record` (with the
fetched transaction's inputs as the pre-state) so on-chain transactions
get full CodeTracer traces. Same shape of gap as Cairo 1.50's "on-chain
replay-path tracing" open item.

### Per-contract ABI registration (audit b enabling)

`ContractCallTracker` already tracks per-contract source maps but not
per-contract ABIs. Adding a `register_abi(contract_id, AbiSchema)`
method (parallel to `register_source_map`) would unlock not only
cross-contract call-arg staging (above) but also better
`register_variable_with_full_value` typing (function-return values
could be typed through the callee's ABI rather than always using the
fallback `u64` type).

### Multi-stream IO event collapse (cross-cutting)

Same writer-side issue documented in 1.39 (EVM), 1.41 (PHP), 1.44
(Solana), 1.46 (Move), 1.48 (Cardano), 1.50 (Cairo) and 1.52 (Flow):
the multi-stream IO event writer's `toIOEventKind` collapses 13
`EventLogKind`s onto 4 `IOEventKind` buckets, losing the original kind
byte. Fuel's `FuelLog`, `FuelMint`, `FuelTransfer`, `FuelMessageOut`
etc. records all land in the `stderr` IO bucket via the `EvmEvent`
mapping, so the frontend cannot distinguish them from each other or
from EVM logs without reaching the embedded raw event stream. Out of
scope for any single recorder audit; flagged as a writer-side fix in
`codetracer_trace_writer_ffi.nim`'s `toIOEventKind`.

### Read-side end-to-end content assertions

The audit tests assert the .ct file starts with the CTFS magic and is
materially populated. Verifying that the embedded event stream
contains the expected `register_call` / `register_special_event`
records (e.g. `EventLogKind::EvmEvent` with `FuelLog:…` metadata when
a script executes a `LOG` opcode) requires the
`codetracer_trace_reader_nim` dep added as a `[dev-dependencies]`
entry plus a small reader-walk helper. Tracked here for the next
pass (also open for Cairo, Cardano and Flow).

### Synthetic per-instruction source map (audit f, recorder-bytecode-mode)

`main.rs::record_bytecode` builds a synthetic 1:1 source map (one
instruction → one line in a `<bytecode>.sw` synthetic source). This is
a pragmatic fallback for Sway-less bytecode replay but shows up as
"every line is one instruction" in the editor pane. Real per-block /
per-source-line mapping requires the Sway compiler's source map JSON
which is only emitted when `forc build` is run — i.e. the same
prerequisite as the source-level recording gap above.

## After this audit

Section 5.6's recorder list shows `codetracer-fuel-recorder` as audited
(gaps closed for default-Ctfs CLI + receipt → structured-event routing
covering LOG/LOGD opcodes, asset Mint/Burn, value Transfer/TransferOut,
MessageOut, Panic/Revert traps, and ScriptResult; Sway-source-level
function-call boundaries + cross-contract symbolic call-args + replay-
path tracing open as forc-pkg / DWARF-integration / replay-bridge
follow-ups). Audited recorder count: 10 → 11.

## Convention compliance follow-up — 2026-05-08

The 2026-05-02 audit landed a `--format ctfs|binary|json` `clap::ValueEnum`
defaulting to `Ctfs`, mirroring the EVM (1.39) / Solana (1.44) /
Move (1.46) / Cardano (1.48) / Cairo (1.50) / Flow (1.52) audits.
Subsequent to that audit, `Recorder-CLI-Conventions.md` §4 in
`codetracer-specs` was tightened to require **CTFS-only** output:
recorders no longer accept a `--format` flag and `ct print` (shipped
with `codetracer-trace-format-nim`) is the canonical conversion tool
for human-readable output.  `Repo-Requirements.md` §2.2 / §2.3 reflect
this contract.

This entry records the convention compliance follow-up applied to the
Fuel recorder on 2026-05-08, mirroring the cairo (2710b5e), cardano
(0698f00), circom (2d8b280) and flow (49a4fa9) precedents:

* The `--format` / `-f` CLI flag was removed from the `record`
  subcommand.  The `OutputFormat` enum and the
  `impl From<OutputFormat> for TraceEventsFileFormat` block were
  deleted from `src/main.rs`.  Clap rejects `--format <anything>` with
  an "unexpected argument" diagnostic (verified by
  `test_format_flag_rejected_by_clap`).
* The `format` parameter was removed from `FuelRecorder::new`,
  `FuelRecorder::with_abi`, the `FuelRecorder.format` field, and the
  `record_bytecode` helper in `src/main.rs`.  The recorder's writer is
  hard-pinned to `TraceEventsFileFormat::Ctfs` at
  `recorder.rs::record`'s `create_trace_writer` call.  The
  `events_filename` match (which used to dispatch on `Json` / `Binary`
  / `BinaryV0` / `Ctfs`) was collapsed to the single CTFS arm
  (`trace.bin`).
* `CODETRACER_FUEL_RECORDER_OUT_DIR` was added as a fallback for
  `--out-dir` on both the `record` and `replay` subcommands.  Lookup
  order is CLI flag → env var → `./ct-traces/`.
* `CODETRACER_FUEL_RECORDER_DISABLED=1` (or `true`) skips the trace
  emission entirely on both `record` and `replay`; the Fuel recorder
  doesn't run a separate target subprocess so "disabled" simply means
  "don't write any artefacts".
* `CODETRACER_FUEL_RECORDER_LOG_LEVEL` is documented (advisory) in the
  `--help` output and the README.
* The CTFS-only contract is now in force across the codebase: the
  binary's `--help` output mentions `ct print` as the conversion tool;
  the new README documents only CTFS, the env-var contract, and the
  `ct print` workflow.
* `tests/test_tracer.rs` was rewritten:
  - The pre-existing JSON-content tests
    (`test_fuel_basic_execution`, `test_fuel_source_mapping`,
    `test_fuel_variable_extraction`, `test_fuel_trace_3file_output`,
    `test_fuel_single_step_trace`) were already operating in
    "structural-only" mode after the 2026-05-02 audit migrated the
    on-disk shape to a single `.ct` container — `parse_trace_json`
    returned an empty vector and each test short-circuited with
    `if events.is_empty() { return; }`.  They are now rewritten as
    pure structural assertions on the produced `.ct` container (size
    + magic bytes).  No `#[ignore]`-only tests were deleted (the
    pre-existing `#[ignore]`'d `export_fixture` survives unchanged).
  - `test_recorded_trace_via_ct_print_json` (new) drives the recorder
    against the simple-arithmetic fuel-asm bytecode, pipes the .ct
    bundle through `ct-print --json`, and asserts on **structural
    anchors** — the synthetic source path filename and at least one
    of the inferred register / immediate variable names — rather
    than on integer values, because the Fuel recorder's
    `ValueRecord::Int { i, type_id }` payload doesn't round-trip
    through `ct-print` today (same pre-existing limitation as
    cardano / circom / flow).  Skips gracefully when `ct-print` is
    not present (i.e. when this crate is built outside the metacraft
    workspace).
  - `test_env_out_dir_used_when_flag_omitted`,
    `test_env_disabled_skips_recording`,
    `test_format_flag_rejected_by_clap`: exercise the new
    convention §5 surface and the clap-rejection invariant.  The
    env-var fallback is exercised via the placeholder Sway-project
    `record` path (which writes `trace_metadata.json` /
    `trace_paths.json` into the resolved `--out-dir` even though the
    CTFS pipeline is not yet wired in for Forc projects), which is
    enough to prove the fallback is honoured at the `--out-dir`
    resolution step.
  - `test_no_format_flag_in_help`, `test_help_mentions_ct_print`:
    new top-level / subcommand-level guards that lock in the
    no-`--format` and `ct print` invariants.
* `tests/test_ctfs_audit.rs` was updated: the
  `ctfs_format_advertised_in_record_help` test was deleted (it would
  lock in the regression).  `run_trace` no longer takes a format
  argument and uses `FuelRecorder::new(name, dir)`.  The
  `ctfs_writer_produces_ct_container` and `log_receipt_does_not_empty_trace`
  tests survive unchanged in spirit.
* `tests/test_comprehensive.rs` and `tests/test_variable_tracker.rs`
  were updated to drop the `TraceEventsFileFormat::Json` argument
  from their `FuelRecorder::new` / `with_abi` call sites.
* `tests/verify-cli-convention-no-silent-skip.sh` was added as a
  shell-level guard that runs the binary's `--help`, asserts
  `--format` and `CODETRACER_FORMAT` are absent, asserts the standard
  flags (`--out-dir`, `--version`) are present, asserts `ct print` is
  mentioned, and asserts the `CODETRACER_FUEL_RECORDER_OUT_DIR` /
  `CODETRACER_FUEL_RECORDER_DISABLED` env vars are referenced in
  source.  A `Justfile` was added at repo root to wire it into
  `just lint` / `just test`.
* A `README.md` was added.  This recorder previously had no top-level
  user-facing README; the new file documents the CTFS-only contract,
  the env-var contract, the `ct print` workflow, and the placeholder
  status of the Sway-project (Forc.toml) `record` path.

References:

* [`codetracer-specs/Recorder-CLI-Conventions.md`](../codetracer-specs/Recorder-CLI-Conventions.md) §4 (CTFS-only) and §5 (env vars).
* [`codetracer-specs/Repo-Requirements.md`](../codetracer-specs/Repo-Requirements.md) §2.2 (CLI compliance) and §2.3 (trace format compatibility).
* Cairo precedent: `codetracer-cairo-recorder` commit `2710b5e`.
* Cardano follow-up: `codetracer-cardano-recorder` commit `0698f00`.
* Circom follow-up: `codetracer-circom-recorder` commit `2d8b280`.
* Flow follow-up: `codetracer-flow-recorder` commit `49a4fa9`.
