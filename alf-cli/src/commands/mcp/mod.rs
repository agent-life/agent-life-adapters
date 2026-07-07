//! `alf mcp serve` — a stdio MCP server inside the `alf` binary (WP-M2a/M2b).
//!
//! ## Protocol posture (design L8)
//! Built on rmcp v2.1.x. We declare [`ProtocolVersion::LATEST`] (2025-11-25) and
//! rely on rmcp's echo-negotiation: the server accepts whichever known revision
//! the client requests (2024-11-05 … 2026-07-28 RC). Feature floor is the
//! 2025-06-18 structured-output convention — every tool declares an
//! `outputSchema` (generated from the same serde structs the CLI prints, via
//! `schemars`) and returns `structuredContent` **plus** the serialized-JSON
//! `TextContent` block (rmcp's [`CallToolResult::structured`] does both). All
//! diagnostics go to **stderr**; the protocol owns stdout.
//!
//! ## Threading model
//! The subcommand owns a tokio runtime. Every tool defers the blocking CLI seam
//! (`Config::load`, the `reqwest::blocking` `ApiClient`, filesystem work) to
//! [`tokio::task::spawn_blocking`] — `reqwest::blocking` panics if driven from
//! inside an async runtime thread, so this indirection is mandatory, not
//! stylistic. [`call_blocking`] wraps the simple case; [`call_streaming`] adds
//! the mpsc→progress-notification bridge for the long-running tools
//! (`alf_sync`, `alf_restore`).
//!
//! ## Never call `run()`
//! The tools call the additive seams (`help::status_json`, `check::gather`,
//! `sync::run_one_agent`, `restore::run_for_mcp`, `export::dry_run_result`,
//! `add::track`, `vault::{add_core,list_core,delete_core}`, `agents::list_result`)
//! — never the printing `run()` functions or main's error path, both of which
//! write JSON to stdout and would corrupt the protocol stream.
//!
//! WP-M2b ships the full v1 tool surface (12 tools): `alf_status`, `alf_check`,
//! `alf_sync`, `alf_restore`, `alf_export_dry_run`, `alf_track`, `alf_configure`,
//! `alf_vault_add`, `alf_vault_list`, `alf_vault_delete`, `alf_agents_list`,
//! `alf_docs`. The watch loop + `alf_watch_set` (design §6) land in WP-M3;
//! `alf_agents_set` is deferred to v1.2 (design §16.1).

mod configure;
mod docs;

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Implementation, ProgressNotificationParam, ProtocolVersion, ServerCapabilities,
    ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_handler, tool_router, ErrorData, RoleServer, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::adapter;
use crate::commands::{add, agents, check, export, help, restore, sync, vault};
use crate::config::Config;
use crate::output::Progress;
use crate::schema::{StatusResult, WatchStatus};
use crate::vault_key::{self, VaultKeyArgs};
use crate::vault_migrate;
use alf_core::{RestoreMode, VaultKey};

/// The `instructions` preamble returned in the initialize result — the compact
/// SKILL.md-opening equivalent an MCP host shows to its agent.
const INSTRUCTIONS: &str = "\
This server gives you durable, portable memory continuity via ALF (the Agent \
Life Format). Your memories, identity, and credentials are exported from your \
workspace and synced to the agent-life cloud, so you keep the same self across \
restarts, machines, and framework migrations.

Start every session by calling `alf_status` — it reports whether ALF is \
configured (API key, tracked agents, per-agent cloud sync state). Then:
- `alf_check` runs a full pre-flight diagnostic and returns actionable issues.
- `alf_sync` performs an incremental sync (export → delta → upload); pass \
  `recover: true` to re-derive against cloud truth when a local base is missing.
- `alf_restore` restores from the cloud (head, or a read-only `at_sequence` \
  preview, or `dry_run` to list what would be written).
- `alf_configure` (generic runtime) sets the `.alf-map.json` that describes \
  where your memories live; `alf_track` opts an extra file into sync.
- `alf_vault_add`/`alf_vault_list`/`alf_vault_delete` manage the zero-knowledge \
  credentials vault (a key is generated on first add; back up the key file — \
  its fingerprint is returned, never its bytes).
- `alf_agents_list` shows the tracked agents; `alf_docs` returns deeper docs \
  for any topic (sync, restore, recovery, vault, map-file, …).

Syncing is safe and idempotent — run `alf_sync` after notable changes. \
Destructive and key-custody operations (force-first-sync, purge, vault \
rotate-key/decrypt, external-root blessing) are deliberately CLI/human \
ceremonies, not tools; `alf_docs` routes you to their runbooks. Write memories \
where your framework writes them, and map that location. Diagnostics are on \
stderr; this channel carries only protocol messages.";

/// The MCP server handler. Holds the per-server context (runtime + workspace +
/// pinned agent) captured at spawn; each tool reuses the CLI seams against it.
#[derive(Clone)]
pub struct AlfServer {
    runtime: String,
    workspace: Option<PathBuf>,
    agent: Option<String>,
    #[expect(dead_code, reason = "the tool_handler macro reads this router field")]
    tool_router: ToolRouter<Self>,
}

impl AlfServer {
    fn new(runtime: String, workspace: Option<PathBuf>, agent: Option<String>) -> Self {
        Self {
            runtime,
            workspace,
            agent,
            tool_router: Self::tool_router(),
        }
    }

    /// Owned clones of the per-server context, for moving into a `spawn_blocking`
    /// closure.
    fn owned(&self) -> (String, Option<PathBuf>, Option<String>) {
        (
            self.runtime.clone(),
            self.workspace.clone(),
            self.agent.clone(),
        )
    }
}

// ---------------------------------------------------------------------------
// Tool parameter structs
// ---------------------------------------------------------------------------

/// Tools that take no arguments still accept an (absent or empty) object; rmcp
/// defaults missing `arguments` to `{}`, which deserializes into this.
#[derive(Deserialize, JsonSchema, Default)]
struct NoParams {}

/// Parameters for `alf_sync`.
#[derive(Deserialize, JsonSchema, Default)]
struct SyncParams {
    /// Re-pull the cloud-reconstructed base and re-derive the delta against
    /// cloud truth. The unattended self-heal for a missing/diverged local base
    /// (recovery cases E4/E9). Defaults to false.
    #[serde(default)]
    recover: Option<bool>,
}

/// Parameters for `alf_restore`.
#[derive(Deserialize, JsonSchema, Default)]
struct RestoreParams {
    /// Restore the workspace as it was after this sequence — a read-only
    /// preview: `~/.alf/state` is not moved, so a later `alf_sync` is unaffected.
    /// Omit for a head restore.
    #[serde(default)]
    at_sequence: Option<u64>,
    /// List what a restore would write and touch nothing. Defaults to false.
    #[serde(default)]
    dry_run: Option<bool>,
    /// Memory restore mode for runtimes with a mutable per-agent store: `total`
    /// (exact, default) or `merge` (keep local-only rows). Ignored by file
    /// runtimes.
    #[serde(default)]
    mode: Option<String>,
}

/// Parameters for `alf_track`.
#[derive(Deserialize, JsonSchema)]
struct TrackParams {
    /// Path to track. Workspace-relative by default; with `external: true`, any
    /// path under a pre-blessed root (blessing itself stays a CLI ceremony).
    path: String,
    /// Track a file OUTSIDE the workspace. Requires a pre-blessed root and passes
    /// the non-overridable denylist; setting this true is your consent (the CLI's
    /// `--yes-external`). Defaults to false.
    #[serde(default)]
    external: Option<bool>,
}

/// Parameters for `alf_configure` (generic runtime only). Pass exactly one of
/// `map` (full replacement) or `patch` (deep merge).
#[derive(Deserialize, JsonSchema, Default)]
struct ConfigureParams {
    /// A complete `.alf-map.json` object to write (replaces the existing map).
    #[serde(default)]
    map: Option<serde_json::Value>,
    /// A partial object deep-merged into the existing map.
    #[serde(default)]
    patch: Option<serde_json::Value>,
}

/// Parameters for `alf_vault_add`.
#[derive(Deserialize, JsonSchema)]
struct VaultAddParams {
    /// Service identifier (e.g. "email", "openai").
    service: String,
    /// The secret value (API key, token, or password). Transits model context —
    /// identical to the CLI flow where the agent types it.
    secret: String,
    /// Optional account username / address (a plaintext descriptor).
    #[serde(default)]
    username: Option<String>,
    /// Optional plaintext label — the selector for list/delete. Defaults to the
    /// username.
    #[serde(default)]
    label: Option<String>,
    /// Optional plaintext description (visible to the sync service).
    #[serde(default)]
    description: Option<String>,
    /// Plaintext tags. An `alf-vault` tag is always added.
    #[serde(default)]
    tags: Vec<String>,
    /// Extra `key=value` pairs folded into the encrypted payload.
    #[serde(default)]
    fields: Vec<String>,
    /// Replace an existing record with the same label instead of duplicating it.
    #[serde(default)]
    update: Option<bool>,
}

/// Parameters for `alf_vault_delete`. Pass exactly one selector.
#[derive(Deserialize, JsonSchema, Default)]
struct VaultDeleteParams {
    /// Record UUID.
    #[serde(default)]
    id: Option<String>,
    /// Plaintext label.
    #[serde(default)]
    label: Option<String>,
    /// Service name.
    #[serde(default)]
    service: Option<String>,
}

/// Parameters for `alf_docs`.
#[derive(Deserialize, JsonSchema)]
struct DocsParams {
    /// The topic to look up (e.g. sync, restore, recovery, vault, rotate-key,
    /// force-first-sync, purge, agents, map-file, mcp).
    topic: String,
}

// ---------------------------------------------------------------------------
// Vault-tool result wrappers
// ---------------------------------------------------------------------------

/// The `alf_vault_add` result: the CLI's [`vault::AddResult`] plus — when this
/// call auto-generated a vault key — the key's fingerprint and path. Key bytes
/// are **never** returned (no `--stdout` analog exists in the server, by design).
#[derive(Serialize, JsonSchema)]
struct VaultAddResult {
    #[serde(flatten)]
    add: vault::AddResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    key_generated: Option<KeyGenInfo>,
}

/// A newly generated vault key's public descriptors — fingerprint + on-disk
/// path, never the key material.
#[derive(Serialize, JsonSchema)]
struct KeyGenInfo {
    fingerprint: String,
    path: String,
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[tool_router]
impl AlfServer {
    /// The single monitoring query: config + API-key presence + per-agent cloud
    /// sync state, plus the (M2a-stubbed) watch stanza.
    #[tool(
        name = "alf_status",
        description = "Report ALF configuration and per-agent cloud sync state. Call this first \
in every session. Returns config/API-key presence, tracked agents with their last-synced \
sequence, per-agent service reachability, and a watch-loop stanza (inactive until the watch \
loop ships).",
        output_schema = rmcp::handler::server::tool::schema_for_output::<StatusResult>()
            .expect("StatusResult is a valid output schema")
    )]
    async fn alf_status(
        &self,
        Parameters(_): Parameters<NoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        call_blocking(|| {
            Ok(StatusResult {
                status: help::status_json()?,
                watch: WatchStatus::default(),
            })
        })
        .await
    }

    /// Full pre-flight diagnostic — the same JSON as `alf check`.
    #[tool(
        name = "alf_check",
        description = "Run a full pre-flight diagnostic for this runtime + workspace: workspace \
resolution, resources, API key, service reachability, discovered agents, and vault parity. \
Returns issues (error/warning/info) with suggestions. Also runs agent discovery \
(information-only), exactly like `alf check`.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<check::CheckResult>()
            .expect("CheckResult is a valid output schema")
    )]
    async fn alf_check(
        &self,
        Parameters(_): Parameters<NoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (runtime, workspace, agent) = self.owned();
        call_blocking(move || check::gather(&runtime, workspace.as_deref(), agent.as_deref())).await
    }

    /// Incremental sync (export → reconcile → delta → upload). Emits progress
    /// notifications while the blocking sync runs, if the client supplied a token.
    #[tool(
        name = "alf_sync",
        description = "Incrementally sync this agent to the cloud: export the workspace, reconcile \
memory identities, compute a delta against the last snapshot, and upload it (auto-registering the \
agent on first sync). Pass recover:true to re-derive against cloud truth when the local base is \
missing or diverged. Safe and idempotent.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<sync::SyncResult>()
            .expect("SyncResult is a valid output schema")
    )]
    async fn alf_sync(
        &self,
        Parameters(params): Parameters<SyncParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let (runtime, workspace, agent) = self.owned();
        let recover = params.recover.unwrap_or(false);
        call_streaming(ctx, move |sink| {
            let (outcome, selected) = sync::run_one_agent(
                &runtime,
                workspace.as_deref(),
                agent.as_deref(),
                recover,
                /* force_first_sync: */ false,
                /* human: */ false,
                Progress::callback(sink),
            )?;
            Ok(sync::build_sync_result(outcome, &selected))
        })
        .await
    }

    /// Restore from the cloud: head, a read-only point-in-time preview
    /// (`at_sequence`), or a dry-run listing. Emits progress if a token is given.
    #[tool(
        name = "alf_restore",
        description = "Restore this agent from the cloud. Default restores the head of history and \
updates local sync state. Pass at_sequence:N for a read-only point-in-time preview (state is NOT \
moved). Pass dry_run:true to list what would be written without touching anything. mode is total \
(default) or merge for runtimes with a mutable per-agent store.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<restore::RestoreToolResult>()
            .expect("RestoreToolResult is a valid output schema")
    )]
    async fn alf_restore(
        &self,
        Parameters(params): Parameters<RestoreParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let (runtime, workspace, agent) = self.owned();
        let at_sequence = params.at_sequence;
        let dry_run = params.dry_run.unwrap_or(false);
        let mode = match params.mode.as_deref() {
            None | Some("total") => RestoreMode::Total,
            Some("merge") => RestoreMode::Merge,
            Some(other) => {
                return Ok(tool_error(&anyhow::anyhow!(
                    "invalid mode '{other}' (expected 'total' or 'merge')"
                )));
            }
        };
        // The server has no per-call vault-key flags; restore resolves a key from
        // ALF_VAULT_KEY / the per-runtime default file (generic credential restore
        // needs the operator to provide the key — see the handoff note).
        let key_args = VaultKeyArgs::default();
        call_streaming(ctx, move |sink| {
            restore::run_for_mcp(
                &runtime,
                workspace.as_deref(),
                agent.as_deref(),
                at_sequence,
                dry_run,
                mode,
                &key_args,
                Progress::callback(sink),
            )
        })
        .await
    }

    /// Preview what a sync would upload — the `alf export --dry-run` file list.
    #[tool(
        name = "alf_export_dry_run",
        description = "Preview the file set this agent's export would archive, without writing \
anything. Returns the agent name, memory-record count, and the raw file list with sizes. Use it \
to confirm the map/tracked files resolve as expected before syncing.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<export::ExportDryRunResult>()
            .expect("ExportDryRunResult is a valid output schema")
    )]
    async fn alf_export_dry_run(
        &self,
        Parameters(_): Parameters<NoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (runtime, workspace, agent) = self.owned();
        call_blocking(move || {
            export::dry_run_result(&runtime, workspace.as_deref(), agent.as_deref())
        })
        .await
    }

    /// Track a workspace file (or a blessed external file) so sync includes it.
    #[tool(
        name = "alf_track",
        description = "Opt a file into sync's include list (idempotent: added:false means it was \
already tracked). Workspace-relative by default. With external:true a file outside the workspace \
can be tracked, but only under a pre-blessed root and never on the sensitive denylist — blessing \
a new root stays a CLI/human ceremony.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<add::AddResult>()
            .expect("AddResult is a valid output schema")
    )]
    async fn alf_track(
        &self,
        Parameters(params): Parameters<TrackParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (runtime, workspace, agent) = self.owned();
        let external = params.external.unwrap_or(false);
        call_blocking(move || {
            add::track(
                &runtime,
                workspace.as_deref(),
                agent.as_deref(),
                &params.path,
                external,
            )
        })
        .await
    }

    /// Configure the generic runtime's `.alf-map.json` (validated read-modify-write).
    #[tool(
        name = "alf_configure",
        description = "Generic runtime only: set the .alf-map.json that describes which workspace \
files become memory records (and how they are chunked/tagged/dated). Pass exactly one of `map` \
(full replacement) or `patch` (deep merge). The result is validated before writing — an invalid \
configuration is rejected with nothing written.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<configure::ConfigureResult>()
            .expect("ConfigureResult is a valid output schema")
    )]
    async fn alf_configure(
        &self,
        Parameters(params): Parameters<ConfigureParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (runtime, workspace, _agent) = self.owned();
        call_blocking(move || {
            configure::configure(&runtime, workspace.as_deref(), params.map, params.patch)
        })
        .await
    }

    /// Add a credential to the zero-knowledge vault (auto-keygen on first use).
    #[tool(
        name = "alf_vault_add",
        description = "Encrypt a credential and append it to the agent's vault (Layer 4). The \
ciphertext syncs; the plaintext descriptors (service, label, tags) stay visible. On the first add \
with no key resolvable, a vault key is generated (0600) and its fingerprint + path returned — \
never the key bytes; back up that file. Pass update:true to replace a same-label record.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<VaultAddResult>()
            .expect("VaultAddResult is a valid output schema")
    )]
    async fn alf_vault_add(
        &self,
        Parameters(params): Parameters<VaultAddParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (runtime, workspace, agent) = self.owned();
        call_blocking(move || {
            vault_add_impl(&runtime, workspace.as_deref(), agent.as_deref(), params)
        })
        .await
    }

    /// List the vault's plaintext descriptors — no key touched.
    #[tool(
        name = "alf_vault_list",
        description = "List the plaintext descriptors (service, label, description, tags, \
algorithm) of every credential in the agent's vault. Never touches ciphertext or the key — use it \
to find a record to delete.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<vault::ListResult>()
            .expect("vault ListResult is a valid output schema")
    )]
    async fn alf_vault_list(
        &self,
        Parameters(_): Parameters<NoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (runtime, workspace, agent) = self.owned();
        call_blocking(move || vault_list_impl(&runtime, workspace.as_deref(), agent.as_deref()))
            .await
    }

    /// Delete one vault record by id, label, or service — no key needed.
    #[tool(
        name = "alf_vault_delete",
        description = "Remove a single credential from the agent's vault. Selecting works on \
plaintext descriptors so no key is needed. Pass exactly one of id, label, or service. Recoverable \
via a point-in-time restore of an earlier sequence.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<vault::DeleteResult>()
            .expect("vault DeleteResult is a valid output schema")
    )]
    async fn alf_vault_delete(
        &self,
        Parameters(params): Parameters<VaultDeleteParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (runtime, workspace, agent) = self.owned();
        call_blocking(move || {
            vault_delete_impl(&runtime, workspace.as_deref(), agent.as_deref(), params)
        })
        .await
    }

    /// List the tracked-agent mapping joined with sync state.
    #[tool(
        name = "alf_agents_list",
        description = "List the [[agents]] mapping (the agents `alf check` discovered) joined with \
each agent's sync state: runtime, alias, alf agent id, workspace, enabled flag, last-synced \
sequence, and whether a local snapshot exists.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<agents::ListResult>()
            .expect("agents ListResult is a valid output schema")
    )]
    async fn alf_agents_list(
        &self,
        Parameters(_): Parameters<NoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        // Match `alf agents` (no -r): span every runtime. Rows carry their own
        // runtime, so an agent can filter client-side.
        call_blocking(|| agents::list_result(None)).await
    }

    /// Progressive-disclosure documentation for a topic.
    #[tool(
        name = "alf_docs",
        description = "Return the documentation section for a topic instead of shipping a tool for \
every corner of the CLI. Topics include sync, restore, recovery, vault, rotate-key, \
force-first-sync, purge, agents, map-file, and mcp — the routing target for the CLI/human-only \
ceremonies that are deliberately not tools.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<docs::DocResult>()
            .expect("DocResult is a valid output schema")
    )]
    async fn alf_docs(
        &self,
        Parameters(params): Parameters<DocsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        call_blocking(move || docs::resolve(&params.topic)).await
    }
}

// ---------------------------------------------------------------------------
// Vault-tool blocking implementations (scope + key policy live here, not in the
// shared vault seams — the server owns the "which key, generated where" policy).
// ---------------------------------------------------------------------------

/// The vault scope (agent id whose per-agent vault/key paths apply) for the
/// pinned server: the CLI's strict vault scope, falling back to the
/// workspace-derived id on a first-contact empty mapping so the vault lands
/// under the id the next export will stamp.
fn server_vault_scope(
    config: &Config,
    runtime: &str,
    agent: Option<&str>,
    workspace: Option<&Path>,
) -> anyhow::Result<Option<Uuid>> {
    if let Some(id) = crate::selector::vault_scope_agent_id(config, runtime, agent)? {
        return Ok(Some(id));
    }
    // Empty mapping → derive from the workspace via the adapter (generic reads
    // `.alf-agent-id`; supported runtimes their own derivation).
    if let (Some(ws), Some(adapt)) = (workspace, adapter::get_adapter(runtime)) {
        return Ok(adapt.resolve_agent_id(ws).ok());
    }
    Ok(None)
}

/// The vault key path the server uses by default for a runtime + scope. Generic
/// gets a dedicated `~/.alf/vault-keys/{id}.key` (vault_key.rs deliberately has
/// no generic arm); supported runtimes reuse their existing per-runtime default.
fn server_default_key_path(runtime: &str, scope: Option<Uuid>) -> anyhow::Result<PathBuf> {
    if runtime == "generic" {
        let id = scope.ok_or_else(|| {
            anyhow::anyhow!(
                "cannot resolve the generic agent id for the vault key — pin --agent or set a \
                 workspace so the id can be derived"
            )
        })?;
        let home = alf_core::home_dir().context("Could not determine home directory")?;
        Ok(home
            .join(".alf")
            .join("vault-keys")
            .join(format!("{id}.key")))
    } else {
        vault_key::default_key_path(runtime, scope)?
            .ok_or_else(|| anyhow::anyhow!("no default vault key path for runtime '{runtime}'"))
    }
}

/// Resolve a vault key for the server, generating one at the default path if
/// none resolves. Resolution order (design §6, brief task 6): explicit
/// ALF_VAULT_KEY env / server key file → the default key path (existing) → a
/// freshly generated key at that path. Returns the args to pass through the
/// existing [`vault::add_core`] key seam, plus keygen descriptors when it
/// generated (fingerprint + path only, never bytes).
fn resolve_or_generate_vault_key(
    runtime: &str,
    scope: Option<Uuid>,
) -> anyhow::Result<(VaultKeyArgs, Option<KeyGenInfo>)> {
    let base = VaultKeyArgs::default();
    // A key already resolves (explicit env, or a per-runtime default file that
    // exists) → pass the args through unchanged; add_core re-resolves identically.
    if vault_key::resolve(&base, runtime, scope)?.is_some() {
        return Ok((base, None));
    }

    let path = server_default_key_path(runtime, scope)?;
    if path.is_file() {
        // A key file already exists at the default path (e.g. a prior generic
        // add) — use it, no regeneration.
        return Ok((
            VaultKeyArgs {
                key_file: Some(path),
                key_env: None,
            },
            None,
        ));
    }

    // Generate a fresh key at the default path (0600, private).
    let key = VaultKey::generate();
    let fingerprint = key.fingerprint();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    crate::fs_private::write_private(&path, &key.to_base64())
        .with_context(|| format!("writing vault key {}", path.display()))?;

    Ok((
        VaultKeyArgs {
            key_file: Some(path.clone()),
            key_env: None,
        },
        Some(KeyGenInfo {
            fingerprint,
            path: path.display().to_string(),
        }),
    ))
}

fn vault_add_impl(
    runtime: &str,
    workspace: Option<&Path>,
    agent: Option<&str>,
    params: VaultAddParams,
) -> anyhow::Result<VaultAddResult> {
    let config = Config::load()?;
    vault_migrate::require_migrated(&config, runtime)?;
    let scope = server_vault_scope(&config, runtime, agent, workspace)?;
    let (key_args, key_generated) = resolve_or_generate_vault_key(runtime, scope)?;

    let add = vault::add_core(
        None, // input: the agent's default vault path for this scope
        &params.service,
        "account", // credential_type default (matches `alf vault add`)
        params.username.as_deref(),
        Some(&params.secret),
        None, // secret_file
        None, // secret_json
        params.label.as_deref(),
        params.description.as_deref(),
        &params.tags,
        &params.fields,
        None, // agent_id metadata defaults to the scope
        scope,
        params.update.unwrap_or(false),
        &key_args,
        runtime,
    )?;

    Ok(VaultAddResult { add, key_generated })
}

fn vault_list_impl(
    runtime: &str,
    workspace: Option<&Path>,
    agent: Option<&str>,
) -> anyhow::Result<vault::ListResult> {
    let config = Config::load()?;
    vault_migrate::require_migrated(&config, runtime)?;
    let scope = server_vault_scope(&config, runtime, agent, workspace)?;
    vault::list_core(None, scope)
}

fn vault_delete_impl(
    runtime: &str,
    workspace: Option<&Path>,
    agent: Option<&str>,
    params: VaultDeleteParams,
) -> anyhow::Result<vault::DeleteResult> {
    let config = Config::load()?;
    vault_migrate::require_migrated(&config, runtime)?;
    let scope = server_vault_scope(&config, runtime, agent, workspace)?;
    let selector = vault::Selector {
        id: params.id,
        label: params.label,
        service: params.service,
    };
    vault::delete_core(None, &selector, None, scope)
}

// ---------------------------------------------------------------------------
// Blocking-seam bridges
// ---------------------------------------------------------------------------

/// Run a blocking seam on a `spawn_blocking` thread and map its result into a
/// dual (structuredContent + text) tool result. A seam error becomes a *tool*
/// error (`isError`); a panicked/cancelled worker becomes a protocol error.
async fn call_blocking<T>(
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<CallToolResult, ErrorData>
where
    T: Serialize + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(value)) => structured_ok(&value),
        Ok(Err(e)) => Ok(tool_error(&e)),
        Err(join) => Err(worker_failed(join)),
    }
}

/// Like [`call_blocking`], but bridges the seam's `Progress` lines to MCP
/// progress notifications: the blocking work pushes status lines into an
/// unbounded channel; a concurrent task drains them to `notify_progress` — only
/// when the client supplied a progress token in the request `_meta`.
async fn call_streaming<T>(
    ctx: RequestContext<RoleServer>,
    work: impl FnOnce(&(dyn Fn(&str) + Sync)) -> anyhow::Result<T> + Send + 'static,
) -> Result<CallToolResult, ErrorData>
where
    T: Serialize + Send + 'static,
{
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let progress_token = ctx.meta.get_progress_token();
    let peer = ctx.peer.clone();
    let drain = tokio::spawn(async move {
        let mut step = 0.0_f64;
        while let Some(message) = rx.recv().await {
            if let Some(token) = progress_token.clone() {
                step += 1.0;
                let _ = peer
                    .notify_progress(
                        ProgressNotificationParam::new(token, step).with_message(message),
                    )
                    .await;
            }
        }
    });

    let joined = tokio::task::spawn_blocking(move || {
        let forward = move |message: &str| {
            // Send failure only means the drain task is gone — nothing to do.
            let _ = tx.send(message.to_string());
        };
        work(&forward)
    })
    .await;
    // The blocking closure dropped `tx` on return, so the drain sees the channel
    // close and finishes after flushing the last messages.
    let _ = drain.await;

    match joined {
        Ok(Ok(value)) => structured_ok(&value),
        Ok(Err(e)) => Ok(tool_error(&e)),
        Err(join) => Err(worker_failed(join)),
    }
}

#[tool_handler]
impl ServerHandler for AlfServer {
    fn get_info(&self) -> ServerInfo {
        // Identify as `alf` at the CLI's version (from_build_env would report the
        // rmcp crate). ServerInfo/Implementation are #[non_exhaustive], so build
        // them through the provided constructors.
        let mut implementation = Implementation::from_build_env();
        implementation.name = "alf".into();
        implementation.version = env!("CARGO_PKG_VERSION").into();
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::LATEST)
            .with_instructions(INSTRUCTIONS)
            .with_server_info(implementation)
    }
}

/// Serialize a success value into a dual (structuredContent + serialized-JSON
/// TextContent) tool result. A serialization failure is a genuine internal
/// error (the value came from our own typed structs), so it maps to a protocol
/// error, not a tool error.
fn structured_ok<T: serde::Serialize>(value: &T) -> Result<CallToolResult, ErrorData> {
    let json = serde_json::to_value(value).map_err(|e| {
        ErrorData::internal_error(format!("failed to serialize tool result: {e}"), None)
    })?;
    Ok(CallToolResult::structured(json))
}

/// Map a seam error to a **tool execution error** (isError, not a protocol
/// error) carrying the CLI's `{ok:false, code?, error, hint}` shape as both
/// structured content and text — so the agent can self-correct (spec 2025-11-25
/// wants tool failures as tool errors).
fn tool_error(err: &anyhow::Error) -> CallToolResult {
    let mut obj = serde_json::Map::new();
    obj.insert("ok".into(), serde_json::Value::Bool(false));
    match err.downcast_ref::<crate::errors::CliError>() {
        Some(cli) => {
            obj.insert(
                "code".into(),
                serde_json::Value::String(cli.code.to_string()),
            );
            obj.insert("error".into(), serde_json::Value::String(cli.cause.clone()));
            if !cli.remedy.is_empty() {
                obj.insert("hint".into(), serde_json::Value::String(cli.remedy.clone()));
            }
        }
        None => {
            obj.insert(
                "error".into(),
                serde_json::Value::String(format!("{err:#}")),
            );
            let hint = crate::output::error_hint(err);
            if !hint.is_empty() {
                obj.insert("hint".into(), serde_json::Value::String(hint));
            }
        }
    }
    CallToolResult::structured_error(serde_json::Value::Object(obj))
}

/// A `spawn_blocking` task that panicked or was cancelled is an infrastructure
/// failure, not a tool result — surface it as a protocol internal error.
fn worker_failed(join: tokio::task::JoinError) -> ErrorData {
    ErrorData::internal_error(format!("alf worker task failed: {join}"), None)
}

/// Entry point for `alf mcp serve`. Owns the tokio runtime; runs the stdio
/// server until the client closes the connection (stdin EOF / SIGTERM). All
/// logging goes to stderr — stdout is the protocol stream.
pub fn serve(runtime: &str, workspace: Option<&Path>, agent: Option<&str>) -> anyhow::Result<()> {
    let server = AlfServer::new(
        runtime.to_string(),
        workspace.map(Path::to_path_buf),
        agent.map(str::to_string),
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime for the MCP server")?;

    rt.block_on(async move {
        eprintln!(
            "alf mcp serve: stdio server ready (runtime={})",
            server.runtime
        );
        let running = server
            .serve(rmcp::transport::io::stdio())
            .await
            .context("failed to start the stdio MCP server")?;
        let reason = running
            .waiting()
            .await
            .context("the MCP server terminated abnormally")?;
        eprintln!("alf mcp serve: stopped ({reason:?})");
        Ok(())
    })
}
