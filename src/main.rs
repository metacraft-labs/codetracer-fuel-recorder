//! CLI entry point for the CodeTracer Fuel recorder.
//!
//! Supports the `record` subcommand which either:
//! - Builds a Sway project and executes it (when a project dir with Forc.toml is given)
//! - Executes raw FuelVM bytecode from a .bin file (when --bytecode is given)
//!
//! # Usage
//!
//! ```text
//! codetracer-fuel-recorder record <PROJECT_DIR> \
//!     -o <output-dir> \
//!     [-f binary|json]
//!
//! codetracer-fuel-recorder record --bytecode <FILE.bin> \
//!     -o <output-dir> \
//!     [-f binary|json]
//! ```

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use codetracer_trace_writer::TraceEventsFileFormat;
use eyre::{Context, Result};

use codetracer_fuel_recorder::abi_decoder::AbiSchema;
use codetracer_fuel_recorder::recorder::FuelRecorder;
use codetracer_fuel_recorder::replay::{self, ReplayConfig};
use codetracer_fuel_recorder::source_map::SwaySourceMap;

// ---------------------------------------------------------------------------
// CLI definition
// ---------------------------------------------------------------------------

/// CodeTracer Fuel recorder -- record Sway/FuelVM execution traces.
#[derive(Debug, Parser)]
#[command(
    name = "codetracer-fuel-recorder",
    version,
    about = "Record Sway smart-contract execution traces for CodeTracer"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Build and record a Sway project, or record raw FuelVM bytecode.
    ///
    /// When PROJECT_DIR is given (must contain a Forc.toml), compiles and
    /// executes the Sway project. When --bytecode is given, executes raw
    /// FuelVM bytecode from a .bin file.
    Record(RecordArgs),

    /// Replay an on-chain transaction from a fuel-core node.
    ///
    /// Fetches a transaction by ID from a fuel-core GraphQL API, extracts
    /// involved contracts, fetches their bytecode, and replays via dryRun
    /// or historical execution.
    Replay(ReplayArgs),

    /// Print version information.
    Version,
}

#[derive(Debug, Clone, ValueEnum)]
enum OutputFormat {
    Binary,
    Json,
}

#[derive(Debug, clap::Args)]
struct RecordArgs {
    /// Path to the Sway project directory (must contain Forc.toml).
    /// Not required when --bytecode is provided.
    project_dir: Option<PathBuf>,

    /// Path to a raw FuelVM bytecode file (.bin).
    #[arg(long = "bytecode")]
    bytecode: Option<PathBuf>,

    /// Directory where the trace files will be written.
    ///
    /// The directory will be created if it does not exist.
    #[arg(short = 'o', long = "out-dir", default_value = "./ct-traces/")]
    out_dir: PathBuf,

    /// Output format for the trace.
    #[arg(short = 'f', long = "format", default_value = "binary")]
    format: OutputFormat,

    /// Path to a Sway ABI JSON file for variable name enrichment.
    #[arg(long = "abi")]
    abi: Option<PathBuf>,

    /// GraphQL debug API endpoint for remote tracing fallback.
    ///
    /// When provided, the recorder can use fuel-core's GraphQL debug API
    /// instead of embedded fuel-vm execution. Requires fuel-core to be
    /// running with --debug. Note: this is slow (one round-trip per
    /// instruction) and is only recommended as a fallback.
    #[arg(long = "graphql-endpoint")]
    graphql_endpoint: Option<String>,
}

#[derive(Debug, clap::Args)]
struct ReplayArgs {
    /// GraphQL RPC URL of the fuel-core node.
    #[arg(long = "rpc-url", default_value = "http://localhost:4000/v1/graphql")]
    rpc_url: String,

    /// Transaction ID to replay (hex string with 0x prefix).
    #[arg(long = "tx-id")]
    tx_id: String,

    /// Directory containing forc build output for source maps.
    ///
    /// When provided, the replayer will look for source maps in
    /// `<source-dir>/out/debug/` to enable source-level debugging.
    /// Without this, only disassembly-level replay is available.
    #[arg(long = "source-dir")]
    source_dir: Option<PathBuf>,

    /// Directory where the replay output will be written.
    #[arg(short = 'o', long = "out-dir", default_value = "./ct-traces/")]
    out_dir: PathBuf,

    /// Enable historical execution (state rewind) for replay at the
    /// original block height. Requires fuel-core running with
    /// --historical-execution flag.
    #[arg(long = "historical-execution")]
    historical_execution: bool,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Record(args) => record(args),
        Commands::Replay(args) => run_replay(args),
        Commands::Version => {
            println!(
                "codetracer-fuel-recorder {}",
                env!("CARGO_PKG_VERSION")
            );
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// `record` implementation
// ---------------------------------------------------------------------------

/// Execute the `record` subcommand.
fn record(args: RecordArgs) -> Result<()> {
    let format = match args.format {
        OutputFormat::Binary => TraceEventsFileFormat::Binary,
        OutputFormat::Json => TraceEventsFileFormat::Json,
    };

    // Load ABI if provided
    let abi = if let Some(abi_path) = &args.abi {
        let abi_json = std::fs::read_to_string(abi_path)
            .with_context(|| format!("failed to read ABI file: {}", abi_path.display()))?;
        Some(AbiSchema::from_json(&abi_json)?)
    } else {
        None
    };

    if let Some(bytecode_path) = &args.bytecode {
        // Bytecode mode: read raw bytecode from .bin file
        return record_bytecode(bytecode_path, &args.out_dir, format, abi);
    }

    // Project dir mode: validate and record a Sway project
    let project_dir_arg = args.project_dir.ok_or_else(|| {
        eyre::eyre!("either PROJECT_DIR or --bytecode must be provided")
    })?;

    let project_dir = project_dir_arg
        .canonicalize()
        .with_context(|| format!("project directory not found: {}", project_dir_arg.display()))?;

    let forc_toml = project_dir.join("Forc.toml");
    if !forc_toml.exists() {
        return Err(eyre::eyre!(
            "no Forc.toml found in project directory: {}",
            project_dir.display()
        ));
    }

    eprintln!(
        "Project: {} ({})",
        project_dir.display(),
        forc_toml.display()
    );

    // Recording from Forc.toml not yet implemented (needs forc-pkg)
    eprintln!("Recording not yet implemented for Sway projects (use --bytecode for raw bytecode)");

    // Create output directory and write placeholder files (backwards compat)
    let out_dir = &args.out_dir;
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("cannot create output dir: {}", out_dir.display()))?;

    let metadata = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "recorder": "codetracer-fuel-recorder",
        "format": match args.format {
            OutputFormat::Binary => "binary",
            OutputFormat::Json => "json",
        },
        "status": "placeholder"
    });

    let metadata_path = out_dir.join("trace_metadata.json");
    std::fs::write(
        &metadata_path,
        serde_json::to_string_pretty(&metadata).unwrap(),
    )
    .with_context(|| format!("failed to write {}", metadata_path.display()))?;

    let paths = serde_json::json!({
        "project_dir": project_dir.to_string_lossy(),
        "sources": []
    });

    let paths_path = out_dir.join("trace_paths.json");
    std::fs::write(
        &paths_path,
        serde_json::to_string_pretty(&paths).unwrap(),
    )
    .with_context(|| format!("failed to write {}", paths_path.display()))?;

    eprintln!("Trace output written to {}", out_dir.display());
    eprintln!("  trace_metadata.json");
    eprintln!("  trace_paths.json");

    Ok(())
}

/// Record a trace from raw FuelVM bytecode.
fn record_bytecode(
    bytecode_path: &PathBuf,
    out_dir: &PathBuf,
    format: TraceEventsFileFormat,
    abi: Option<AbiSchema>,
) -> Result<()> {
    let bytecode = std::fs::read(bytecode_path)
        .with_context(|| format!("failed to read bytecode file: {}", bytecode_path.display()))?;

    let program_name = bytecode_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("fuel-program");

    // Create a synthetic source file path
    let source_path = bytecode_path.with_extension("sw");

    // Create a simple line mapping (one instruction per line)
    let num_instructions = bytecode.len() / 4;
    let entries: Vec<(usize, PathBuf, u32)> = (0..num_instructions)
        .map(|i| (i, source_path.clone(), (i + 1) as u32))
        .collect();
    let source_map = SwaySourceMap::from_line_mapping(entries);

    let recorder = if let Some(abi) = abi {
        FuelRecorder::with_abi(program_name, out_dir, format, abi)
    } else {
        FuelRecorder::new(program_name, out_dir, format)
    };
    recorder.record(bytecode, &source_map, &source_path)?;

    eprintln!("Trace output written to {}", out_dir.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// `replay` implementation
// ---------------------------------------------------------------------------

/// Execute the `replay` subcommand.
fn run_replay(args: ReplayArgs) -> Result<()> {
    let config = ReplayConfig {
        rpc_url: args.rpc_url,
        tx_id: args.tx_id,
        source_dir: args.source_dir,
        output_dir: args.out_dir,
        historical_execution: args.historical_execution,
    };

    let summary = replay::replay_transaction(&config)?;

    eprintln!();
    eprintln!("Replay complete:");
    eprintln!("  Transaction: {}", summary.tx_id);
    if let Some(height) = summary.block_height {
        eprintln!("  Block height: {}", height);
    }
    eprintln!("  Contracts: {}", summary.contract_ids.len());
    eprintln!(
        "  Bytecode fetched: {}",
        summary.contracts_with_bytecode
    );
    eprintln!("  Source maps: {}", summary.has_source_maps);
    eprintln!("  Dry run receipts: {}", summary.dry_run_receipts);

    Ok(())
}
