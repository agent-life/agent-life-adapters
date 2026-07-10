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

/// Watch-loop status (WP-M3). `active` is false when no loop is running (no API
/// key, unresolved agent, or paused/parked); `sources` carries per-source cadence
/// and dirty state; `parked`/`backoff_retry_in_secs` surface the recovery state
/// machine so an agent can see why auto-sync stopped.
#[derive(Serialize, JsonSchema, Default)]
pub struct WatchStatus {
    /// Whether the auto-sync watch loop is actively syncing (running, not paused,
    /// not parked).
    pub active: bool,
    /// Whether the loop is paused (`alf_watch_set {pause:true}` or mid-restore).
    #[serde(default)]
    pub paused: bool,
    /// Why the loop is NOT running (e.g. "no API key configured", "watch loop
    /// not started: unknown runtime"). Present only when the loop never
    /// started or bailed (manual §4.6).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inactive_reason: Option<String>,
    /// Present when auto-sync has parked on an unrecoverable error and is waiting
    /// for operator intervention (design §7.W4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parked: Option<WatchParked>,
    /// Seconds until the next retry while backing off after a transient error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backoff_retry_in_secs: Option<u64>,
    /// Per-source watch state.
    pub sources: Vec<WatchSource>,
}

/// A parked auto-sync error (coded, with a remediation hint).
#[derive(Serialize, JsonSchema)]
pub struct WatchParked {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// One watched source's loop state.
#[derive(Serialize, JsonSchema)]
pub struct WatchSource {
    pub source: String,
    /// The resolved sync cadence for this source (seconds).
    pub interval_secs: u64,
    /// True for the §6.1 tracked-file (full-snapshot rollover) channel.
    pub tracked: bool,
    /// Whether the source has unsynced changes pending.
    pub dirty: bool,
    /// Number of change events observed since the last sync.
    pub dirty_count: u64,
    /// How long ago (seconds) this source last synced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_fire_secs_ago: Option<u64>,
    /// Set when a file has churned continuously for 24 h and can never be safely
    /// captured (never sync torn bytes — surface it instead).
    #[serde(default)]
    pub never_quiesced_warning: bool,
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
