//! `alf` — CLI for the Agent Life Format.
//!
//! Export, import, validate, and sync AI agent data across frameworks.

mod adapter;
mod api_client;
mod commands;
mod config;
mod state;

use clap::{Parser, Subcommand};
use colored::Colorize;
use std::path::PathBuf;
use std::process;

#[derive(Parser)]
#[command(
    name = "alf",
    about = "Agent Life Format — portable backup, sync, and migration for AI agents",
    version,
    after_help = "Documentation: https://agent-life.ai\nSpecification: https://github.com/agent-life/agent-life-data-format"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Export an agent workspace to an .alf archive
    Export {
        /// Agent framework runtime (openclaw, zeroclaw)
        #[arg(short, long)]
        runtime: String,

        /// Path to the agent workspace directory
        #[arg(short, long)]
        workspace: PathBuf,

        /// Output .alf file path [default: ./<agent-name>.alf]
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Import an .alf archive into an agent workspace
    Import {
        /// Agent framework runtime (openclaw, zeroclaw)
        #[arg(short, long)]
        runtime: String,

        /// Path to the agent workspace directory
        #[arg(short, long)]
        workspace: PathBuf,

        /// Path to the .alf file to import
        alf_file: PathBuf,
    },

    /// Validate an .alf archive against the ALF specification
    Validate {
        /// Path to the .alf file to validate
        alf_file: PathBuf,
    },

    /// Incremental sync to the cloud
    Sync {
        /// Agent framework runtime (openclaw, zeroclaw)
        #[arg(short, long)]
        runtime: String,

        /// Path to the agent workspace directory
        #[arg(short, long)]
        workspace: PathBuf,
    },

    /// Download and restore from the cloud
    Restore {
        /// Agent framework runtime (openclaw, zeroclaw)
        #[arg(short, long)]
        runtime: String,

        /// Path to the agent workspace directory
        #[arg(short, long)]
        workspace: PathBuf,

        /// Agent ID to restore
        #[arg(short, long)]
        agent: String,
    },

    /// Authenticate with the agent-life service
    Login {
        /// API key (skip interactive login)
        #[arg(short, long)]
        key: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Export {
            runtime,
            workspace,
            output,
        } => commands::export::run(&runtime, &workspace, output.as_deref()),

        Command::Import {
            runtime,
            workspace,
            alf_file,
        } => commands::import::run(&runtime, &alf_file, &workspace),

        Command::Validate { alf_file } => commands::validate::run(&alf_file),

        Command::Sync {
            runtime,
            workspace,
        } => commands::sync::run(&runtime, &workspace),

        Command::Restore {
            runtime,
            workspace,
            agent,
        } => commands::restore::run(&runtime, &workspace, &agent),

        Command::Login { key } => commands::login::run(key.as_deref()),
    };

    if let Err(err) = result {
        eprintln!("{} {err:#}", "error:".red().bold());
        process::exit(1);
    }
}