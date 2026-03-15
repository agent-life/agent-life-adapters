//! `alf` — CLI for the Agent Life Format.
//!
//! Export, import, validate, and sync AI agent data across frameworks.

mod adapter;
mod api_client;
mod commands;
mod config;
mod context;
pub mod output;
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
    disable_help_subcommand = true,
    after_help = "Documentation: https://agent-life.ai\nSpecification: https://github.com/agent-life/agent-life-data-format"
)]
struct Cli {
    /// Output human-readable text instead of JSON
    #[arg(long, global = true, env = "ALF_HUMAN")]
    human: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Export an agent workspace to an .alf archive
    #[command(long_about = "Export reads the agent workspace (SOUL.md, config, principals, etc.) \
        and writes a single .alf archive. Reads from the workspace path; writes to the given \
        output file or ./<agent-name>.alf by default.\n\n\
        Example: alf export -r openclaw -w ./my-agent -o backup.alf")]
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
    #[command(long_about = "Import unpacks an .alf file into the given workspace directory. \
        Reads the .alf file; writes SOUL.md, config, principals, and other files into the workspace.\n\n\
        Example: alf import -r openclaw -w ./restored-agent archive.alf")]
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
    #[command(long_about = "Validate checks the .alf file structure and contents against the \
        ALF spec. Does not modify any files.\n\n\
        Example: alf validate backup.alf")]
    Validate {
        /// Path to the .alf file to validate
        alf_file: PathBuf,
    },

    /// Incremental sync to the cloud
    #[command(long_about = "Sync exports the workspace to a temporary .alf, uploads it to the \
        agent-life service, and updates ~/.alf/state/{agent_id}.toml and the snapshot file \
        (~/.alf/state/{agent_id}-snapshot.alf). Use 'alf restore' to download later.\n\n\
        Example: alf sync -r openclaw -w ./my-agent")]
    Sync {
        /// Agent framework runtime (openclaw, zeroclaw)
        #[arg(short, long)]
        runtime: String,

        /// Path to the agent workspace directory
        #[arg(short, long)]
        workspace: PathBuf,
    },

    /// Download and restore from the cloud
    #[command(long_about = "Restore downloads the latest snapshot from the agent-life service \
        and imports it into the workspace. Reads state from ~/.alf/state/; writes to the workspace.\n\n\
        Example: alf restore -r openclaw -w ./my-agent -a <agent-id>")]
    Restore {
        /// Agent framework runtime (openclaw, zeroclaw)
        #[arg(short, long)]
        runtime: String,

        /// Path to the agent workspace directory
        #[arg(short, long)]
        workspace: PathBuf,

        /// Agent ID to restore (if omitted, uses the single tracked agent from ~/.alf/state/)
        #[arg(short, long)]
        agent: Option<String>,
    },

    /// Authenticate with the agent-life service
    #[command(long_about = "Login stores your API key in ~/.alf/config.toml (service.api_key). \
        Use -k to pass the key non-interactively.\n\n\
        Example: alf login -k <your-api-key>")]
    Login {
        /// API key (skip interactive login)
        #[arg(short, long)]
        key: Option<String>,
    },

    /// Check the runtime environment and report readiness to sync
    #[command(long_about = "Check inspects the OpenClaw (or ZeroClaw) environment and reports \
        whether alf can find the workspace, memory files, API key, and service. \
        Use this before sync to diagnose configuration issues.\n\n\
        Example: alf check -r openclaw\n\
        Example: alf check -r openclaw -w ~/custom-workspace")]
    Check {
        /// Agent framework runtime (openclaw, zeroclaw)
        #[arg(short, long)]
        runtime: String,

        /// Path to the agent workspace directory (auto-discovered if omitted)
        #[arg(short, long)]
        workspace: Option<PathBuf>,
    },

    /// Show help (overview, status, files, troubleshoot, or per-command)
    #[command(long_about = "Topics: overview (default), status, files, troubleshoot, or a command name \
        (export, import, sync, restore, validate, login, check). \
        Status output is JSON by default; use --human for text.")]
    Help {
        /// Topic: overview (default), status, files, troubleshoot, or a command name
        topic: Option<String>,

        /// Deprecated: JSON is now the default for status. Kept for backward compatibility.
        #[arg(long, hide = true)]
        json: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    if cli.human {
        std::env::set_var("ALF_HUMAN", "1");
    }

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
        } => commands::restore::run(&runtime, &workspace, agent.as_deref()),

        Command::Login { key } => commands::login::run(key.as_deref()),

        Command::Check {
            runtime,
            workspace,
        } => commands::check::run(&runtime, workspace.as_deref()),

        Command::Help { topic, json } => commands::help::run(topic.as_deref(), json),
    };

    if let Err(err) = result {
        let hint = error_hint(&err);
        if output::human_mode() {
            eprintln!("{} {err:#}", "error:".red().bold());
            if !hint.is_empty() {
                eprintln!("{}", hint);
            }
        } else {
            output::json_error(&format!("{err:#}"), &hint);
        }
        process::exit(1);
    }
}

/// One-line hint for known error kinds to guide users to fix or get more help.
fn error_hint(err: &anyhow::Error) -> String {
    let msg = err.to_string();
    if msg.contains("API key") || msg.contains("api_key") || msg.contains("Unauthorized") {
        return "Run 'alf login' to set an API key, or 'alf help troubleshoot' for more.".into();
    }
    if msg.contains("No agent ID specified") || msg.contains("no agents are tracked") {
        return "Run 'alf sync -r <runtime> -w <workspace>' first, or 'alf help status' to list agents.".into();
    }
    if msg.contains("Unknown runtime") {
        return "Supported runtimes: openclaw, zeroclaw. Run 'alf help troubleshoot' for more.".into();
    }
    if msg.contains("workspace") && (msg.contains("not found") || msg.contains("does not exist")) {
        return "Run 'alf help troubleshoot' for workspace and path guidance.".into();
    }
    String::new()
}