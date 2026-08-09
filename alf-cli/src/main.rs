//! `alf` — CLI for the Agent Life Format.
//!
//! Export, import, validate, and sync AI agent data across frameworks.

mod adapter;
mod api_client;
mod commands;
mod config;
mod context;
mod discovery;
mod errors;
mod fs_private;
pub mod output;
mod schema;
mod selector;
mod state;
mod vault_key;
mod vault_migrate;

#[cfg(test)]
mod doc_cli_lint;

use clap::{Parser, Subcommand};
use colored::Colorize;
use std::path::PathBuf;
use std::process;

use crate::config::Config;
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

    /// Agent to operate on: a runtime alias or an alf agent id from the
    /// [[agents]] mapping (falls back to ALF_AGENT, then the sole enabled agent)
    #[arg(long, global = true, value_name = "ALIAS_OR_ID")]
    agent: Option<String>,

    #[command(subcommand)]
    command: Command,
}

// clap subcommand enums are constructed once at startup, so the size spread
// between variants (some carry many flags) is not worth boxing.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Command {
    /// Export an agent workspace to an .alf archive
    #[command(
        long_about = "Export reads the agent workspace (SOUL.md, config, principals, etc.) \
        and writes a single .alf archive. Reads from the workspace path; writes to the given \
        output file or ./<agent-name>.alf by default.\n\n\
        Layer 4 (credentials) is the agent's explicit ALF vault (~/.alf/vault/credentials.json), \
        already AEAD-encrypted by `alf vault add` — export never reads a vault key.\n\n\
        Example: alf export -r openclaw -w ./my-agent -o backup.alf"
    )]
    Export {
        /// Agent framework runtime (openclaw, zeroclaw, hermes, generic)
        #[arg(short, long)]
        runtime: Option<String>,

        /// Path to the agent workspace directory
        #[arg(short, long)]
        workspace: Option<PathBuf>,

        /// Output .alf file path [default: ./<agent-name>.alf]
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Preview the files that would be archived without writing anything
        #[arg(long)]
        dry_run: bool,
    },

    /// Track an arbitrary workspace file so sync includes it
    #[command(
        long_about = "Add records a workspace file in the agent's include list \
        (.alf-include.json) so the next `alf sync` includes it under raw/openclaw/. \
        ALF does not auto-discover arbitrary files — the agent opts each one in \
        explicitly. The path is interpreted relative to the workspace.\n\n\
        Deleting the file and running `alf sync` removes it from the include list \
        and appends a note to .alf-sync-log.md.\n\n\
        Example: alf add notes.txt -r openclaw -w ./my-agent"
    )]
    Add {
        /// Path to the file to track (workspace-relative, or any path with --external)
        path: Option<String>,

        /// Agent framework runtime (openclaw, zeroclaw, hermes, generic)
        #[arg(short, long)]
        runtime: Option<String>,

        /// Path to the agent workspace directory
        #[arg(short, long)]
        workspace: Option<PathBuf>,

        /// Track a file OUTSIDE the workspace (D3). The file must resolve under a
        /// blessed root (see --allow-root) and is not on the sensitive denylist.
        #[arg(long)]
        external: bool,

        /// Bless a directory as an allowed root for --external adds (host-local
        /// policy, never written into archives). Can be used on its own.
        #[arg(long, value_name = "DIR")]
        allow_root: Option<PathBuf>,

        /// Skip the interactive confirm for an --external add (only honored when
        /// the target is already under a pre-blessed root).
        #[arg(long)]
        yes_external: bool,
    },

    /// Import an .alf archive into an agent workspace
    #[command(
        long_about = "Import unpacks an .alf file into the given workspace directory. \
        Reads the .alf file; writes SOUL.md, config, principals, and other files into the workspace.\n\n\
        Vault key: when a key resolves, Layer 4 ciphertext is decrypted and secrets are restored \
        into the runtime (e.g. auth profiles). When no key resolves, non-credential layers still \
        import; encrypted credentials are not written back — check warnings for \"pass … ALF_VAULT_KEY\" \
        or re-authenticate. Legacy metadata-only rows (<not-exported>) are skipped with warnings.\n\n\
        Example: alf import -r openclaw -w ./restored-agent archive.alf"
    )]
    Import {
        /// Agent framework runtime (openclaw, zeroclaw, hermes, generic)
        #[arg(short, long)]
        runtime: Option<String>,

        /// Path to the agent workspace directory
        #[arg(short, long)]
        workspace: Option<PathBuf>,

        /// Path to the .alf file to import
        alf_file: PathBuf,

        /// Memory restore mode for a live per-agent store (ZeroClaw brain.db):
        /// total (exact, default) or merge (keep local-only rows).
        #[arg(long, value_enum, default_value_t = RestoreModeArg::Total)]
        mode: RestoreModeArg,

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
        The agent to sync comes from --agent (alias or id), then the ALF_AGENT environment \
        variable, then the sole enabled agent in the [[agents]] mapping. --all syncs every \
        enabled agent sequentially. A first sync with an empty mapping discovers and maps \
        the install's agents automatically.\n\n\
        Example: alf sync -r openclaw -w ./my-agent\n\
        Example: alf sync --recover -r openclaw -w ./my-agent\n\n\
        Layer 4 (credentials) is the agent's explicit ALF vault, already AEAD-encrypted \
        by `alf vault add`; sync carries it as-is and never reads a vault key."
    )]
    Sync {
        /// Agent framework runtime (openclaw, zeroclaw, hermes, generic)
        #[arg(short, long)]
        runtime: Option<String>,

        /// Path to the agent workspace directory
        #[arg(short, long)]
        workspace: Option<PathBuf>,

        /// Sync every enabled agent (collects per-agent results; never fail-fast)
        #[arg(long, conflicts_with = "agent")]
        all: bool,

        /// Pull cloud snapshot + deltas to repair a missing local base snapshot
        #[arg(long)]
        recover: bool,

        /// Allow first sync to overwrite an already-registered cloud agent
        #[arg(long)]
        force_first_sync: bool,
    },

    /// Download and restore from the cloud
    #[command(
        long_about = "Restore downloads a snapshot from the agent-life service and imports it \
        into the workspace.\n\n\
        Default: restores the head of history. Updates ~/.alf/state/ so the next `alf sync` \
        runs against the restored base.\n\n\
        --at-sequence N: point-in-time preview. Materializes the merged archive as it looked \
        after sequence N into ~/.alf/preview/{agent}/seq-N/ (the three newest previews are \
        kept; the JSON result carries `preview_path`). The live workspace, -w, and \
        ~/.alf/state/ are untouched — no follow-up restore is needed. See \
        docs/how_alf_syncs.md.\n\n\
        Vault key (head restore): same behavior as `alf import` — with a resolved key, Layer 4 is \
        decrypted into the runtime; without a key, restore still applies other layers and warnings \
        explain that secrets were not restored.\n\n\
        Vault key (preview): a preview does NOT decrypt credentials unless --with-credentials is \
        passed, and NEVER writes the live ~/.alf/vault — the restored Layer 4 stays inside the \
        preview directory. Previews are pruned to the 3 newest per agent and expire after 24 h; \
        `alf purge` removes them all.\n\n\
        The agent comes from the global --agent (an alias from the [[agents]] mapping, or a \
        UUID — an unmapped UUID restores by id onto a fresh host), then ALF_AGENT, then the \
        sole enabled mapped agent, then the single tracked agent in ~/.alf/state/.\n\n\
        Example: alf restore -r openclaw -w ./my-agent --agent <alias-or-id>\n\
        Example: alf restore --at-sequence 3 -r openclaw --agent <alias-or-id>"
    )]
    Restore {
        /// Agent framework runtime (openclaw, zeroclaw, hermes, generic)
        #[arg(short, long)]
        runtime: Option<String>,

        /// Path to the agent workspace directory
        #[arg(short, long)]
        workspace: Option<PathBuf>,

        /// Restore the workspace as it was after sequence N. Read-only preview;
        /// ~/.alf/state/ is not modified.
        #[arg(long, value_name = "N")]
        at_sequence: Option<u64>,

        /// Preview the files that would be written; touches neither the workspace nor ~/.alf/state/
        #[arg(long)]
        dry_run: bool,

        /// Memory restore mode for a live per-agent store (ZeroClaw brain.db):
        /// total (exact, default) or merge (keep local-only rows).
        #[arg(long, value_enum, default_value_t = RestoreModeArg::Total)]
        mode: RestoreModeArg,

        /// Point-in-time previews only: also decrypt Layer 4 into the preview
        /// directory. Off by default — a preview is for inspecting history, and
        /// plaintext secrets should not outlive the inspection. The LIVE vault is
        /// never written by a preview either way.
        #[arg(long)]
        with_credentials: bool,

        #[command(flatten)]
        key: VaultKeyCli,
    },

    /// Remove cloud sync data and agent registration (does not delete local workspace files)
    #[command(
        long_about = "Purge calls DELETE /v1/agents/:id on the agent-life service: it removes all \
        snapshot and delta blobs in storage for this agent and deletes the agent row. It does not \
        delete files under the workspace. It resets ~/.alf/state/ for this agent so the next \
        `alf sync` uploads a full snapshot again.\n\n\
        The agent comes from the global --agent (alias or id), then ALF_AGENT, then the sole \
        enabled mapped agent, then the single tracked agent in ~/.alf/state/.\n\n\
        Example: alf purge -r openclaw -w ./my-agent"
    )]
    Purge {
        /// Agent framework runtime (openclaw, zeroclaw, hermes, generic)
        #[arg(short, long)]
        runtime: Option<String>,

        /// Path to the agent workspace directory (used for CLI consistency; not modified)
        #[arg(short, long)]
        workspace: Option<PathBuf>,
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
        Check also discovers the install's agents and records them in the [[agents]] \
        mapping in ~/.alf/config.toml. Discovery is information-only: it never changes \
        an existing agent's id or enabled flag — enabling is explicit (alf agents enable).\n\n\
        Example: alf check -r openclaw\n\
        Example: alf check -r openclaw -w ~/custom-workspace"
    )]
    Check {
        /// Agent framework runtime (openclaw, zeroclaw, hermes, generic)
        #[arg(short, long)]
        runtime: Option<String>,

        /// Path to the agent workspace directory (auto-discovered if omitted)
        #[arg(short, long)]
        workspace: Option<PathBuf>,
    },

    /// List discovered agents and manage which are enabled for sync
    #[command(
        long_about = "Agents lists the [[agents]] mapping in ~/.alf/config.toml — the agents \
        `alf check` discovered in this install — joined with each agent's sync state from \
        ~/.alf/state/. Subcommands enable or disable an agent for sync; both are idempotent. \
        Disabling keeps the cloud archive and local state.\n\n\
        Without -r, the list spans every runtime and enable/disable resolve the name across \
        all runtimes (erroring if it is ambiguous); -r scopes to one runtime.\n\n\
        Example: alf agents\n\
        Example: alf agents enable main\n\
        Example: alf agents -r zeroclaw enable default"
    )]
    Agents {
        /// Scope to one runtime (openclaw, zeroclaw, hermes, generic); default: all
        #[arg(short, long)]
        runtime: Option<String>,

        #[command(subcommand)]
        command: Option<AgentsCommand>,
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

    /// Run an MCP (Model Context Protocol) server over stdio
    #[command(
        long_about = "Serve ALF tools to an MCP-capable agent host over stdio. The host \
        spawns `alf mcp serve` as a subprocess and drives it with JSON-RPC on stdin/stdout; \
        all diagnostics go to stderr. The v1 surface is 13 tools: alf_status, alf_check, \
        alf_sync, alf_restore, alf_export_dry_run, alf_track, alf_configure, alf_vault_add, \
        alf_vault_list, alf_vault_delete, alf_agents_list, alf_docs, and alf_watch_set — each \
        with a declared outputSchema and structured results. A background watch loop auto-syncs \
        while the session is alive. Full reference: docs/cli-reference.md, or the alf_docs tool \
        with topic \"mcp\".\n\n\
        The runtime and workspace pin which agent this server operates on, exactly like the \
        other subcommands (global --agent selects among mapped agents).\n\n\
        Example: alf mcp serve -r generic -w ./my-agent"
    )]
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
}

#[derive(Subcommand)]
enum McpCommand {
    /// Serve ALF tools over stdio for an MCP-capable agent host
    Serve {
        /// Agent framework runtime (openclaw, zeroclaw, hermes, generic)
        #[arg(short, long)]
        runtime: Option<String>,

        /// Path to the agent workspace directory
        #[arg(short, long)]
        workspace: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum AgentsCommand {
    /// List mapped agents with their sync state (default)
    List,
    /// Enable an agent for sync (registers lazily on first sync)
    Enable {
        /// Agent alias or alf agent id
        agent: String,
    },
    /// Disable an agent (cloud archive and local state are kept)
    Disable {
        /// Agent alias or alf agent id
        agent: String,
    },
}

#[derive(Subcommand)]
enum VaultCommand {
    /// Generate a fresh 32-byte vault key
    #[command(
        long_about = "Generates a cryptographically-random 32-byte vault key and writes\n\
        it as base64 to the given file with mode 0600. Pass --stdout to print the\n\
        key on stdout instead (use for piping into env vars on ephemeral hosts).\n\n\
        Example: alf vault keygen --out ~/.openclaw/state/<alf-agent-id>/.alf-vault-key"
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

        /// Runtime for default key path resolution (openclaw, zeroclaw, hermes, generic).
        #[arg(short, long, default_value = "openclaw")]
        runtime: String,

        #[command(flatten)]
        key: VaultKeyCli,
    },

    /// Add an account credential to the agent's vault
    #[command(
        long_about = "Encrypts a credential under the resolved vault key and appends it\n\
        to the ALF vault. The default target is the selected agent's vault at\n\
        ~/.alf/vault/<alf-agent-id>/credentials.json, which `alf sync` merges into the\n\
        archive's encrypted Layer 4 and `alf restore` brings back on another host. The\n\
        vault is ALF's own store — separate from any runtime keystore — and the agent\n\
        chooses exactly what goes in it.\n\n\
        The secret may come from --secret, --secret-file, stdin, or --secret-json (a\n\
        JSON object whose user/password/token fields are mapped automatically). Every\n\
        record is tagged `alf-vault`.\n\n\
        Example: alf vault add -r openclaw --service email --type account \\\n\
        --secret-json /config/agent/.runtime-config/email.json \\\n\
        --label me@agent-life.run --tag agent-provisioned --update"
    )]
    Add {
        /// Vault file to append to [default: ~/.alf/vault/credentials.json]
        #[arg(short = 'i', long = "in")]
        input: Option<PathBuf>,

        /// Service identifier (e.g., "email", "telegram").
        #[arg(short, long)]
        service: String,

        /// Credential type: account, api_key, oauth_token, custom, ...
        #[arg(short = 't', long = "type", default_value = "account")]
        credential_type: String,

        /// Account username / address (plaintext descriptor).
        #[arg(short, long)]
        username: Option<String>,

        /// Secret value. Overrides --secret-file, --secret-json, and stdin.
        #[arg(long)]
        secret: Option<String>,

        /// File whose trimmed contents are the secret.
        #[arg(long)]
        secret_file: Option<PathBuf>,

        /// JSON object file; user/password/token fields are mapped automatically.
        #[arg(long)]
        secret_json: Option<PathBuf>,

        /// Plaintext label, the selector for list/decrypt/delete [default: username].
        #[arg(short, long)]
        label: Option<String>,

        /// Optional plaintext description (visible to the sync service).
        #[arg(short, long)]
        description: Option<String>,

        /// Plaintext tags (repeatable). An `alf-vault` tag is always added.
        #[arg(long = "tag")]
        tags: Vec<String>,

        /// Extra key=value pairs folded into the encrypted payload (repeatable).
        #[arg(long = "field")]
        fields: Vec<String>,

        /// Agent UUID (defaults to the nil UUID).
        #[arg(short = 'a', long = "agent-id")]
        agent_id: Option<String>,

        /// Replace an existing record with the same label instead of duplicating it.
        #[arg(long)]
        update: bool,

        /// Runtime for default key + vault path resolution (openclaw, zeroclaw, hermes).
        #[arg(short, long, default_value = "openclaw")]
        runtime: String,

        #[command(flatten)]
        key: VaultKeyCli,
    },

    /// Decrypt one record and print its payload
    #[command(
        long_about = "Reads the agent's vault (or --in credentials.json / .alf archive),\n\
        decrypts a single selected record under the resolved key, and prints the\n\
        plaintext envelope. Refuses to print to non-TTY stdout without --yes-insecure.\n\n\
        Select with one of: --id <UUID>, --label <STR>, --service <STR>.\n\n\
        Example: alf vault decrypt --label kleo@agent-life.run"
    )]
    Decrypt {
        /// Path to credentials.json or .alf archive [default: the agent's vault].
        #[arg(short = 'i', long = "in")]
        input: Option<PathBuf>,

        /// Record selector: UUID.
        #[arg(long)]
        id: Option<String>,

        /// Record selector: plaintext label.
        #[arg(short, long)]
        label: Option<String>,

        /// Record selector: service name.
        #[arg(short, long)]
        service: Option<String>,

        /// Runtime for default key + vault path resolution.
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
        the agent's vault (or --in file/archive). Never touches ciphertext or keys —\n\
        useful for triage and for picking a record to surgically delete.\n\n\
        Example: alf vault list\n\
        Example: alf vault list --in backup.alf"
    )]
    List {
        /// Path to credentials.json or .alf archive [default: the agent's vault].
        #[arg(short = 'i', long = "in")]
        input: Option<PathBuf>,

        /// Runtime for default vault path resolution (alias scoping).
        #[arg(short, long, default_value = "openclaw")]
        runtime: String,
    },

    /// Remove a single credential record (NO key required)
    #[command(
        long_about = "Drops one record from the agent's vault (or --in credentials.json).\n\
        Selecting works on plaintext descriptors so the agent never needs to decrypt\n\
        anything — the surgical-delete path for ephemeral hosts that provisioned an\n\
        account on the user's behalf and now needs to remove it.\n\n\
        Example: alf vault delete --label kleo@agent-life.run"
    )]
    Delete {
        /// Path to credentials.json to mutate [default: the agent's vault].
        #[arg(short = 'i', long = "in")]
        input: Option<PathBuf>,

        /// Record selector: UUID.
        #[arg(long)]
        id: Option<String>,

        /// Record selector: plaintext label.
        #[arg(short, long)]
        label: Option<String>,

        /// Record selector: service name.
        #[arg(short, long)]
        service: Option<String>,

        /// Write to a different file (default: overwrite the input).
        #[arg(short, long)]
        out: Option<PathBuf>,

        /// Runtime for default vault path resolution (alias scoping).
        #[arg(short, long, default_value = "openclaw")]
        runtime: String,
    },

    /// Rotate the vault key: re-encrypt every record under a new key
    #[command(
        name = "rotate-key",
        long_about = "Decrypts every record in the agent's vault under the OLD key\n\
        (resolved with the usual flags/default-file order) and re-encrypts under a\n\
        NEW key — a freshly generated one by default, or --new-key-file. Crash-safe:\n\
        the new key is persisted before the vault is rewritten, and an interrupted\n\
        run self-heals on the next invocation. Never prints key material; the JSON\n\
        carries fingerprints only.\n\n\
        Point-in-time restores of pre-rotation sequences always need the old key —\n\
        keep a copy until you no longer need that history.\n\n\
        Example: alf vault rotate-key -r openclaw --agent main"
    )]
    RotateKey {
        /// Path to credentials.json [default: the agent's vault].
        #[arg(short = 'i', long = "in")]
        input: Option<PathBuf>,

        /// Use an existing key file as the NEW key (default: generate one).
        #[arg(long, value_name = "PATH")]
        new_key_file: Option<PathBuf>,

        /// Write the new key here (0600; refuses to overwrite without --force).
        #[arg(long, value_name = "PATH")]
        new_key_out: Option<PathBuf>,

        /// Overwrite an existing --new-key-out file.
        #[arg(long)]
        force: bool,

        /// Runtime for default key + vault path resolution.
        #[arg(short, long, default_value = "openclaw")]
        runtime: String,

        #[command(flatten)]
        key: VaultKeyCli,
    },

    /// Move a legacy install-scoped vault to the per-agent layout
    #[command(
        long_about = "Moves the pre-multi-agent vault (~/.alf/vault/credentials.json)\n\
        and vault key (~/.<runtime>/state/.alf-vault-key) to their per-agent locations.\n\
        Runs automatically before vault/sync/export/import/restore when the target is\n\
        unambiguous; this command is the explicit escape hatch for ambiguous installs\n\
        (several enabled agents, cross-runtime evidence). Ciphertext moves verbatim —\n\
        no key is required. --dry-run reports the decision without writing.\n\n\
        Example: alf vault migrate -r openclaw --agent main"
    )]
    Migrate {
        /// Runtime whose legacy key path and agent mapping apply.
        #[arg(short, long, default_value = "openclaw")]
        runtime: String,

        /// Report the migration decision without writing anything.
        #[arg(long)]
        dry_run: bool,
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
}

impl VaultKeyCli {
    pub fn to_args(&self) -> VaultKeyArgs {
        VaultKeyArgs {
            key_file: self.vault_key_file.clone(),
            key_env: self.vault_key_env.clone(),
        }
    }
}

/// Memory restore mode for runtimes with a mutable per-agent store (ZeroClaw
/// `brain.db`). Ignored by file/markdown runtimes.
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default)]
pub enum RestoreModeArg {
    /// Replace the agent's slice so it exactly equals the archive (default).
    #[default]
    Total,
    /// Upsert the archive over the current slice, keeping local-only rows.
    Merge,
}

impl From<RestoreModeArg> for alf_core::RestoreMode {
    fn from(m: RestoreModeArg) -> Self {
        match m {
            RestoreModeArg::Total => alf_core::RestoreMode::Total,
            RestoreModeArg::Merge => alf_core::RestoreMode::Merge,
        }
    }
}

fn main() {
    let cli = Cli::parse();

    if cli.human {
        std::env::set_var("ALF_HUMAN", "1");
    }

    // The global --agent selector, threaded into every agent-scoped command.
    let agent_flag = cli.agent.clone();
    let agent = agent_flag.as_deref();

    let result = match cli.command {
        Command::Export {
            runtime,
            workspace,
            output,
            dry_run,
        } => (|| -> anyhow::Result<()> {
            let config = Config::load()?;
            let runtime = config.resolve_runtime(runtime);
            commands::export::run(
                &runtime,
                workspace.as_deref(),
                agent,
                output.as_deref(),
                dry_run,
            )
        })(),

        Command::Add {
            path,
            runtime,
            workspace,
            external,
            allow_root,
            yes_external,
        } => (|| -> anyhow::Result<()> {
            let config = Config::load()?;
            let runtime = config.resolve_runtime(runtime);
            commands::add::run(
                &runtime,
                workspace.as_deref(),
                agent,
                path.as_deref(),
                external,
                allow_root.as_deref(),
                yes_external,
            )
        })(),

        Command::Import {
            runtime,
            workspace,
            alf_file,
            mode,
            key,
        } => (|| -> anyhow::Result<()> {
            let config = Config::load()?;
            let runtime = config.resolve_runtime(runtime);
            commands::import::run(
                &runtime,
                &alf_file,
                workspace.as_deref(),
                agent,
                mode.into(),
                &key.to_args(),
            )
        })(),

        Command::Validate {
            alf_file,
            strict_crypto,
        } => commands::validate::run(&alf_file, strict_crypto),

        Command::Sync {
            runtime,
            workspace,
            all,
            recover,
            force_first_sync,
        } => (|| -> anyhow::Result<()> {
            let config = Config::load()?;
            let runtime = config.resolve_runtime(runtime);
            commands::sync::run(
                &runtime,
                workspace.as_deref(),
                agent,
                all,
                recover,
                force_first_sync,
            )
        })(),

        Command::Restore {
            runtime,
            workspace,
            at_sequence,
            dry_run,
            mode,
            key,
            with_credentials,
        } => (|| -> anyhow::Result<()> {
            let config = Config::load()?;
            let runtime = config.resolve_runtime(runtime);
            commands::restore::run(
                &runtime,
                workspace.as_deref(),
                agent,
                at_sequence,
                dry_run,
                mode.into(),
                &key.to_args(),
                with_credentials,
            )
        })(),

        Command::Purge { runtime, workspace } => (|| -> anyhow::Result<()> {
            let config = Config::load()?;
            let runtime = config.resolve_runtime(runtime);
            commands::purge::run(&runtime, workspace.as_deref(), agent)
        })(),

        Command::Login { key } => commands::login::run(key.as_deref()),

        Command::Check { runtime, workspace } => (|| -> anyhow::Result<()> {
            let config = Config::load()?;
            let runtime = config.resolve_runtime(runtime);
            commands::check::run(&runtime, workspace.as_deref(), agent)
        })(),

        // No -r ⇒ span all runtimes: rows are runtime-tagged, so scoping
        // to [defaults].runtime would strand zeroclaw/hermes rows (and
        // break the `alf agents enable <alias>` remedies sync emits).
        Command::Agents { runtime, command } => match command.unwrap_or(AgentsCommand::List) {
            AgentsCommand::List => commands::agents::list(runtime.as_deref()),
            AgentsCommand::Enable { agent } => commands::agents::enable(runtime.as_deref(), &agent),
            AgentsCommand::Disable { agent } => {
                commands::agents::disable(runtime.as_deref(), &agent)
            }
        },

        Command::Help { topic, json } => commands::help::run(topic.as_deref(), json),

        Command::Vault { command } => dispatch_vault(command, agent),

        Command::Mcp { command } => match command {
            // The MCP host owns stdout for the JSON-RPC protocol stream, so this
            // arm must NEVER yield into main's shared error path below: in
            // non-human mode (an MCP host never sets ALF_HUMAN) that path prints
            // a `{"ok":false,…}` JSON error to stdout, which would corrupt the
            // stream — including a `Config::load()` failure on a malformed
            // ~/.alf/config.toml, which would land as the first bytes the client
            // reads during `initialize`. Handle failure on stderr and exit here.
            McpCommand::Serve { runtime, workspace } => {
                let outcome = (|| -> anyhow::Result<()> {
                    let config = Config::load()?;
                    let runtime = config.resolve_runtime(runtime);
                    commands::mcp::serve(&runtime, workspace.as_deref(), agent)
                })();
                if let Err(err) = outcome {
                    eprintln!("alf mcp serve: {err:#}");
                    process::exit(1);
                }
                process::exit(0);
            }
        },
    };

    if let Err(err) = result {
        // Coded errors (WP0 agent-facing classes) carry their own remedy and a
        // machine-readable code; everything else keeps the legacy shape.
        if let Some(cli_err) = err.downcast_ref::<errors::CliError>() {
            if output::human_mode() {
                eprintln!("{} {}", "error:".red().bold(), cli_err.cause);
                if !cli_err.remedy.is_empty() {
                    eprintln!("{}", cli_err.remedy);
                }
            } else {
                output::json_error_coded(cli_err.code, &cli_err.cause, &cli_err.remedy);
            }
            process::exit(1);
        }
        let hint = output::error_hint(&err);
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

fn dispatch_vault(cmd: VaultCommand, agent: Option<&str>) -> anyhow::Result<()> {
    match cmd {
        // keygen touches no default paths — no scope, no migration trigger.
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
        } => {
            // encrypt only prints a record (no vault file) — lenient scope.
            let (config, scope) = vault_scope(&runtime, agent, /* strict: */ false)?;
            vault_migrate::require_migrated_locked(&config, &runtime)?;
            commands::vault::encrypt(
                input.as_deref(),
                &service,
                &credential_type,
                description.as_deref(),
                label.as_deref(),
                &tags,
                &capabilities,
                agent_id.as_deref(),
                scope,
                &key.to_args(),
                &runtime,
            )
        }
        VaultCommand::Add {
            input,
            service,
            credential_type,
            username,
            secret,
            secret_file,
            secret_json,
            label,
            description,
            tags,
            fields,
            agent_id,
            update,
            runtime,
            key,
        } => {
            // All caller-owned input, especially a bare stdin prompt, completes
            // before the default-vault L3 guard. An unattended TTY must not keep
            // the watch loop or other MCP tools agent_busy indefinitely.
            let prepared = input
                .is_none()
                .then(|| {
                    commands::vault::prepare_add_input(
                        username.as_deref(),
                        secret.as_deref(),
                        secret_file.as_deref(),
                        secret_json.as_deref(),
                        &fields,
                    )
                })
                .transpose()?;
            let (config, scope) = vault_scope(&runtime, agent, input.is_none())?;
            vault_migrate::require_migrated_locked(&config, &runtime)?;
            // Default-vault RMWs share the watch/MCP advisory lock. Explicit
            // --in targets remain caller-owned and lock-free.
            let _vault_lock = input
                .is_none()
                .then(|| commands::mcp::lock_default_vault_mutation(scope))
                .transpose()?;
            match prepared {
                Some(prepared) => commands::vault::add_prepared(
                    None,
                    &service,
                    &credential_type,
                    prepared,
                    label.as_deref(),
                    description.as_deref(),
                    &tags,
                    agent_id.as_deref(),
                    scope,
                    update,
                    &key.to_args(),
                    &runtime,
                ),
                None => commands::vault::add(
                    input.as_deref(),
                    &service,
                    &credential_type,
                    username.as_deref(),
                    secret.as_deref(),
                    secret_file.as_deref(),
                    secret_json.as_deref(),
                    label.as_deref(),
                    description.as_deref(),
                    &tags,
                    &fields,
                    agent_id.as_deref(),
                    scope,
                    update,
                    &key.to_args(),
                    &runtime,
                ),
            }
        }
        VaultCommand::Decrypt {
            input,
            id,
            label,
            service,
            runtime,
            yes_insecure,
            key,
        } => {
            let (config, scope) = vault_scope(&runtime, agent, input.is_none())?;
            vault_migrate::require_migrated_locked(&config, &runtime)?;
            commands::vault::decrypt(
                input.as_deref(),
                &commands::vault::Selector { id, label, service },
                scope,
                &key.to_args(),
                &runtime,
                yes_insecure,
            )
        }
        VaultCommand::List { input, runtime } => {
            let (config, scope) = vault_scope(&runtime, agent, input.is_none())?;
            vault_migrate::require_migrated_locked(&config, &runtime)?;
            commands::vault::list(input.as_deref(), scope)
        }
        VaultCommand::Delete {
            input,
            id,
            label,
            service,
            out,
            runtime,
        } => {
            let (config, scope) = vault_scope(&runtime, agent, input.is_none())?;
            vault_migrate::require_migrated_locked(&config, &runtime)?;
            // Only delete's default input + default output mutates canonical
            // shared state. An explicit --in/--out is caller-owned.
            let _vault_lock = (input.is_none() && out.is_none())
                .then(|| commands::mcp::lock_default_vault_mutation(scope))
                .transpose()?;
            commands::vault::delete(
                input.as_deref(),
                &commands::vault::Selector { id, label, service },
                out.as_deref(),
                scope,
            )
        }
        VaultCommand::RotateKey {
            input,
            new_key_file,
            new_key_out,
            force,
            runtime,
            key,
        } => {
            let (config, scope) = vault_scope(&runtime, agent, input.is_none())?;
            vault_migrate::require_migrated_locked(&config, &runtime)?;
            let _vault_lock = input
                .is_none()
                .then(|| commands::mcp::lock_default_vault_mutation(scope))
                .transpose()?;
            commands::vault::rotate_key(
                input.as_deref(),
                new_key_file.as_deref(),
                new_key_out.as_deref(),
                force,
                scope,
                &key.to_args(),
                &runtime,
            )
        }
        VaultCommand::Migrate { runtime, dry_run } => {
            let config = Config::load()?;
            commands::vault::migrate(&config, &runtime, agent, dry_run)
        }
    }
}

/// Vault scope resolution (WP1): the agent whose per-agent vault/key default
/// paths apply. Strict for commands consulting a default path (ambiguity is a
/// coded error, D9); lenient when the caller passed an explicit `--in` (the
/// default-file key step then simply won't resolve). The scope also supplies
/// the credential record's `agent_id` default — an explicit `--agent-id` only
/// overrides the record's metadata field, never the paths.
fn vault_scope(
    runtime: &str,
    agent: Option<&str>,
    strict: bool,
) -> anyhow::Result<(Config, Option<uuid::Uuid>)> {
    let config = Config::load()?;
    let scope = if strict {
        selector::vault_scope_agent_id(&config, runtime, agent)?
    } else {
        selector::vault_scope_agent_id_lenient(&config, runtime, agent)?
    };
    Ok((config, scope))
}
