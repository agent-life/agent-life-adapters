//! Shared output DTOs used by both the CLI's JSON stdout and the MCP tool
//! layer's `structuredContent` + `outputSchema`.
//!
//! These structs derive **both** `serde::Serialize` (the byte-for-byte JSON the
//! CLI has always printed) and `schemars::JsonSchema` (so the MCP server can
//! declare each tool's `outputSchema` from the same type the CLI prints — the
//! design's "one JSON contract, transported" property). Adding the schema
//! derive does not change serialization, so the CLI stdout stays identical
//! (pinned by tests in `help.rs`).
//!
//! Only the genuinely *shared* status structs live here. Command-specific
//! result types (`CheckResult`, `SyncResult`) stay co-located with the code
//! that builds them, made `pub(crate)` and given the same `JsonSchema` derive
//! in place — moving them would separate the types from their builders for no
//! reachability gain.

use schemars::JsonSchema;
use serde::Serialize;

/// JSON-serializable status (paths as strings). Printed verbatim by
/// `alf help status`; embedded in [`StatusResult`] for the MCP `alf_status`
/// tool.
#[derive(Serialize, JsonSchema)]
pub struct StatusJson {
    pub config_path: String,
    pub config_exists: bool,
    pub api_key_set: bool,
    pub state_dir: String,
    pub state_dir_exists: bool,
    /// True if API key is set and at least one tracked agent is reachable on the service.
    pub service_reachable: bool,
    pub agents: Vec<AgentJson>,
    /// Per-agent service status (only present when API key set and we queried).
    pub agent_service_status: Vec<AgentServiceStatusJson>,
}

#[derive(Serialize, JsonSchema)]
pub struct AgentJson {
    pub agent_id: String,
    pub last_synced_sequence: u64,
    pub last_synced_at: Option<String>,
    pub snapshot_exists: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct AgentServiceStatusJson {
    pub agent_id: String,
    pub online: bool,
    pub name: Option<String>,
    pub server_latest_sequence: Option<u64>,
    pub error: Option<String>,
}

/// The MCP `alf_status` tool result: the CLI's [`StatusJson`] plus server-only
/// extensions. In WP-M2a the only extension is the `watch` stanza, stubbed
/// inactive until WP-M3 lands the watch loop.
#[derive(Serialize, JsonSchema)]
pub struct StatusResult {
    #[serde(flatten)]
    pub status: StatusJson,
    /// Watch-loop state. Stubbed (`active: false`, empty `sources`) until
    /// WP-M3 owns the loop; present now so the tool's schema is stable across
    /// the train.
    pub watch: WatchStatus,
}

/// Watch-loop status. WP-M2a ships the inactive stub; WP-M3 fills `sources`
/// with per-source `{ last_tick, dirty_count, backoff }` rows.
#[derive(Serialize, JsonSchema, Default)]
pub struct WatchStatus {
    /// Whether an auto-sync watch loop is running. Always `false` in v1 M2a —
    /// there is no loop yet (`alf mcp serve` syncs only on explicit `alf_sync`).
    pub active: bool,
    /// Per-source watch state. Always empty until WP-M3.
    pub sources: Vec<WatchSource>,
}

/// One watched source's loop state (WP-M3 shape; never emitted in M2a).
#[derive(Serialize, JsonSchema)]
pub struct WatchSource {
    pub source: String,
    pub last_tick: Option<String>,
    pub dirty_count: u64,
}

/// Schema shim for [`alf_core::FileEntry`], which lives in `alf-core` and so
/// cannot derive `schemars::JsonSchema` (that crate is deliberately alf-cli-only
/// — Lambda-ARM64 invariant). The MCP dry-run tools (`alf_export_dry_run`,
/// `alf_restore`) carry `Vec<FileEntry>` in their results; annotate that field
/// with `#[schemars(with = "Vec<crate::schema::FileEntrySchema>")]` so the tool's
/// `outputSchema` describes the same `{path, size}` shape `FileEntry` serializes
/// to. Only the schema derive is used — never constructed.
#[derive(JsonSchema)]
#[allow(dead_code)]
pub(crate) struct FileEntrySchema {
    pub path: String,
    pub size: u64,
}
