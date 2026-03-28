//! CLI entry point for the CodeTracer Fuel recorder.
//!
//! Supports the `record` subcommand which builds a Sway project, executes it
//! on an embedded FuelVM instance with single-stepping, and writes the
//! CodeTracer trace output files.
//!
//! # Usage
//!
//! ```text
//! codetracer-fuel-recorder record <PROJECT_DIR> \
//!     -o <output-dir> \
//!     [-f binary|json]
//! ```

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use eyre::{Context, Result};

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
    /// Build and record a Sway project.
    ///
    /// Compiles the Sway project at PROJECT_DIR (must contain a Forc.toml),
    /// executes it on an embedded FuelVM instance, and writes the CodeTracer
    /// trace files to the output directory.
    Record(RecordArgs),

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
    project_dir: PathBuf,

    /// Directory where the trace files will be written.
    ///
    /// The directory will be created if it does not exist.
    #[arg(short = 'o', long = "out-dir", default_value = "./ct-traces/")]
    out_dir: PathBuf,

    /// Output format for the trace.
    #[arg(short = 'f', long = "format", default_value = "binary")]
    format: OutputFormat,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Record(args) => record(args),
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
// `record` implementation (stub)
// ---------------------------------------------------------------------------

/// Execute the `record` subcommand.
fn record(args: RecordArgs) -> Result<()> {
    // 1. Validate project directory and Forc.toml
    let project_dir = args
        .project_dir
        .canonicalize()
        .with_context(|| format!("project directory not found: {}", args.project_dir.display()))?;

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

    // 2. Recording not yet implemented
    eprintln!("Recording not yet implemented");

    // 3. Create output directory
    let out_dir = &args.out_dir;
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("cannot create output dir: {}", out_dir.display()))?;

    // 4. Write placeholder trace files
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
