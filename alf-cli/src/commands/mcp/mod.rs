//! `alf mcp serve` — a stdio MCP server inside the `alf` binary (WP-M2a).
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
//! stylistic.
//!
//! ## Never call `run()`
//! The tools call the additive seams (`help::status_json`, `check::gather`,
//! `sync::run_one_agent`) — never the printing `run()` functions or main's
//! error path, both of which write JSON to stdout and would corrupt the
//! protocol stream.
//!
//! WP-M2a ships exactly three tools: `alf_status`, `alf_check`, `alf_sync`. The
//! remaining surface (design §6) lands in WP-M2b/M3.

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
use serde::Deserialize;

use crate::commands::{check, help, sync};
use crate::output::Progress;
use crate::schema::{StatusResult, WatchStatus};

/// The `instructions` preamble returned in the initialize result — the compact
/// SKILL.md-opening equivalent an MCP host shows to its agent.
const INSTRUCTIONS: &str = "\
This server gives you durable, portable memory continuity via ALF (the Agent \
Life Format). Your memories, identity, and credentials are exported from your \
workspace and synced to the agent-life cloud, so you keep the same self across \
restarts, machines, and framework migrations.

Start every session by calling `alf_status` — it reports whether ALF is \
configured (API key, tracked agents, per-agent cloud sync state). Then:
- `alf_check` runs a full pre-flight diagnostic (workspace, resources, service \
  reachability, vault parity) and returns actionable issues + suggestions.
- `alf_sync` performs an incremental sync: it exports your workspace, computes \
  a delta against the last snapshot, and uploads it. Pass `recover: true` to \
  re-derive against cloud truth when a local base is missing or diverged.

Syncing is safe and idempotent — run `alf_sync` after you make notable changes \
to your memories or identity. Destructive and key-custody operations \
(force-first-sync, purge, vault rotate-key/decrypt) are deliberately CLI/human \
ceremonies, not tools. Additional tools (restore, vault, track, and `alf_docs` \
for deeper documentation) are part of the ALF MCP surface. Diagnostics are on \
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
}

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
        match tokio::task::spawn_blocking(help::status_json).await {
            Ok(Ok(status)) => structured_ok(&StatusResult {
                status,
                watch: WatchStatus::default(),
            }),
            Ok(Err(e)) => Ok(tool_error(&e)),
            Err(join) => Err(worker_failed(join)),
        }
    }

    /// Full pre-flight diagnostic — the same JSON as `alf check`, including
    /// discovery, vault parity, and the issues/suggestions list.
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
        let runtime = self.runtime.clone();
        let workspace = self.workspace.clone();
        let agent = self.agent.clone();
        let joined = tokio::task::spawn_blocking(move || {
            check::gather(&runtime, workspace.as_deref(), agent.as_deref())
        })
        .await;
        match joined {
            Ok(Ok(result)) => structured_ok(&result),
            Ok(Err(e)) => Ok(tool_error(&e)),
            Err(join) => Err(worker_failed(join)),
        }
    }

    /// Incremental sync (export → reconcile → delta → upload). Emits progress
    /// notifications while the blocking sync runs, if the client supplied a
    /// progress token.
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
        // Progress bridge: the blocking sync (on a spawn_blocking thread) pushes
        // status lines into an unbounded channel; a concurrent async task drains
        // them into MCP progress notifications — but only when the client asked
        // for progress (a token in the request `_meta`).
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

        let runtime = self.runtime.clone();
        let workspace = self.workspace.clone();
        let agent = self.agent.clone();
        let recover = params.recover.unwrap_or(false);
        let joined = tokio::task::spawn_blocking(move || {
            let forward = move |message: &str| {
                // Send failure only means the drain task is gone — nothing to do.
                let _ = tx.send(message.to_string());
            };
            let (outcome, selected) = sync::run_one_agent(
                &runtime,
                workspace.as_deref(),
                agent.as_deref(),
                recover,
                /* force_first_sync: */ false,
                /* human: */ false,
                Progress::callback(&forward),
            )?;
            Ok::<_, anyhow::Error>(sync::build_sync_result(outcome, &selected))
        })
        .await;
        // The blocking closure dropped `tx` on return, so the drain sees the
        // channel close and finishes after flushing the last messages.
        let _ = drain.await;

        match joined {
            Ok(Ok(result)) => structured_ok(&result),
            Ok(Err(e)) => Ok(tool_error(&e)),
            Err(join) => Err(worker_failed(join)),
        }
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
