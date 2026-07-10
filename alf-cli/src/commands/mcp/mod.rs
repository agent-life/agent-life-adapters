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
//! The v1 tool surface is 13 tools: `alf_status`, `alf_check`, `alf_sync`,
//! `alf_restore`, `alf_export_dry_run`, `alf_track`, `alf_configure`,
//! `alf_vault_add`, `alf_vault_list`, `alf_vault_delete`, `alf_agents_list`,
//! `alf_docs` (M2b) plus `alf_watch_set` (WP-M3). WP-M3 also ships the watch
//! loop itself ([`watch`]): a background, token-free auto-sync loop steered by
//! `alf_watch_set` and observed via the `alf_status` watch stanza.
//! `alf_agents_set` is deferred to v1.2 (design §16.1).

mod configure;
mod docs;
pub(crate) mod watch;

use std::path::{Path, PathBuf};
use std::time::Duration;

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
- `alf_restore` restores from the cloud (head; `at_sequence: N` is a read-only \
  point-in-time preview materialized into a separate preview directory — the \
  live workspace and sync state are never touched; `dry_run` lists what would \
  be written).
- `alf_configure` (generic runtime) sets the `.alf-map.json` that describes \
  where your memories live; `alf_track` opts an extra file into sync.
- `alf_vault_add`/`alf_vault_list`/`alf_vault_delete` manage the zero-knowledge \
  credentials vault (a key is generated on first add; back up the key file — \
  its fingerprint is returned, never its bytes).
- `alf_agents_list` shows the tracked agents; `alf_docs` returns deeper docs \
  for any topic (sync, restore, recovery, vault, map-file, …).
- `alf_watch_set` steers the background auto-sync loop (cadence + pause).

Once you are configured, a watch loop syncs your changes automatically at zero \
token cost — you rarely need to call `alf_sync` yourself; just check \
`alf_status` if you want to confirm. Syncing is safe and idempotent — run \
`alf_sync` after notable changes if you want an immediate sync. \
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
    /// Shared handle to the watch loop (WP-M3). `None` before the loop is wired
    /// (unit contexts); the tools degrade to the inactive stub when absent.
    watch: Option<std::sync::Arc<watch::WatchHandle>>,
    /// Serializes the per-agent read-modify-write tools (`alf_vault_add`,
    /// `alf_vault_delete`, `alf_configure`, `alf_track`, `alf_check`) in-process.
    /// rmcp fans a request batch out across threads (WP-M3 review E1), so without
    /// this two concurrent write-tool calls race their RMW — e.g. two first
    /// `alf_vault_add`s generating different keys, or two `alf_configure`s
    /// corrupting the map. Lock hierarchy L1 (manual §6); never nested with
    /// `sync_lock`.
    write_lock: std::sync::Arc<std::sync::Mutex<()>>,
    /// Serializes whole-workspace operations in-process (manual §6, L2):
    /// `alf_sync`, head `alf_restore`, and the watch loop's `run_due` all take
    /// it, so a manual sync can never interleave with an in-flight watch sync.
    /// tokio (not std) because the guard is legally held across `.await`s; the
    /// watch loop only ever `try_lock`s so it never blocks the runtime.
    sync_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
    #[expect(dead_code, reason = "the tool_handler macro reads this router field")]
    tool_router: ToolRouter<Self>,
}

impl AlfServer {
    fn new(
        runtime: String,
        workspace: Option<PathBuf>,
        agent: Option<String>,
        watch: Option<std::sync::Arc<watch::WatchHandle>>,
        sync_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
    ) -> Self {
        Self {
            runtime,
            workspace,
            agent,
            watch,
            write_lock: std::sync::Arc::new(std::sync::Mutex::new(())),
            sync_lock,
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
    /// Preview the workspace as it was after this sequence. A true read-only
    /// preview: nothing in the live workspace is written — files are
    /// materialized into a separate preview directory (returned as
    /// `preview_path` in the result) and `~/.alf/state` is untouched, so a
    /// later `alf_sync` is unaffected. Omit for a head restore.
    #[serde(default)]
    at_sequence: Option<u64>,
    /// List what a restore would write and touch nothing. Defaults to false.
    #[serde(default)]
    dry_run: Option<bool>,
    /// Memory restore mode for runtimes with a mutable per-agent store: `total`
    /// (exact, default) or `merge` (keep local-only rows). Ignored by
    /// file/markdown runtimes. `extend` renders the two choices as an inline JSON
    /// `enum` a limited LLM can read; the handler still validates (belt + braces).
    #[serde(default)]
    #[schemars(extend("enum" = ["total", "merge"]))]
    mode: Option<String>,
}

/// Parameters for `alf_track`.
#[derive(Deserialize, JsonSchema)]
struct TrackParams {
    /// Path to track. The file will sync as raw bytes. Must be an EXISTING
    /// regular file. Workspace-relative or absolute, but (unless `external:true`)
    /// it must resolve INSIDE the workspace or it is rejected. alf's own managed
    /// files (`.alf-include.json`, the sync log) cannot be tracked.
    path: String,
    /// Track a file OUTSIDE the workspace. Only available on the hermes and
    /// generic runtimes, and only under a pre-blessed root (passing the
    /// non-overridable denylist); setting true is your consent (the CLI's
    /// `--yes-external`). Blessing a new root stays a CLI/human ceremony.
    /// Defaults to false.
    #[serde(default)]
    external: Option<bool>,
}

/// Parameters for `alf_configure` (generic runtime only): a discriminated write —
/// `operation` picks replace vs merge, `body` is the map object. Both required,
/// so there is no "which of two blobs did I set" ambiguity.
#[derive(Deserialize, JsonSchema)]
struct ConfigureParams {
    /// "replace" writes `body` as the whole `.alf-map.json`; "merge" deep-merges
    /// `body` into the existing map.
    #[schemars(extend("enum" = ["replace", "merge"]))]
    operation: String,
    /// The `.alf-map.json` object — full for "replace", partial for "merge".
    /// Call alf_docs with topic="map-file" for the exact shape.
    body: serde_json::Value,
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
    /// Extra encrypted fields. Each entry MUST be a single `key=value` string
    /// (e.g. `region=eu-west-1`); an entry without `=` is rejected. Pass a list
    /// of such strings — not an object.
    #[serde(default)]
    #[schemars(inner(pattern(r"^[^=]+=.*$")))]
    fields: Vec<String>,
    /// Replace an existing record with the same label instead of duplicating it.
    #[serde(default)]
    update: Option<bool>,
}

/// Parameters for `alf_vault_delete`: a discriminated selector — `by` names the
/// descriptor to match on, `value` is what to match. Both required, so there is
/// no way to pass zero or several selectors.
#[derive(Deserialize, JsonSchema)]
struct VaultDeleteParams {
    /// Which plaintext descriptor to match the record on.
    #[schemars(extend("enum" = ["id", "label", "service"]))]
    by: String,
    /// The value to match. For by="id" this is the record UUID (e.g.
    /// 123e4567-e89b-12d3-a456-426614174000); for "label"/"service" it is the
    /// plaintext label or service name shown by alf_vault_list.
    value: String,
}

/// Parameters for `alf_watch_set`. Every field is optional — only the ones you
/// pass change. Intervals are `<n><unit>` strings (unit is one of s|m|h|d, e.g.
/// `90s`, `15m`, `1h30m`); a bare number with no unit is REJECTED.
#[derive(Deserialize, JsonSchema, Default)]
struct WatchSetParams {
    /// Delta-channel cadence for memory/raw sources. Format `<n><unit>`
    /// (unit s|m|h|d, e.g. `90s`, `15m`, `1h30m`) — a bare integer is rejected.
    /// Clamped to 60s (1 min) – 86400s (24 h); a sub-floor value is silently
    /// raised to the floor and reported in the result notes.
    #[serde(default)]
    #[schemars(pattern(r"^(\d+[smhd])+$"))]
    default_interval: Option<String>,
    /// Per-source cadence overrides. Values use the same `<n><unit>` format as
    /// `default_interval`. Keys must be REAL source ids — do not invent them;
    /// call `alf_status` and read its watch stanza to get the valid ids.
    /// Unknown ids are rejected with the list of valid ids; the tracked-files
    /// channel is set via `tracked_files_interval`, not here.
    #[serde(default)]
    per_source: Option<std::collections::HashMap<String, String>>,
    /// Cadence for the §6.1 tracked-file rollover channel (a full snapshot is
    /// expensive). Same `<n><unit>` format; clamped to 900s (15 min) – 86400s
    /// (24 h); a sub-floor value is silently raised.
    #[serde(default)]
    #[schemars(pattern(r"^(\d+[smhd])+$"))]
    tracked_files_interval: Option<String>,
    /// Pause (`true`) or resume (`false`) auto-sync. Resuming also clears a park.
    #[serde(default)]
    pause: Option<bool>,
}

/// The `alf_watch_set` result: the effective cadence config after the change.
#[derive(Debug, Serialize, JsonSchema)]
struct WatchSetResult {
    ok: bool,
    /// Whether auto-sync is running (loop active and not paused).
    active: bool,
    paused: bool,
    default_interval_secs: u64,
    tracked_files_interval_secs: u64,
    per_source_secs: std::collections::BTreeMap<String, u64>,
    /// Human-readable notes (e.g. an interval that was clamped to a floor).
    notes: Vec<String>,
}

/// Parameters for `alf_docs`.
#[derive(Deserialize, JsonSchema)]
struct DocsParams {
    /// The topic to look up. Canonical topics: sync, restore, recovery, vault,
    /// rotate-key, force-first-sync, purge, agents, check, export, add, import,
    /// validate, map-file, mcp. Common aliases are also accepted; an unknown
    /// topic returns the full list, so a wrong guess self-corrects on retry.
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
sequence, per-agent service reachability, and the live watch-loop stanza (active flag, per-source \
cadence + dirty state, any backoff/parked recovery state).",
        output_schema = rmcp::handler::server::tool::schema_for_output::<StatusResult>()
            .expect("StatusResult is a valid output schema")
    )]
    async fn alf_status(
        &self,
        Parameters(_): Parameters<NoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        // Snapshot the live watch loop (cheap, non-blocking) before deferring the
        // service query to the blocking pool.
        let watch = match &self.watch {
            Some(h) if h.is_active() => watch::to_status(h.snapshot()),
            Some(h) => WatchStatus {
                inactive_reason: h.inactive_reason(),
                ..WatchStatus::default()
            },
            None => WatchStatus::default(),
        };
        call_blocking(move || {
            Ok(StatusResult {
                status: help::status_json()?,
                watch,
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
        // Locked: `check::gather` persists discovered `[[agents]]` rows to
        // config.toml — an RMW that must serialize with the other config writers.
        call_blocking_locked(self.write_lock.clone(), move || {
            check::gather(&runtime, workspace.as_deref(), agent.as_deref())
        })
        .await
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
        let watch = self.watch.clone();
        // L2: serialize against other manual syncs/restores and the watch loop's
        // in-flight sync (manual §6). Bounded wait → `agent_busy`, never a hang.
        let Ok(permit) = tokio::time::timeout(
            Duration::from_secs(120),
            self.sync_lock.clone().lock_owned(),
        )
        .await
        else {
            return Ok(tool_error(&agent_busy(
                "another sync or restore is still running in this server",
            )));
        };
        call_streaming(ctx, move |sink| {
            let _permit = permit; // L2 held for the whole blocking seam
            let _agent_lock = acquire_agent_lock(&runtime, workspace.as_deref(), agent.as_deref())?;
            let (outcome, selected) = sync::run_one_agent(
                &runtime,
                workspace.as_deref(),
                agent.as_deref(),
                recover,
                /* force_first_sync: */ false,
                /* human: */ false,
                Progress::callback(sink),
            )?;
            // A clean manual sync un-parks the watch loop (design §7.W4).
            if let Some(h) = &watch {
                h.note_manual_sync_ok();
            }
            Ok(sync::build_sync_result(outcome, &selected))
        })
        .await
    }

    /// Restore from the cloud: head, a read-only point-in-time preview
    /// (`at_sequence`), or a dry-run listing. Emits progress if a token is given.
    #[tool(
        name = "alf_restore",
        description = "Restore this agent from the cloud. Default restores the head of history and \
updates local sync state. Pass at_sequence:N for a read-only point-in-time preview: files are \
written to a preview directory (preview_path in the result), never into the live workspace, and \
sync state is not moved. Pass dry_run:true to list what would be written without touching \
anything. mode is total (default) or merge for runtimes with a mutable per-agent store.",
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
        let head = !dry_run && at_sequence.is_none();
        // L2 (manual §6): only a HEAD restore rewrites the live workspace + sync
        // state, so only it serializes against syncs. Previews import into the
        // preview dir and dry-runs write nothing — both stay lock-free.
        let permit = if head {
            match tokio::time::timeout(
                Duration::from_secs(120),
                self.sync_lock.clone().lock_owned(),
            )
            .await
            {
                Ok(g) => Some(g),
                Err(_) => {
                    return Ok(tool_error(&agent_busy(
                        "another sync or restore is still running in this server",
                    )));
                }
            }
        } else {
            None
        };
        // Pause the watch loop for the duration of a HEAD restore: it must never
        // sync a workspace mid-restore (design §6/§11 — the pause hook). Previews
        // never touch the workspace, so the loop has nothing to see and no guard
        // is taken. The guard resumes the loop on drop.
        let _restore_guard = head
            .then(|| self.watch.as_ref().map(watch::restore_guard))
            .flatten();
        call_streaming(ctx, move |sink| {
            let _permit = permit; // L2 held for the whole blocking seam
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
already tracked). Tracked files sync as RAW BYTES (no memory-record parsing), and any change to \
one triggers a FULL-SNAPSHOT rollover on the tracked-files cadence (15 min floor, alf_watch_set \
tracked_files_interval) — track sparingly; map memory sources instead where possible. \
Workspace-relative by default. With external:true a file outside the workspace can be tracked, \
but only under a pre-blessed root and never on the sensitive denylist — blessing a new root \
stays a CLI/human ceremony.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<add::AddResult>()
            .expect("AddResult is a valid output schema")
    )]
    async fn alf_track(
        &self,
        Parameters(params): Parameters<TrackParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (runtime, workspace, agent) = self.owned();
        let external = params.external.unwrap_or(false);
        let watch = self.watch.clone();
        call_blocking_locked(self.write_lock.clone(), move || {
            let result = add::track(
                &runtime,
                workspace.as_deref(),
                agent.as_deref(),
                &params.path,
                external,
            )?;
            // The include list defines part of the watch surface — the loop
            // re-derives it on the next tick, no restart needed (manual §4.3).
            if let Some(h) = &watch {
                h.request_resurface();
            }
            Ok(result)
        })
        .await
    }

    /// Configure the generic runtime's `.alf-map.json` (validated read-modify-write).
    #[tool(
        name = "alf_configure",
        description = "Generic runtime only (call alf_status to confirm your runtime): set the \
.alf-map.json that maps workspace files to memory records (and how they are chunked/tagged/dated). \
Set operation to \"replace\" (write body as the whole map) or \"merge\" (deep-merge body into the \
existing map); body is the map object — call alf_docs topic=\"map-file\" for its shape. Validated \
before writing: an invalid configuration is rejected with nothing written.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<configure::ConfigureResult>()
            .expect("ConfigureResult is a valid output schema")
    )]
    async fn alf_configure(
        &self,
        Parameters(params): Parameters<ConfigureParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (runtime, workspace, _agent) = self.owned();
        let watch = self.watch.clone();
        call_blocking_locked(self.write_lock.clone(), move || {
            let (map, patch) = match params.operation.as_str() {
                "replace" => (Some(params.body), None),
                "merge" => (None, Some(params.body)),
                other => {
                    anyhow::bail!("operation must be \"replace\" or \"merge\" (got {other:?})")
                }
            };
            let result = configure::configure(&runtime, workspace.as_deref(), map, patch)?;
            // The map defines the watch surface — re-derive it (manual §4.3).
            if let Some(h) = &watch {
                h.request_resurface();
            }
            Ok(result)
        })
        .await
    }

    /// Add a credential to the zero-knowledge vault (auto-keygen on first use).
    #[tool(
        name = "alf_vault_add",
        description = "Encrypt a credential and append it to the agent's vault (Layer 4). The \
ciphertext syncs; the plaintext descriptors (service, label, tags) stay visible. On the first add \
with no key resolvable, a vault key is generated (0600) and its fingerprint + path returned — \
never the key bytes; back up that file. An add whose service+label match an existing record is \
rejected unless update:true.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<VaultAddResult>()
            .expect("VaultAddResult is a valid output schema")
    )]
    async fn alf_vault_add(
        &self,
        Parameters(params): Parameters<VaultAddParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (runtime, workspace, agent) = self.owned();
        call_blocking_locked(self.write_lock.clone(), move || {
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
plaintext descriptors so no key is needed: set by to \"id\", \"label\", or \"service\" and value \
to what to match (use alf_vault_list to find it). Recoverable via a point-in-time restore of an \
earlier sequence.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<vault::DeleteResult>()
            .expect("vault DeleteResult is a valid output schema")
    )]
    async fn alf_vault_delete(
        &self,
        Parameters(params): Parameters<VaultDeleteParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (runtime, workspace, agent) = self.owned();
        call_blocking_locked(self.write_lock.clone(), move || {
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

    /// Steer the watch loop: cadence knobs + pause/resume (design §6/§11.3).
    #[tool(
        name = "alf_watch_set",
        description = "Control the auto-sync watch loop. Set the delta cadence (default_interval, \
1 min–24 h), the tracked-file rollover cadence (tracked_files_interval, 15 min–24 h), per-source \
overrides, and/or pause:true|false. Returns the effective cadence; intervals below a floor are \
clamped (noted). If the loop is not running the tool errors and the message says why (e.g. no \
API key, unresolved agent); a paused or parked loop is still steerable — pause:false clears a \
park.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<WatchSetResult>()
            .expect("WatchSetResult is a valid output schema")
    )]
    async fn alf_watch_set(
        &self,
        Parameters(params): Parameters<WatchSetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(handle) = self.watch.clone() else {
            return Ok(tool_error(&anyhow::anyhow!(
                "the watch loop is not running (no API key or the agent could not be resolved); \
                 configure and sync first, then re-launch the server"
            )));
        };
        // The documented contract (manual §3.13): the tool errors when the loop
        // is not running, and says WHY (paused/parked loops stay steerable —
        // pause:false is what clears a park, so those still pass this gate).
        if !handle.is_active() {
            return Ok(tool_error(&anyhow::anyhow!(
                "the watch loop is not running: {}",
                handle
                    .inactive_reason()
                    .unwrap_or_else(|| "unknown startup failure".into())
            )));
        }
        // Serialized like the other config/FS writers so a concurrent
        // `alf_watch_set` can't lose an update mid read-modify-write (review G4).
        call_blocking_locked(self.write_lock.clone(), move || {
            watch_set_impl(&handle, params)
        })
        .await
    }
}

/// Apply an `alf_watch_set` change to the running loop and return the effective
/// cadence. Parses + clamps each interval; a below-floor value is clamped with a
/// note (this is the production validation that the design's test-only floor
/// override is deliberately rejected by — the real floors always apply here).
fn watch_set_impl(
    handle: &std::sync::Arc<watch::WatchHandle>,
    params: WatchSetParams,
) -> anyhow::Result<WatchSetResult> {
    use watch::engine::{clamp_delta, clamp_tracked};

    let mut cfg = handle.config();
    let mut notes = Vec::new();

    if let Some(raw) = &params.default_interval {
        let d = watch::parse_interval(raw)
            .ok_or_else(|| anyhow::anyhow!("invalid default_interval '{raw}' (e.g. 15m, 1h)"))?;
        if clamp_delta(d) != d {
            notes.push(format!(
                "default_interval '{raw}' clamped to the 1 min–24 h range"
            ));
        }
        cfg.set_default(d);
    }
    if let Some(raw) = &params.tracked_files_interval {
        let d = watch::parse_interval(raw).ok_or_else(|| {
            anyhow::anyhow!("invalid tracked_files_interval '{raw}' (e.g. 30m, 1h)")
        })?;
        if clamp_tracked(d) != d {
            notes.push(format!(
                "tracked_files_interval '{raw}' clamped to the 15 min–24 h range"
            ));
        }
        cfg.set_tracked(d);
    }
    if let Some(map) = &params.per_source {
        // Validate EVERY id before mutating anything: unknown ids are typos a
        // weak model would otherwise never notice (the old behavior echoed
        // them back as ok:true), and the tracked channel has its own knob.
        let snap = handle.snapshot();
        for id in map.keys() {
            match snap.sources.iter().find(|s| s.source == *id) {
                None => {
                    let mut valid: Vec<&str> =
                        snap.sources.iter().map(|s| s.source.as_str()).collect();
                    valid.sort_unstable();
                    anyhow::bail!(
                        "unknown per_source id '{id}' — valid ids: {}",
                        if valid.is_empty() {
                            "(none — the loop has no sources yet)".to_string()
                        } else {
                            valid.join(", ")
                        }
                    );
                }
                Some(s) if s.tracked => anyhow::bail!(
                    "'{id}' is the tracked-files channel — set tracked_files_interval \
                     instead of per_source"
                ),
                Some(_) => {}
            }
        }
        for (id, raw) in map {
            let d = watch::parse_interval(raw).ok_or_else(|| {
                anyhow::anyhow!("invalid per_source['{id}'] interval '{raw}' (e.g. 5m)")
            })?;
            if clamp_delta(d) != d {
                notes.push(format!(
                    "per_source['{id}'] '{raw}' clamped to the 1 min–24 h range"
                ));
            }
            cfg.set_per_source(id.clone(), d);
        }
    }
    if let Some(pause) = params.pause {
        cfg.paused = pause;
    }

    let effective = handle.set_config(cfg);
    // Derive `active` from the engine snapshot (review C2): the snapshot's `active`
    // already accounts for pause AND park, so a parked loop reports active:false
    // instead of the earlier pause-only recomputation.
    let snap = handle.snapshot();
    Ok(WatchSetResult {
        ok: true,
        active: handle.is_active() && snap.active,
        paused: effective.paused,
        default_interval_secs: effective.default_interval().as_secs(),
        tracked_files_interval_secs: effective.tracked_files_interval().as_secs(),
        per_source_secs: effective
            .per_source()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_secs()))
            .collect(),
        notes,
    })
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

    // Generate a fresh key at the default path (0600, private), written with
    // O_EXCL so a concurrent first-add cannot clobber it (review E1): losing the
    // race means a credential would be sealed under a discarded key —
    // permanently undecryptable. The loser re-reads the winner's key instead.
    let key = VaultKey::generate();
    let fingerprint = key.fingerprint();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    match crate::fs_private::write_private_new(&path, &key.to_base64()) {
        Ok(()) => Ok((
            VaultKeyArgs {
                key_file: Some(path.clone()),
                key_env: None,
            },
            Some(KeyGenInfo {
                fingerprint,
                path: path.display().to_string(),
            }),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // A concurrent add generated the key first — use it, don't overwrite.
            Ok((
                VaultKeyArgs {
                    key_file: Some(path),
                    key_env: None,
                },
                None,
            ))
        }
        Err(e) => {
            Err(anyhow::Error::from(e).context(format!("writing vault key {}", path.display())))
        }
    }
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
    // L3: two MCP servers pinned to the same agent must not interleave the
    // vault read-modify-write (manual §6). write_lock (L1) is already held.
    let _agent_lock = scope.map(vault_agent_lock).transpose()?;

    // Duplicate guard (manual §3.8): without update:true, an add whose service +
    // effective label match an existing record is rejected — a repeated
    // identical call must never silently duplicate.
    if !params.update.unwrap_or(false) {
        if let Some(label) = params.label.as_deref().or(params.username.as_deref()) {
            // A missing vault (first add) has no records — a list error there
            // just means "nothing to collide with".
            if let Ok(existing) = vault_list_impl(runtime, workspace, agent) {
                if let Some(dup) = existing
                    .credentials()
                    .iter()
                    .find(|c| c.label() == Some(label) && c.service() == params.service)
                {
                    anyhow::bail!(
                        "a credential with label '{label}' for service '{}' already exists \
                         (id {}); pass update:true to replace it, or use a different label",
                        params.service,
                        dup.id()
                    );
                }
            }
        }
    }

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
    // L3: cross-process vault RMW protection, same as vault_add_impl.
    let _agent_lock = scope.map(vault_agent_lock).transpose()?;
    let selector = match params.by.as_str() {
        "id" => vault::Selector {
            id: Some(params.value),
            label: None,
            service: None,
        },
        "label" => vault::Selector {
            id: None,
            label: Some(params.value),
            service: None,
        },
        "service" => vault::Selector {
            id: None,
            label: None,
            service: Some(params.value),
        },
        other => anyhow::bail!("by must be one of id, label, or service (got {other:?})"),
    };
    vault::delete_core(None, &selector, None, scope)
}

// ---------------------------------------------------------------------------
// Locking helpers (manual §6)
// ---------------------------------------------------------------------------

/// The coded `agent_busy` error: another sync/restore (this process or another)
/// holds the lock past the bounded wait.
pub(crate) fn agent_busy(cause: &str) -> anyhow::Error {
    crate::errors::CliError {
        code: crate::errors::codes::AGENT_BUSY,
        cause: cause.to_string(),
        remedy: "retry shortly; alf_status shows what auto-sync is doing".to_string(),
    }
    .into()
}

/// L3 for the vault tools: the per-agent advisory lock by known agent id.
fn vault_agent_lock(agent_id: Uuid) -> anyhow::Result<watch::lock::AgentLock> {
    let lock_file = watch::lock_path(agent_id)?;
    if let Some(dir) = lock_file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    watch::lock::acquire_timeout(
        &lock_file,
        Duration::from_secs(10),
        Duration::from_millis(250),
    )?
    .ok_or_else(|| agent_busy("another ALF process is syncing or restoring this agent"))
}

/// L3: acquire the per-agent cross-process advisory lock with a bounded wait
/// (10 s), resolving the pinned agent first. `agent_busy` if another ALF
/// process holds it past the wait. Callers hold the returned guard across the
/// whole-workspace operation.
fn acquire_agent_lock(
    runtime: &str,
    workspace: Option<&Path>,
    agent: Option<&str>,
) -> anyhow::Result<watch::lock::AgentLock> {
    let (_ws, agent_id) = watch::resolve_loop_context(runtime, workspace, agent)?;
    let lock_file = watch::lock_path(agent_id)?;
    if let Some(dir) = lock_file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    watch::lock::acquire_timeout(
        &lock_file,
        Duration::from_secs(10),
        Duration::from_millis(250),
    )?
    .ok_or_else(|| agent_busy("another ALF process is syncing or restoring this agent"))
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

/// Like [`call_blocking`], but holds `write_lock` for the whole seam so the
/// per-agent write-tools serialize against each other in-process (review E1).
/// The guard is taken **inside** the blocking closure (a `std::sync::Mutex` must
/// not be held across an `.await`).
async fn call_blocking_locked<T>(
    write_lock: std::sync::Arc<std::sync::Mutex<()>>,
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<CallToolResult, ErrorData>
where
    T: Serialize + Send + 'static,
{
    call_blocking(move || {
        // Poison-tolerant: a panicked prior holder left no invariant broken (the
        // guard protects only the FS RMW), so recover the guard and proceed.
        let _guard = write_lock.lock().unwrap_or_else(|e| e.into_inner());
        work()
    })
    .await
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

/// CLI remedies name commands and flags an MCP agent cannot run; rewrite the
/// known CLI phrasings into tool guidance before they reach the wire
/// (manual §5). Ordered longest-match-first so `alf sync --recover` wins over
/// `alf sync`. CLI stdout is untouched — this rewrite exists only on the MCP
/// path.
const MCP_REMEDY_REWRITES: &[(&str, &str)] = &[
    (
        "alf sync --force-first-sync",
        "the CLI ceremony `alf sync --force-first-sync` (human terminal — see alf_docs          topic \"force-first-sync\")",
    ),
    ("alf sync --recover", "alf_sync with recover:true"),
    (
        "alf vault rotate-key",
        "the CLI ceremony `alf vault rotate-key` (human terminal — see alf_docs topic          \"rotate-key\")",
    ),
    (
        "alf vault migrate",
        "the CLI ceremony `alf vault migrate` (human terminal — see alf_docs topic          \"vault\")",
    ),
    (
        "alf agents enable",
        "the CLI ceremony `alf agents enable` (human terminal — enable/disable are not tools)",
    ),
    ("alf login", "the CLI ceremony `alf login` (human terminal)"),
    ("alf check", "the alf_check tool (CLI: `alf check`"),
    ("alf restore", "the alf_restore tool (CLI: `alf restore`"),
    ("alf sync", "the alf_sync tool (CLI: `alf sync`"),
];

/// Apply [`MCP_REMEDY_REWRITES`] to a remedy string.
fn mcp_hint(remedy: &str) -> String {
    let mut out = remedy.to_string();
    for (cli, tool) in MCP_REMEDY_REWRITES {
        out = out.replace(cli, tool);
    }
    out
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
                obj.insert(
                    "hint".into(),
                    serde_json::Value::String(mcp_hint(&cli.remedy)),
                );
            }
        }
        None => {
            obj.insert(
                "error".into(),
                serde_json::Value::String(format!("{err:#}")),
            );
            let hint = crate::output::error_hint(err);
            if !hint.is_empty() {
                obj.insert("hint".into(), serde_json::Value::String(mcp_hint(&hint)));
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
    let runtime_s = runtime.to_string();
    let workspace_buf = workspace.map(Path::to_path_buf);
    let agent_s = agent.map(str::to_string);

    // The watch loop runs only when an API key is configured — otherwise every
    // catch-up sync would fail and immediately park. An unconfigured server still
    // answers tools; `alf_status` reports the loop inactive (M2a stub semantics).
    let api_key_set = Config::load()
        .map(|c| !c.service.api_key.trim().is_empty())
        .unwrap_or(false);

    let mut cfg = watch::build_config(runtime, workspace);
    // A per-process constant backoff jitter (from the PID) de-synchronizes retry
    // storms across ALF processes on one machine — no rand dependency needed.
    cfg.backoff_jitter = (std::process::id() % 100) as f64 / 100.0 * 0.3;
    let handle = std::sync::Arc::new(watch::WatchHandle::new(cfg));
    if !api_key_set {
        handle.set_inactive_reason(
            "no API key configured — the watch loop only runs when service.api_key is set \
             (run `alf login` in a terminal, then restart the MCP server)",
        );
    }
    // L2 (manual §6): shared by the manual sync/restore tools and the watch loop.
    let sync_lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));

    let server = AlfServer::new(
        runtime_s.clone(),
        workspace_buf.clone(),
        agent_s.clone(),
        Some(handle.clone()),
        sync_lock.clone(),
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime for the MCP server")?;

    rt.block_on(async move {
        eprintln!("alf mcp serve: stdio server ready (runtime={runtime_s})");
        // The watch loop (design §11) runs concurrently with the protocol handler
        // for the lifetime of the session; the host owns that lifetime (§5). It is
        // spawned only when an API key is configured — otherwise every catch-up
        // sync would fail and park, so the loop stays down and `alf_status` reports
        // it inactive.
        let loop_task = api_key_set.then(|| {
            tokio::spawn(watch::run_loop(
                handle,
                runtime_s,
                workspace_buf,
                agent_s,
                sync_lock,
            ))
        });
        let running = server
            .serve(rmcp::transport::io::stdio())
            .await
            .context("failed to start the stdio MCP server")?;
        let reason = running
            .waiting()
            .await
            .context("the MCP server terminated abnormally")?;
        if let Some(task) = loop_task {
            task.abort();
        }
        eprintln!("alf mcp serve: stopped ({reason:?})");
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle_with_sources() -> std::sync::Arc<watch::WatchHandle> {
        let handle = std::sync::Arc::new(watch::WatchHandle::new(Default::default()));
        handle.set_sources_for_test(&[
            alf_core::WatchSpec::file("journal", "/ws/memories"),
            alf_core::WatchSpec::file("tracked-files", "/ws/x").as_tracked(),
        ]);
        handle
    }

    #[test]
    fn mcp_hint_rewrites_recover_before_plain_sync() {
        let hinted = mcp_hint("Run 'alf sync --recover' to re-derive against cloud truth");
        assert!(hinted.contains("alf_sync with recover:true"), "{hinted}");
        assert!(!hinted.contains("--recover'"), "{hinted}");
    }

    #[test]
    fn mcp_hint_labels_cli_ceremonies() {
        let hinted = mcp_hint("Run 'alf vault rotate-key' to rotate");
        assert!(hinted.contains("human terminal"), "{hinted}");
        assert!(hinted.contains("alf_docs"), "{hinted}");
    }

    #[test]
    fn tool_error_carries_rewritten_hint() {
        let err: anyhow::Error = crate::errors::CliError {
            code: crate::errors::codes::SYNC_UPLOAD_FAILED,
            cause: "x".into(),
            remedy: "run alf sync --recover".into(),
        }
        .into();
        let result = tool_error(&err);
        let sc = serde_json::to_value(result.structured_content.as_ref().unwrap()).unwrap();
        assert_eq!(sc["code"], "sync_upload_failed");
        assert!(
            sc["hint"].as_str().unwrap().contains("recover:true"),
            "hint must speak tool language: {sc}"
        );
    }

    #[test]
    fn watch_set_rejects_unknown_per_source_id() {
        let handle = handle_with_sources();
        let params = WatchSetParams {
            per_source: Some(
                [("bogus".to_string(), "5m".to_string())]
                    .into_iter()
                    .collect(),
            ),
            ..Default::default()
        };
        let err = watch_set_impl(&handle, params).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("unknown per_source id 'bogus'"), "{msg}");
        assert!(msg.contains("journal"), "must list the valid ids: {msg}");
    }

    #[test]
    fn watch_set_rejects_tracked_channel_id_pointing_to_tracked_files_interval() {
        let handle = handle_with_sources();
        let params = WatchSetParams {
            per_source: Some(
                [("tracked-files".to_string(), "30m".to_string())]
                    .into_iter()
                    .collect(),
            ),
            ..Default::default()
        };
        let err = watch_set_impl(&handle, params).unwrap_err();
        assert!(
            format!("{err:#}").contains("tracked_files_interval"),
            "must point at the right knob: {err:#}"
        );
    }

    #[test]
    fn watch_set_applies_a_valid_per_source_override() {
        let handle = handle_with_sources();
        let params = WatchSetParams {
            per_source: Some(
                [("journal".to_string(), "5m".to_string())]
                    .into_iter()
                    .collect(),
            ),
            ..Default::default()
        };
        let result = watch_set_impl(&handle, params).unwrap();
        assert!(result.ok);
        assert_eq!(result.per_source_secs.get("journal"), Some(&300));
    }

    #[test]
    fn watch_set_clamps_sub_floor_intervals_with_notes() {
        let handle = handle_with_sources();
        // 30s < 1-min delta floor; 5m < 15-min tracked floor.
        let params = WatchSetParams {
            default_interval: Some("30s".into()),
            tracked_files_interval: Some("5m".into()),
            pause: Some(true),
            ..Default::default()
        };
        let result = watch_set_impl(&handle, params).unwrap();
        assert_eq!(result.default_interval_secs, 60, "delta clamped to floor");
        assert_eq!(
            result.tracked_files_interval_secs, 900,
            "tracked clamped to floor"
        );
        assert!(result.paused);
        assert!(
            result.notes.len() >= 2,
            "both clamps must be noted: {:?}",
            result.notes
        );

        // A value above the floors passes through verbatim.
        let params = WatchSetParams {
            default_interval: Some("10m".into()),
            ..Default::default()
        };
        let result = watch_set_impl(&handle, params).unwrap();
        assert_eq!(result.default_interval_secs, 600);
    }

    #[test]
    fn watch_set_rejects_a_malformed_interval() {
        let handle = handle_with_sources();
        let params = WatchSetParams {
            default_interval: Some("soon".into()),
            ..Default::default()
        };
        assert!(watch_set_impl(&handle, params).is_err());
    }
}
