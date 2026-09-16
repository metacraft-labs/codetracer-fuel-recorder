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

#### Record raw FuelVM bytecode

```bash
codetracer-fuel-recorder record --bytecode <FILE.bin> --out-dir <dir>
```

Reads the raw FuelVM bytecode from `FILE.bin`, executes it under
single-stepping, and writes a CTFS trace bundle to `--out-dir`.

#### Record a Sway project (placeholder)

```bash
codetracer-fuel-recorder record <PROJECT_DIR> --out-dir <dir>
```

`PROJECT_DIR` must contain a `Forc.toml`. End-to-end Sway-project
recording (forc-pkg integration) is not yet implemented; the placeholder
path writes `trace_metadata.json` / `trace_paths.json` to `--out-dir`
for backward compatibility. Use `--bytecode` for full CTFS recording
today.

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

| Module | Purpose |
|---|---|
| `main.rs` | CLI entry point (clap) |
| `recorder.rs` | Top-level recording orchestration; receipt → event-log routing |
| `interpreter.rs` | Single-stepping FuelVM driver |
| `source_map.rs` | Mapping between FuelVM PC and Sway source locations |
| `contract_call.rs` | Cross-contract Call/Return tracking via FuelVM receipts |
| `variable_tracker.rs` | Heuristic register → variable-name inference |
| `abi_decoder.rs` | Sway ABI JSON parsing for variable enrichment |
| `replay.rs` | fuel-core GraphQL replay |
| `graphql_debug.rs` | fuel-core debug API client |
| `lib.rs` | Public library API |

### Testing

```bash
cargo test
just test     # also runs verify-cli-convention-no-silent-skip.sh
```

Integration test programs live in `test-programs/`.

### Environment variables

The recorder respects the standard CodeTracer recorder env-var contract
defined in `Recorder-CLI-Conventions.md` §5:

| Variable | CLI equivalent | Description |
|---|---|---|
| `CODETRACER_FUEL_RECORDER_OUT_DIR` | `--out-dir` | Fallback output directory when `--out-dir` is omitted. The CLI flag always wins. |
| `CODETRACER_FUEL_RECORDER_DISABLED` | — | Set to `1` or `true` to run the recorder in pass-through mode (no trace artefacts written). |
| `CODETRACER_FUEL_RECORDER_LOG_LEVEL` | — | Recorder log verbosity (advisory; the Fuel recorder currently logs to stderr unconditionally). |

### Contributing

We'd be very happy if the community finds this useful, and if anyone wants to:

* Use and test the Fuel/Sway support of CodeTracer.
* Provide feedback and discuss alternative implementation ideas: in the
  issue tracker, or in our [discord](https://discord.gg/qSDCAFMP).
* Contribute code to enhance the Fuel/Sway support of CodeTracer.
* Provide [sponsorship](https://opencollective.com/codetracer), so we
  can hire dedicated full-time maintainers for this project.

### Legal info

LICENSE: Apache-2.0

Copyright (c) 2025 Metacraft Labs Ltd
