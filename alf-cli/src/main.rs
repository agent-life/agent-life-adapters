//! `alf` — CLI for the Agent Life Format.
//!
//! Export, import, validate, and sync AI agent data across frameworks.

mod adapter;
mod api_client;
mod commands;
mod config;
mod context;
mod fs_private;
pub mod output;
mod state;
mod vault_key;

use clap::{Parser, Subcommand};
use colored::Colorize;
use std::path::PathBuf;
use std::process;

use crate::vault_key::VaultKeyArgs;

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
    #[command(
        long_about = "Export reads the agent workspace (SOUL.md, config, principals, etc.) \
        and writes a single .alf archive. Reads from the workspace path; writes to the given \
        output file or ./<agent-name>.alf by default.\n\n\
        Example: alf export -r openclaw -w ./my-agent -o backup.alf"
    )]
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

        #[command(flatten)]
        key: VaultKeyCli,
    },

    /// Import an .alf archive into an agent workspace
    #[command(
        long_about = "Import unpacks an .alf file into the given workspace directory. \
        Reads the .alf file; writes SOUL.md, config, principals, and other files into the workspace.\n\n\
        Example: alf import -r openclaw -w ./restored-agent archive.alf"
    )]
    Import {
        /// Agent framework runtime (openclaw, zeroclaw)
        #[arg(short, long)]
        runtime: String,

        /// Path to the agent workspace directory
        #[arg(short, long)]
        workspace: PathBuf,

        /// Path to the .alf file to import
        alf_file: PathBuf,

        #[command(flatten)]
        key: VaultKeyCli,
    },

    /// Validate an .alf archive against the ALF specification
    #[command(
        long_about = "Validate checks the .alf file structure and contents against the \
        ALF spec. Does not modify any files.\n\n\
        --strict-crypto: credential records with algorithm \"none\" or unknown algorithms become errors.\n\n\
        Example: alf validate backup.alf\n\
        Example: alf validate --strict-crypto backup.alf"
    )]
    Validate {
        /// Path to the .alf file to validate
        alf_file: PathBuf,

        /// Treat weak/unknown credential crypto as errors (default: legacy metadata-only and unknown algorithms are warnings)
        #[arg(long)]
        strict_crypto: bool,
    },

    /// Incremental sync to the cloud
    #[command(
        long_about = "Sync exports the workspace to a temporary .alf, uploads it to the \
        agent-life service, and updates ~/.alf/state/{agent_id}.toml and the snapshot file \
        (~/.alf/state/{agent_id}-snapshot.alf). Use 'alf restore' to download later.\n\n\
        --recover: when the state file says we have synced before but the local base snapshot \
        is missing (e.g. an old CLI populated state without writing the base), pull the cloud \
        snapshot+deltas to repair the base, then take the normal delta path. Does not touch \
        the workspace.\n\n\
        --force-first-sync: required when the cloud already has an agent with this ID but no \
        local state exists; uploads the current workspace as a fresh snapshot, overwriting \
        cloud history. See docs/how_alf_syncs.md (case E3) before using this.\n\n\
        Example: alf sync -r openclaw -w ./my-agent\n\
        Example: alf sync --recover -r openclaw -w ./my-agent\n\n\
        When a vault key is resolved (same flags as export/import), the snapshot includes encrypted Layer 4 credentials."
    )]
    Sync {
        /// Agent framework runtime (openclaw, zeroclaw)
        #[arg(short, long)]
        runtime: String,

        /// Path to the agent workspace directory
        #[arg(short, long)]
        workspace: PathBuf,

        /// Pull cloud snapshot + deltas to repair a missing local base snapshot
        #[arg(long)]
        recover: bool,

        /// Allow first sync to overwrite an already-registered cloud agent
        #[arg(long)]
        force_first_sync: bool,

        #[command(flatten)]
        key: VaultKeyCli,
    },

    /// Download and restore from the cloud
    #[command(
        long_about = "Restore downloads a snapshot from the agent-life service and imports it \
        into the workspace.\n\n\
        Default: restores the head of history. Updates ~/.alf/state/ so the next `alf sync` \
        runs against the restored base.\n\n\
        --at-sequence N: point-in-time preview. Rebuilds the workspace as it looked after \
        sequence N was applied, WITHOUT touching ~/.alf/state/. Use this to inspect history; \
        run plain `alf restore` again to return to head. See docs/how_alf_syncs.md.\n\n\
        Example: alf restore -r openclaw -w ./my-agent -a <agent-id>\n\
        Example: alf restore --at-sequence 3 -r openclaw -w ./preview -a <agent-id>"
    )]
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

        /// Restore the workspace as it was after sequence N. Read-only preview;
        /// ~/.alf/state/ is not modified.
        #[arg(long, value_name = "N")]
        at_sequence: Option<u64>,

        #[command(flatten)]
        key: VaultKeyCli,
    },

    /// Remove cloud sync data and agent registration (does not delete local workspace files)
    #[command(
        long_about = "Purge calls DELETE /v1/agents/:id on the agent-life service: it removes all \
        snapshot and delta blobs in storage for this agent and deletes the agent row. It does not \
        delete files under the workspace. It resets ~/.alf/state/ for this agent so the next \
        `alf sync` uploads a full snapshot again.\n\n\
        Example: alf purge -r openclaw -w ./my-agent"
    )]
    Purge {
        /// Agent framework runtime (openclaw, zeroclaw)
        #[arg(short, long)]
        runtime: String,

        /// Path to the agent workspace directory (used for CLI consistency; not modified)
        #[arg(short, long)]
        workspace: PathBuf,

        /// Agent ID to purge (if omitted, uses the single tracked agent from ~/.alf/state/)
        #[arg(short, long)]
        agent: Option<String>,
    },

    /// Authenticate with the agent-life service
    #[command(
        long_about = "Login stores your API key in ~/.alf/config.toml (service.api_key). \
        Use -k to pass the key non-interactively.\n\n\
        Example: alf login -k <your-api-key>"
    )]
    Login {
        /// API key (skip interactive login)
        #[arg(short, long)]
        key: Option<String>,
    },

    /// Check the runtime environment and report readiness to sync
    #[command(
        long_about = "Check inspects the OpenClaw (or ZeroClaw) environment and reports \
        whether alf can find the workspace, memory files, API key, and service. \
        Use this before sync to diagnose configuration issues.\n\n\
        Example: alf check -r openclaw\n\
        Example: alf check -r openclaw -w ~/custom-workspace"
    )]
    Check {
        /// Agent framework runtime (openclaw, zeroclaw)
        #[arg(short, long)]
        runtime: String,

        /// Path to the agent workspace directory (auto-discovered if omitted)
        #[arg(short, long)]
        workspace: Option<PathBuf>,
    },

    /// Show help (overview, status, files, troubleshoot, or per-command)
    #[command(
        long_about = "Topics: overview (default), status, files, troubleshoot, or a command name \
        (export, import, sync, restore, purge, validate, login, check). \
        Status output is JSON by default; use --human for text."
    )]
    Help {
        /// Topic: overview (default), status, files, troubleshoot, or a command name
        topic: Option<String>,

        /// Deprecated: JSON is now the default for status. Kept for backward compatibility.
        #[arg(long, hide = true)]
        json: bool,
    },

    /// Manage the zero-knowledge credentials vault
    #[command(
        long_about = "Vault tooling for ALF Layer 4 (credentials). Each credential is\n\
        independently encrypted; plaintext descriptors (service, label, description,\n\
        tags) stay visible so an agent can find and delete records without ever\n\
        holding the vault key. See https://agent-life.ai/docs/cli for details."
    )]
    Vault {
        #[command(subcommand)]
        command: VaultCommand,
    },
}

#[derive(Subcommand)]
enum VaultCommand {
    /// Generate a fresh 32-byte vault key
    #[command(
        long_about = "Generates a cryptographically-random 32-byte vault key and writes\n\
        it as base64 to the given file with mode 0600. Pass --stdout to print the\n\
        key on stdout instead (use for piping into env vars on ephemeral hosts).\n\n\
        Example: alf vault keygen --out ~/.openclaw/state/.alf-vault-key"
    )]
    Keygen {
        /// File to write the base64-encoded key to (mode 0600)
        #[arg(short, long)]
        out: Option<PathBuf>,

        /// Print the base64 key on stdout (for env-var pipelines)
        #[arg(long)]
        stdout: bool,

        /// Overwrite an existing key file
        #[arg(long)]
        force: bool,
    },

    /// Encrypt a plaintext credential into a CredentialRecord
    #[command(
        long_about = "Reads plaintext JSON (a VaultPayload envelope) from --in or stdin,\n\
        encrypts it under the resolved vault key, and emits one CredentialRecord JSON\n\
        on stdout. If the input is not JSON, it is treated as a raw API key string.\n\n\
        Example: echo '{\"vault_payload_version\":1,\"kind\":\"login\",\"username\":\"kleo@agent-life.run\",\"secret\":\"...\"}' \\\n\
        | alf vault encrypt -r openclaw --service email --type account \\\n\
        --description agent-life.run --label kleo@agent-life.run"
    )]
    Encrypt {
        /// Plaintext input file. Stdin if omitted.
        #[arg(short = 'i', long = "in")]
        input: Option<PathBuf>,

        /// Service identifier (e.g., "openai", "email").
        #[arg(short, long)]
        service: String,

        /// Credential type: api_key, oauth_token, account, custom, ...
        #[arg(short = 't', long = "type", default_value = "custom")]
        credential_type: String,

        /// Optional plaintext description (visible to the sync service).
        #[arg(short, long)]
        description: Option<String>,

        /// Optional plaintext label.
        #[arg(short, long)]
        label: Option<String>,

        /// Plaintext tags (repeatable: --tag a --tag b).
        #[arg(long = "tag")]
        tags: Vec<String>,

        /// Capability names this credential enables.
        #[arg(long = "capability")]
        capabilities: Vec<String>,

        /// Agent UUID (defaults to the nil UUID for ad-hoc records).
        #[arg(short = 'a', long = "agent-id")]
        agent_id: Option<String>,

        /// Runtime for default key path resolution (openclaw, zeroclaw).
        #[arg(short, long, default_value = "openclaw")]
        runtime: String,

        #[command(flatten)]
        key: VaultKeyCli,
    },

    /// Decrypt one record and print its payload
    #[command(
        long_about = "Reads credentials.json (or the entry inside a .alf archive),\n\
        decrypts a single selected record under the resolved key, and prints the\n\
        plaintext envelope. Refuses to print to non-TTY stdout without --yes-insecure.\n\n\
        Select with one of: --id <UUID>, --label <STR>, --service <STR>.\n\n\
        Example: alf vault decrypt --in credentials.json --label kleo@agent-life.run"
    )]
    Decrypt {
        /// Path to credentials.json or .alf archive.
        #[arg(short = 'i', long = "in")]
        input: PathBuf,

        /// Record selector: UUID.
        #[arg(long)]
        id: Option<String>,

        /// Record selector: plaintext label.
        #[arg(short, long)]
        label: Option<String>,

        /// Record selector: service name.
        #[arg(short, long)]
        service: Option<String>,

        /// Runtime for default key path resolution.
        #[arg(short, long, default_value = "openclaw")]
        runtime: String,

        /// Allow printing plaintext to a non-TTY stdout.
        #[arg(long)]
        yes_insecure: bool,

        #[command(flatten)]
        key: VaultKeyCli,
    },

    /// List plaintext descriptors (no key required)
    #[command(
        long_about = "Lists the plaintext descriptor fields of every credential in\n\
        the given file or archive. Never touches ciphertext or keys — useful for\n\
        triage and for picking a record to surgically delete.\n\n\
        Example: alf vault list --in credentials.json"
    )]
    List {
        /// Path to credentials.json or .alf archive.
        #[arg(short = 'i', long = "in")]
        input: PathBuf,
    },

    /// Remove a single credential record (NO key required)
    #[command(
        long_about = "Drops one record from credentials.json. Selecting works on\n\
        plaintext descriptors so the agent never needs to decrypt anything — the\n\
        surgical-delete path for ephemeral hosts that provisioned an account on\n\
        the user's behalf and now needs to remove it.\n\n\
        Example: alf vault delete --in credentials.json --label kleo@agent-life.run"
    )]
    Delete {
        /// Path to credentials.json to mutate.
        #[arg(short = 'i', long = "in")]
        input: PathBuf,

        /// Record selector: UUID.
        #[arg(long)]
        id: Option<String>,

        /// Record selector: plaintext label.
        #[arg(short, long)]
        label: Option<String>,

        /// Record selector: service name.
        #[arg(short, long)]
        service: Option<String>,

        /// Write to a different file (default: overwrite --in).
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
}

/// Vault-key flags shared by `alf export`, `alf sync`, `alf import`, `alf restore`, and most `alf vault *`.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct VaultKeyCli {
    /// Explicit path to a base64-encoded 32-byte vault key file.
    #[arg(long, value_name = "PATH")]
    pub vault_key_file: Option<PathBuf>,

    /// Name of an env var holding a base64 vault key (default: ALF_VAULT_KEY).
    #[arg(long, value_name = "VAR")]
    pub vault_key_env: Option<String>,

    /// Path to a file containing a passphrase (Argon2id mode).
    #[arg(long, value_name = "PATH")]
    pub vault_passphrase_file: Option<PathBuf>,

    /// Name of an env var holding a passphrase (Argon2id mode).
    #[arg(long, value_name = "VAR")]
    pub vault_passphrase_env: Option<String>,

    /// Base64-encoded salt for passphrase mode (default: per-runtime constant).
    #[arg(long, value_name = "BASE64")]
    pub vault_salt: Option<String>,
}

impl VaultKeyCli {
    pub fn to_args(&self) -> VaultKeyArgs {
        VaultKeyArgs {
            key_file: self.vault_key_file.clone(),
            key_env: self.vault_key_env.clone(),
            passphrase_file: self.vault_passphrase_file.clone(),
            passphrase_env: self.vault_passphrase_env.clone(),
            salt_b64: self.vault_salt.clone(),
        }
    }
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
            key,
        } => commands::export::run(&runtime, &workspace, output.as_deref(), &key.to_args()),

        Command::Import {
            runtime,
            workspace,
            alf_file,
            key,
        } => commands::import::run(&runtime, &alf_file, &workspace, &key.to_args()),

        Command::Validate {
            alf_file,
            strict_crypto,
        } => commands::validate::run(&alf_file, strict_crypto),

        Command::Sync {
            runtime,
            workspace,
            recover,
            force_first_sync,
            key,
        } => commands::sync::run(
            &runtime,
            &workspace,
            recover,
            force_first_sync,
            &key.to_args(),
        ),

        Command::Restore {
            runtime,
            workspace,
            agent,
            at_sequence,
            key,
        } => commands::restore::run(
            &runtime,
            &workspace,
            agent.as_deref(),
            at_sequence,
            &key.to_args(),
        ),

        Command::Purge {
            runtime,
            workspace,
            agent,
        } => commands::purge::run(&runtime, &workspace, agent.as_deref()),

        Command::Login { key } => commands::login::run(key.as_deref()),

        Command::Check { runtime, workspace } => {
            commands::check::run(&runtime, workspace.as_deref())
        }

        Command::Help { topic, json } => commands::help::run(topic.as_deref(), json),

        Command::Vault { command } => dispatch_vault(command),
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

fn dispatch_vault(cmd: VaultCommand) -> anyhow::Result<()> {
    match cmd {
        VaultCommand::Keygen { out, stdout, force } => {
            commands::vault::keygen(out.as_deref(), force, stdout)
        }
        VaultCommand::Encrypt {
            input,
            service,
            credential_type,
            description,
            label,
            tags,
            capabilities,
            agent_id,
            runtime,
            key,
        } => commands::vault::encrypt(
            input.as_deref(),
            &service,
            &credential_type,
            description.as_deref(),
            label.as_deref(),
            &tags,
            &capabilities,
            agent_id.as_deref(),
            &key.to_args(),
            &runtime,
        ),
        VaultCommand::Decrypt {
            input,
            id,
            label,
            service,
            runtime,
            yes_insecure,
            key,
        } => commands::vault::decrypt(
            &input,
            &commands::vault::Selector {
                id,
                label,
                service,
            },
            &key.to_args(),
            &runtime,
            yes_insecure,
        ),
        VaultCommand::List { input } => commands::vault::list(&input),
        VaultCommand::Delete {
            input,
            id,
            label,
            service,
            out,
        } => commands::vault::delete(
            &input,
            &commands::vault::Selector {
                id,
                label,
                service,
            },
            out.as_deref(),
        ),
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
        return "Supported runtimes: openclaw, zeroclaw. Run 'alf help troubleshoot' for more."
            .into();
    }
    if msg.contains("workspace") && (msg.contains("not found") || msg.contains("does not exist")) {
        return "Run 'alf help troubleshoot' for workspace and path guidance.".into();
    }
    if msg.contains("Local delta base missing") {
        return "See docs/how_alf_syncs.md (case E4) for the recovery procedure.".into();
    }
    if msg.contains("already exists in the cloud") {
        return "See docs/how_alf_syncs.md (case E3) before using --force-first-sync.".into();
    }
    String::new()
}
