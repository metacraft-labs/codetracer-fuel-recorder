## codetracer-fuel-recorder

A recorder for [Sway](https://fuellabs.github.io/sway/) smart contracts and
raw FuelVM bytecode that produces [CodeTracer](https://github.com/metacraft-labs/CodeTracer)
traces.

> **Note:** This project is in early development. APIs and trace formats may change.
> We welcome contributions and discussion!

### Overview

`codetracer-fuel-recorder` embeds the FuelVM (`fuel-vm` 0.62) directly,
single-steps a script transaction, and captures every step of execution
in the canonical CodeTracer CTFS multi-stream format. Receipts emitted
by the FuelVM (LOG/LOGD opcodes, native asset Mint/Burn, value transfers,
cross-chain MessageOut, runtime Panic/Revert traps, the final
ScriptResult) are routed onto the structured event log via
`register_special_event`.

The recorder also ships a `replay` subcommand that fetches an on-chain
transaction from a `fuel-core` GraphQL endpoint and replays it locally.

### Building

```bash
cargo build
```

Or enter the Nix dev shell first:

```bash
nix develop
cargo build
```

### Usage

#### Record a Sway project

```bash
codetracer-fuel-recorder record <PROJECT_DIR> --out-dir <dir>
```

`PROJECT_DIR` must contain a `Forc.toml`. The recorder builds it with
`forc` (0.70.x, which must be on `PATH`; the project's own `out/` is left
untouched) and records the result at source level. If `forc` is missing
or the build fails, the command fails.

#### Record forc-built bytecode

```bash
codetracer-fuel-recorder record --bytecode <project>/out/debug/<name>.bin --out-dir <dir>
```

Executes the bytecode under single-stepping and writes a CTFS trace
bundle to `--out-dir`. The debug symbols `forc build` writes next to the
bytecode (`debug_symbols.obj`, or a JSON map from `forc build -g
<file>.json`) are picked up automatically, or can be named with
`--debug-symbols <file>`. With them the trace records the Sway source
files and lines that execute, and a frame for every function call.
`--abi <file>` accepts the ABI JSON forc writes (`<name>-abi.json`).

What forc's debug symbols do not contain cannot be recorded: they map
instructions to source spans but describe no local variables, so no
locals are recorded; functions forc inlines (it inlines even in debug
builds) run in their caller's frame at the call site's line. Mark a
function `#[inline(never)]` to keep its own frame and lines.

Bytecode without any debug symbols is recorded against a disassembly
listing (`<name>.fuelasm`, one instruction per line) written to
`--out-dir`.

The recorder always writes traces in the canonical CodeTracer CTFS
multi-stream format. There is no `--format` flag — see "Converting
traces" below for human-readable output.

#### Replay an on-chain transaction

```bash
codetracer-fuel-recorder replay \
    --rpc-url http://localhost:4000/v1/graphql \
    --tx-id 0x<tx-id> \
    --out-dir <dir>
```

Fetches the transaction from a `fuel-core` GraphQL endpoint, extracts
involved contracts and their bytecode, and replays the transaction.

#### Converting traces to JSON / text

The recorder is CTFS-only. To convert a recorded `.ct` bundle to a
human-readable form, use `ct print` from
[`codetracer-trace-format-nim`](https://github.com/metacraft-labs/codetracer-trace-format-nim):

```bash
ct-print --json <recording-dir>/<program>.ct
```

`ct-print` accepts `--json`, `--json-events`, `--summary`, and
`--follow` modes; see its `--help` for details. This conversion path
is the canonical way to produce textual oracles for golden-snapshot
tests, debugging, and interop with non-CodeTracer tools.

### Architecture

The recorder is structured around the following modules in `src/`:

| Module                | Purpose                                                        |
| --------------------- | -------------------------------------------------------------- |
| `main.rs`             | CLI entry point (clap)                                         |
| `recorder.rs`         | Top-level recording orchestration; receipt → event-log routing |
| `interpreter.rs`      | Single-stepping FuelVM driver                                  |
| `source_map.rs`       | Mapping between FuelVM PC and Sway source locations            |
| `contract_call.rs`    | Cross-contract Call/Return tracking via FuelVM receipts        |
| `variable_tracker.rs` | Heuristic register → variable-name inference                   |
| `abi_decoder.rs`      | Sway ABI JSON parsing for variable enrichment                  |
| `replay.rs`           | fuel-core GraphQL replay                                       |
| `graphql_debug.rs`    | fuel-core debug API client                                     |
| `lib.rs`              | Public library API                                             |

### Testing

```bash
cargo test
just test     # also runs verify-cli-convention-no-silent-skip.sh
```

Integration test programs live in `test-programs/`.

### Environment variables

The recorder respects the standard CodeTracer recorder env-var contract
defined in `Recorder-CLI-Conventions.md` §5:

| Variable                             | CLI equivalent | Description                                                                                    |
| ------------------------------------ | -------------- | ---------------------------------------------------------------------------------------------- |
| `CODETRACER_FUEL_RECORDER_OUT_DIR`   | `--out-dir`    | Fallback output directory when `--out-dir` is omitted. The CLI flag always wins.               |
| `CODETRACER_FUEL_RECORDER_DISABLED`  | —              | Set to `1` or `true` to run the recorder in pass-through mode (no trace artefacts written).    |
| `CODETRACER_FUEL_RECORDER_LOG_LEVEL` | —              | Recorder log verbosity (advisory; the Fuel recorder currently logs to stderr unconditionally). |

### Contributing

We'd be very happy if the community finds this useful, and if anyone wants to:

- Use and test the Fuel/Sway support of CodeTracer.
- Provide feedback and discuss alternative implementation ideas: in the
  issue tracker, or in our [discord](https://discord.gg/qSDCAFMP).
- Contribute code to enhance the Fuel/Sway support of CodeTracer.
- Provide [sponsorship](https://opencollective.com/codetracer), so we
  can hire dedicated full-time maintainers for this project.

### Legal info

LICENSE: Apache-2.0

Copyright (c) 2025 Metacraft Labs Ltd
