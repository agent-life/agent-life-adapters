//! `alf restore` — download and restore from the cloud.
//!
//! Flow (head restore, default):
//! 1. Load config (check API key)
//! 2. Parse agent ID
//! 3. Fetch and rebuild the cloud archive in memory (no local state change).
//! 4. Resolve adapter; write a durable, workspace-bound restore marker; import
//!    the merged archive into the live workspace.
//! 5. Persist `~/.alf/state/{id}-snapshot.alf`, then state.toml, then clear
//!    the marker. An interruption leaves sync parked until the original
//!    workspace's head restore is rerun.
//!
//! Flow (point-in-time preview, `--at-sequence N`):
//! 1-2. As above.
//! 3. [`fetch_point_in_time`] — fetch snapshot + deltas bounded by `N`, merge in memory.
//!    **Does not touch `~/.alf/state/`** — PIT restores are a read-only preview.
//! 4. Import into `~/.alf/preview/{agent}/seq-{N}/` (manual §3.4) — the live
//!    workspace is NEVER written, so the watch loop has nothing to pick up.
//!
//! `pull_cloud_base` is also reused by `alf sync --recover` (commands/sync.rs).

use crate::adapter;
use crate::api_client::{ApiClient, RestoreDelta};
use crate::config::Config;
use crate::errors::{codes, CliError};
use crate::output;
use crate::output::Progress;
use crate::selector;
use crate::state::{
    clear_restore_inflight, load_restore_inflight, local_base_path, restore_inflight_path,
    save_restore_inflight, state_file_path, AgentState, RestoreInflight, RestoreInflightPhase,
};
use crate::vault_key::{self, VaultKeyArgs};
use crate::vault_migrate;

use alf_core::archive::{AlfReader, DeltaReader};
#[cfg(test)]
use alf_core::rebuild::rebuild_snapshot;
use alf_core::rebuild::rebuild_snapshot_with_sequence;
use alf_core::{Adapter, ArchiveEnumeration, FileEntry, ImportOptions, ImportReport, RestoreMode};

use anyhow::Context;
use anyhow::Result;
use chrono::Utc;
use colored::Colorize;
use schemars::JsonSchema;
use serde::Serialize;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Serialize, JsonSchema)]
pub(crate) struct RestoreResult {
    ok: bool,
    agent_id: String,
    agent_name: String,
    sequence: u64,
    runtime: String,
    memory_records: u64,
    workspace: String,
    /// `true` when invoked with `--at-sequence N`: a true read-only preview —
    /// files land in `preview_path`, and neither the live workspace nor
    /// `~/.alf/state/` is touched (manual §3.4).
    preview: bool,
    /// Echoes the `--at-sequence N` flag; `None` for a head restore.
    #[serde(skip_serializing_if = "Option::is_none")]
    at_sequence: Option<u64>,
    /// Where the preview was materialized (`~/.alf/preview/{agent}/seq-{N}/`);
    /// only present for point-in-time previews. Pruned to the 3 newest per agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    preview_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct RestoreDryRunResult {
    ok: bool,
    dry_run: bool,
    agent_id: String,
    sequence: u64,
    #[schemars(with = "Vec<crate::schema::FileEntrySchema>")]
    would_write: Vec<FileEntry>,
    /// Echoes the `--at-sequence N` flag; `None` for a head preview.
    #[serde(skip_serializing_if = "Option::is_none")]
    at_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

/// The `alf_restore` MCP tool result — one flat object covering both a completed
/// restore and a dry-run preview (distinguished by `dry_run`). A single object
/// schema is required: MCP mandates `outputSchema` have a root `type: "object"`,
/// which a serde-untagged enum (`anyOf`, no root type) does not satisfy. The
/// real-restore fields are `None` on a dry-run and vice-versa.
#[derive(Serialize, JsonSchema)]
pub(crate) struct RestoreToolResult {
    ok: bool,
    /// True when this was a dry-run preview (nothing written).
    dry_run: bool,
    /// True when `at_sequence` was given (point-in-time; local state not moved).
    preview: bool,
    agent_id: String,
    sequence: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    at_sequence: Option<u64>,
    /// Real-restore only (absent on a dry-run).
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    memory_records: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<String>,
    /// Point-in-time previews only: the directory the preview was materialized
    /// into (the live workspace is never written; manual §3.4).
    #[serde(skip_serializing_if = "Option::is_none")]
    preview_path: Option<String>,
    /// Dry-run only (the files a restore would write).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<Vec<crate::schema::FileEntrySchema>>")]
    would_write: Option<Vec<FileEntry>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

impl From<RestoreResult> for RestoreToolResult {
    fn from(r: RestoreResult) -> Self {
        RestoreToolResult {
            ok: r.ok,
            dry_run: false,
            preview: r.preview,
            agent_id: r.agent_id,
            sequence: r.sequence,
            at_sequence: r.at_sequence,
            agent_name: Some(r.agent_name),
            runtime: Some(r.runtime),
            memory_records: Some(r.memory_records),
            workspace: Some(r.workspace),
            preview_path: r.preview_path,
            would_write: None,
            warnings: r.warnings,
        }
    }
}

impl From<RestoreDryRunResult> for RestoreToolResult {
    fn from(d: RestoreDryRunResult) -> Self {
        RestoreToolResult {
            ok: d.ok,
            dry_run: true,
            preview: d.at_sequence.is_some(),
            agent_id: d.agent_id,
            sequence: d.sequence,
            at_sequence: d.at_sequence,
            agent_name: None,
            runtime: None,
            memory_records: None,
            workspace: None,
            preview_path: None,
            would_write: Some(d.would_write),
            warnings: d.warnings,
        }
    }
}

/// Result of a successful [`pull_cloud_base`] call.
pub(crate) struct CloudBase {
    /// The cloud sequence after applying any deltas.
    pub latest_sequence: u64,
    /// Path the merged archive was written to (`~/.alf/state/{id}-snapshot.alf`).
    pub local_base: PathBuf,
}

/// Merge a base snapshot with zero or more delta archives (same semantics as cloud restore).
#[cfg(test)]
pub(crate) fn merge_snapshot_with_deltas(
    snapshot_bytes: &[u8],
    delta_byte_vecs: &[Vec<u8>],
) -> Result<Vec<u8>> {
    let delta_refs: Vec<&[u8]> = delta_byte_vecs.iter().map(|v| v.as_slice()).collect();
    rebuild_snapshot(snapshot_bytes, &delta_refs).map_err(|e| anyhow::anyhow!(e))
}

/// Fetch the restore manifest, download snapshot + deltas, and merge them.
///
/// `up_to_sequence = None` returns the head of history. `Some(N)` returns a
/// point-in-time view bounded by sequence `N` (see the service-side
/// `up_to_sequence` query parameter).
///
/// This helper performs no disk I/O beyond reading the merged archive into
/// memory — it does **not** write `~/.alf/state/`. Use [`pull_cloud_base`]
/// when the local sync state should be updated.
fn fetch_restore_payload(
    client: &ApiClient,
    agent_id: Uuid,
    up_to_sequence: Option<u64>,
    progress: Progress,
) -> Result<(Vec<u8>, u64)> {
    let (final_bytes, latest_sequence, _) =
        fetch_restore_payload_with_snapshot(client, agent_id, up_to_sequence, progress)?;
    Ok((final_bytes, latest_sequence))
}
fn fetch_restore_payload_with_snapshot(
    client: &ApiClient,
    agent_id: Uuid,
    up_to_sequence: Option<u64>,
    progress: Progress,
) -> Result<(Vec<u8>, u64, Vec<u8>)> {
    progress.emit("  Fetching restore manifest...");
    let restore = client.restore(agent_id, up_to_sequence)?;

    let snapshot_bytes = match &restore.snapshot {
        Some(snap) => {
            progress.emit(&format!(
                "  Downloading snapshot (sequence {})...",
                snap.sequence
            ));
            client.download_presigned(&snap.url)?
        }
        None => {
            anyhow::bail!(
                "No snapshot available for agent {}. \
                 The agent must be synced at least once before restoring.",
                agent_id
            );
        }
    };

    let snapshot_sequence = restore.snapshot.as_ref().map(|s| s.sequence).unwrap_or(0);

    let delta_byte_vecs: Vec<Vec<u8>> = if restore.deltas.is_empty() {
        progress.emit("  No additional deltas to apply.");
        Vec::new()
    } else {
        progress.emit(&format!(
            "  Downloading {} delta(s)...",
            restore.deltas.len()
        ));
        let mut out = Vec::with_capacity(restore.deltas.len());
        for (i, delta_info) in restore.deltas.iter().enumerate() {
            progress.emit(&format!(
                "  Downloading delta {} of {} (sequence {})...",
                i + 1,
                restore.deltas.len(),
                delta_info.sequence
            ));
            out.push(client.download_presigned(&delta_info.url)?);
        }
        progress.emit(&format!(
            "  Merging {} delta(s) into snapshot...",
            restore.deltas.len()
        ));
        out
    };

    let latest_sequence = validated_restore_sequence(
        &snapshot_bytes,
        snapshot_sequence,
        &restore.deltas,
        &delta_byte_vecs,
        up_to_sequence,
    )?;

    let delta_refs: Vec<&[u8]> = delta_byte_vecs
        .iter()
        .map(|bytes| bytes.as_slice())
        .collect();
    let final_bytes = rebuild_snapshot_with_sequence(&snapshot_bytes, &delta_refs, latest_sequence)
        .context("Failed to merge snapshot and deltas for restore")?;
    Ok((final_bytes, latest_sequence, snapshot_bytes))
}

/// A cloud restore payload that has been fetched and rebuilt in memory but
/// has not been committed to local sync state.
pub(crate) struct FetchedCloudBase {
    pub(crate) snapshot_bytes: Vec<u8>,
    pub(crate) final_bytes: Vec<u8>,
    pub(crate) latest_sequence: u64,
}

/// Fetch the cloud snapshot + deltas (head) for `agent_id` and rebuild the
/// complete archive in memory. This phase does not mutate local state.
pub(crate) fn fetch_cloud_base(
    client: &ApiClient,
    agent_id: Uuid,
    progress: Progress,
) -> Result<FetchedCloudBase> {
    let (final_bytes, latest_sequence, snapshot_bytes) =
        fetch_restore_payload_with_snapshot(client, agent_id, None, progress)?;
    Ok(FetchedCloudBase {
        snapshot_bytes,
        final_bytes,
        latest_sequence,
    })
}

/// Persist a fetched cloud base after the caller has established that its
/// workspace is safe to pair with this cursor.
///
/// `base.alf` is written before `state.toml`, preserving the existing
/// state-present-implies-base-present invariant.
pub(crate) fn persist_cloud_base(agent_id: Uuid, cloud: &FetchedCloudBase) -> Result<PathBuf> {
    let local_base = local_base_path(agent_id)?;
    if let Some(parent) = local_base.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create state directory {}", parent.display()))?;
    }
    crate::fs_private::write_private_atomic_bytes(&local_base, &cloud.final_bytes).with_context(
        || {
            format!(
                "Failed to write restored snapshot base at {}",
                local_base.display()
            )
        },
    )?;

    let state = AgentState {
        agent_id,
        last_synced_sequence: Some(cloud.latest_sequence),
        last_synced_at: Some(Utc::now()),
    };
    state.save()?;
    Ok(local_base)
}

/// Download the cloud snapshot + deltas (head), merge them, and immediately
/// persist the result. `alf sync --recover` intentionally uses this composed
/// helper because it never mutates the workspace.
pub(crate) fn pull_cloud_base(
    client: &ApiClient,
    agent_id: Uuid,
    progress: Progress,
) -> Result<CloudBase> {
    let fetched = fetch_cloud_base(client, agent_id, progress)?;
    let local_base = persist_cloud_base(agent_id, &fetched)?;
    Ok(CloudBase {
        latest_sequence: fetched.latest_sequence,
        local_base,
    })
}

/// Fetch a point-in-time snapshot at `up_to_sequence` and return the merged
/// archive bytes plus the resulting sequence number.
///
/// **Read-only preview**: this helper deliberately does **not** write to
/// `~/.alf/state/`. The local sync cursor remains pointed at head so a
/// subsequent `alf sync` is unaffected. See `docs/how_alf_syncs.md` for the
/// rationale.
fn fetch_point_in_time(
    client: &ApiClient,
    agent_id: Uuid,
    up_to_sequence: u64,
    progress: Progress,
) -> Result<(Vec<u8>, u64)> {
    fetch_restore_payload(client, agent_id, Some(up_to_sequence), progress)
}

/// Everything a restore needs, resolved once. Shared by the CLI `run` and the
/// MCP `run_for_mcp` seam so the two resolve the agent/workspace identically.
struct RestoreTarget {
    config: Config,
    adapt: Box<dyn Adapter>,
    agent_id: Uuid,
    workspace: PathBuf,
    client: ApiClient,
}

/// Resolve the runtime adapter, agent id, workspace, and API client for a
/// restore. No printing, no network beyond client construction.
fn resolve_target(
    runtime: &str,
    workspace_flag: Option<&Path>,
    agent_arg: Option<&str>,
) -> Result<RestoreTarget> {
    let mut config = Config::load()?;

    let adapt = adapter::get_adapter(runtime).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown runtime '{}'. Supported: {}",
            runtime,
            adapter::supported_runtimes()
        )
    })?;

    // Alias-or-id via the mapping; an unmapped UUID passes through verbatim
    // (restore-by-UUID onto a fresh host); legacy sole-state-file fallback
    // when the mapping is empty.
    let agent_id: Uuid = selector::resolve_for_cloud_op(
        &mut config,
        adapt.as_ref(),
        runtime,
        workspace_flag,
        agent_arg,
    )?;

    // Workspace: -w flag → the agent's mapped workspace → [defaults].workspace.
    let workspace: PathBuf = match workspace_flag {
        Some(w) => w.to_path_buf(),
        None => match config.agents.iter().find(|a| a.alf_agent_id == agent_id) {
            Some(row) => PathBuf::from(&row.workspace),
            None => config.resolve_workspace(None)?,
        },
    };

    let client = ApiClient::from_config(&config)?;

    Ok(RestoreTarget {
        config,
        adapt,
        agent_id,
        workspace,
        client,
    })
}

/// Perform a head or point-in-time restore and import the merged archive into
/// `workspace`. Returns the raw import report plus the resolved cloud sequence.
/// No printing — interstitial messages go to `progress` (stderr for the CLI, a
/// progress notification for MCP).
/// The preview directory for a point-in-time restore (manual §3.4):
/// `~/.alf/preview/{agent_id}/seq-{N}/`. Honors `ALF_HOME`.
fn preview_dir(agent_id: Uuid, seq: u64) -> Result<PathBuf> {
    let home = alf_core::home_dir().context("Could not determine home directory")?;
    Ok(home
        .join(".alf")
        .join("preview")
        .join(agent_id.to_string())
        .join(format!("seq-{seq}")))
}

/// Best-effort prune: keep only the `keep` newest `seq-*` previews (by mtime)
/// for this agent. Errors are ignored — pruning must never fail a restore.
fn prune_previews(agent_id: Uuid, keep: usize) {
    let Ok(base) = preview_dir(agent_id, 0).map(|p| p.parent().map(Path::to_path_buf)) else {
        return;
    };
    let Some(base) = base else { return };
    prune_seq_dirs(&base, keep);
}

/// How long a materialized preview may linger. A preview is inspection
/// scratch: keeping it past a day serves nobody and (with
/// `--with-credentials`) leaves decrypted secrets on disk — including
/// pre-rotation ones a later `alf vault rotate-key` cannot reach (MIN-12).
const PREVIEW_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// The `seq-*` prune, factored for testing: drop anything older than
/// [`PREVIEW_TTL`], then keep only the `keep` newest of what remains.
fn prune_seq_dirs(base: &Path, keep: usize) {
    prune_seq_dirs_at(base, keep, std::time::SystemTime::now(), PREVIEW_TTL)
}

/// [`prune_seq_dirs`] with the clock injected (pure enough to unit-test both
/// the TTL sweep and the keep-N cap without sleeping).
fn prune_seq_dirs_at(
    base: &Path,
    keep: usize,
    now: std::time::SystemTime,
    ttl: std::time::Duration,
) {
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("seq-"))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            meta.is_dir().then(|| {
                (
                    meta.modified().ok().unwrap_or(std::time::UNIX_EPOCH),
                    e.path(),
                )
            })
        })
        .collect();
    // Expired first — a stale preview goes regardless of how few there are.
    dirs.retain(|(mtime, path)| {
        let expired = now
            .duration_since(*mtime)
            .map(|age| age > ttl)
            .unwrap_or(false);
        if expired {
            let _ = fs::remove_dir_all(path);
        }
        !expired
    });
    dirs.sort_by_key(|(mtime, _)| std::cmp::Reverse(*mtime)); // newest first
    for (_, path) in dirs.into_iter().skip(keep) {
        let _ = fs::remove_dir_all(path);
    }
}

/// Remove every preview for `agent_id` (the whole `~/.alf/preview/{id}/` tree).
/// Called by `alf purge`, and available as the "forget everything I previewed"
/// operation.
pub(crate) fn purge_previews(agent_id: Uuid) {
    if let Ok(Some(base)) = preview_dir(agent_id, 0).map(|p| p.parent().map(Path::to_path_buf)) {
        let _ = fs::remove_dir_all(base);
    }
}

/// Render the only safe recovery command for an interrupted head restore.
fn restore_resume_command(runtime: &str, workspace: &Path, agent_id: Uuid) -> String {
    format!(
        "alf restore -r {runtime} -w {} --agent {agent_id}",
        workspace.display()
    )
}

/// Bind a restore record to one concrete live workspace. Existing paths are
/// canonicalized so equivalent symlink spellings resume the same workspace;
/// a not-yet-created workspace is made absolute without changing the disk.
fn workspace_binding(workspace: &Path) -> Result<PathBuf> {
    match workspace.canonicalize() {
        Ok(path) => Ok(path),
        Err(_) if workspace.is_absolute() => Ok(workspace.to_path_buf()),
        Err(_) => Ok(std::env::current_dir()
            .context("Could not determine the current directory for restore")?
            .join(workspace)),
    }
}

fn restore_incomplete_error(
    agent_id: Uuid,
    runtime: &str,
    requested_workspace: &Path,
    detail: impl Into<String>,
) -> anyhow::Error {
    CliError {
        code: codes::RESTORE_INCOMPLETE,
        cause: detail.into(),
        remedy: format!(
            "Re-run {} to complete the interrupted head restore before syncing.",
            restore_resume_command(runtime, requested_workspace, agent_id)
        ),
    }
    .into()
}

/// Refuse to overwrite a valid restore marker from another runtime or
/// workspace. Completing in a different workspace could make a partially
/// imported original workspace sync against the newly committed base.
fn ensure_head_restore_can_resume(
    agent_id: Uuid,
    runtime: &str,
    workspace: &Path,
) -> Result<PathBuf> {
    let requested = workspace_binding(workspace)?;
    match load_restore_inflight(agent_id) {
        Ok(None) => Ok(requested),
        Ok(Some(record)) if record.runtime == runtime && record.workspace == requested => Ok(requested),
        Ok(Some(record)) => Err(restore_incomplete_error(
            agent_id,
            runtime,
            &record.workspace,
            format!(
                "Head restore for agent {agent_id} is incomplete in {} (runtime {}, phase {:?}); refusing to complete it in {} for runtime {runtime}.",
                record.workspace.display(),
                record.runtime,
                record.phase,
                requested.display(),
            ),
        )),
        Err(err) => {
            let marker = restore_inflight_path(agent_id)?;
            Err(restore_incomplete_error(
                agent_id,
                runtime,
                workspace,
                format!(
                    "Restore-in-flight record at {} is malformed or unreadable ({err:#}); refusing to replace it because its original workspace cannot be verified.",
                    marker.display()
                ),
            ))
        }
    }
}

/// Common sync gate for the CLI, MCP tool, watch loop, and `--recover` path.
/// A marker remains authoritative even when its contents cannot be parsed.
pub(crate) fn ensure_sync_not_during_restore(
    agent_id: Uuid,
    runtime: &str,
    workspace: &Path,
) -> Result<()> {
    match load_restore_inflight(agent_id) {
        Ok(None) => Ok(()),
        Ok(Some(record)) => Err(restore_incomplete_error(
            agent_id,
            runtime,
            &record.workspace,
            format!(
                "Head restore for agent {agent_id} is incomplete in {} (phase {:?}); refusing to sync {} against an untrusted local base.",
                record.workspace.display(),
                record.phase,
                workspace.display(),
            ),
        )),
        Err(err) => {
            let marker = restore_inflight_path(agent_id)?;
            Err(restore_incomplete_error(
                agent_id,
                runtime,
                workspace,
                format!(
                    "Restore-in-flight record at {} is malformed or unreadable ({err:#}); refusing to sync until the original workspace is verified.",
                    marker.display()
                ),
            ))
        }
    }
}

fn optional_file_sha256(path: &Path) -> Result<Option<String>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(alf_core::ids::sha256_hex(&bytes))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| {
            format!(
                "Failed to read existing restore state at {}",
                path.display()
            )
        }),
    }
}

fn make_restore_inflight(
    agent_id: Uuid,
    runtime: &str,
    workspace: PathBuf,
    cloud: &FetchedCloudBase,
) -> Result<RestoreInflight> {
    Ok(RestoreInflight {
        version: RestoreInflight::VERSION,
        agent_id,
        runtime: runtime.to_string(),
        workspace,
        target_sequence: cloud.latest_sequence,
        staged_archive_sha256: alf_core::ids::sha256_hex(&cloud.final_bytes),
        previous_base_sha256: optional_file_sha256(&local_base_path(agent_id)?)?,
        previous_state_sha256: optional_file_sha256(&state_file_path(agent_id)?)?,
        phase: RestoreInflightPhase::Importing,
    })
}

#[cfg(feature = "fault-injection")]
fn fault_after_restore_importing() {
    if std::env::var_os("ALF_RESTORE_FAULT_AFTER_IMPORTING").is_some() {
        eprintln!("alf: ALF_RESTORE_FAULT_AFTER_IMPORTING set — aborting after restore marker");
        std::process::exit(137);
    }
}

#[cfg(not(feature = "fault-injection"))]
fn fault_after_restore_importing() {}

#[cfg(feature = "fault-injection")]
fn fault_after_restore_imported() {
    if std::env::var_os("ALF_RESTORE_FAULT_AFTER_IMPORTED").is_some() {
        eprintln!("alf: ALF_RESTORE_FAULT_AFTER_IMPORTED set — aborting after workspace import");
        std::process::exit(137);
    }
}

#[cfg(not(feature = "fault-injection"))]
fn fault_after_restore_imported() {}

/// Perform a head restore into the live workspace, or a point-in-time preview
/// into the preview directory. Returns the import report, the resolved cloud
/// sequence, and the directory that was written.
#[allow(clippy::too_many_arguments)]
fn perform_restore(
    client: &ApiClient,
    agent_id: Uuid,
    workspace: &Path,
    runtime: &str,
    adapt: &dyn Adapter,
    at_sequence: Option<u64>,
    mode: RestoreMode,
    key_args: &VaultKeyArgs,
    with_credentials: bool,
    progress: Progress,
) -> Result<(ImportReport, u64, PathBuf)> {
    // A head restore fetches first but must not move local state until the
    // live adapter import has succeeded. Point-in-time previews remain fully
    // read-only with respect to shared state.
    let head_workspace = at_sequence
        .is_none()
        .then(|| ensure_head_restore_can_resume(agent_id, runtime, workspace))
        .transpose()?;
    let cloud_base = match at_sequence {
        None => Some(fetch_cloud_base(client, agent_id, progress)?),
        Some(_) => None,
    };
    let point_in_time = match at_sequence {
        None => None,
        Some(n) => Some(fetch_point_in_time(client, agent_id, n, progress)?),
    };
    let (final_bytes, latest_sequence) = match (&cloud_base, &point_in_time) {
        (Some(base), None) => (base.final_bytes.as_slice(), base.latest_sequence),
        (None, Some((bytes, sequence))) => (bytes.as_slice(), *sequence),
        _ => unreachable!("head and point-in-time payloads are mutually exclusive"),
    };

    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    let temp_alf = temp_dir.path().join("restored.alf");
    fs::write(&temp_alf, final_bytes)?;

    let (target_dir, mode, merge_warning) = match at_sequence {
        None => (workspace.to_path_buf(), mode, None),
        Some(n) => {
            let dir = preview_dir(agent_id, n)?;
            if dir.exists() {
                fs::remove_dir_all(&dir)
                    .with_context(|| format!("clearing stale preview {}", dir.display()))?;
            }
            // 0700: a preview materializes historical agent content — memory,
            // identity, and (with --with-credentials) decrypted secrets — so
            // neither the tree nor its listing may be world-readable.
            alf_core::fs_atomic::create_dir_private(&dir)
                .with_context(|| format!("creating preview dir {}", dir.display()))?;
            // Merge is meaningless against an empty preview dir: force Total.
            let warning = matches!(mode, RestoreMode::Merge).then(|| {
                "mode=merge ignored for point-in-time previews (imported as total into an \
                 empty preview directory)"
                    .to_string()
            });
            (dir, RestoreMode::Total, warning)
        }
    };

    progress.emit(&format!(
        "  Importing into {}...",
        if at_sequence.is_some() {
            "preview directory"
        } else {
            "workspace"
        }
    ));
    // Previews do NOT decrypt by default (MIN-12): materializing plaintext
    // secrets is not what "inspect history" needs, and the copy would outlive
    // the inspection. `--with-credentials` opts in explicitly.
    let decrypt = at_sequence.is_none() || with_credentials;
    let resolved_key = if decrypt {
        vault_key::resolve(key_args, runtime, Some(agent_id))?
    } else {
        None
    };
    if let Some((_, source)) = &resolved_key {
        progress.emit(&format!(
            "Using vault key from {} — credentials will be decrypted and restored",
            source.label()
        ));
    }
    let import_options = ImportOptions {
        vault_key: resolved_key.as_ref().map(|(k, _)| k),
        mode,
        // Sandboxed: keep Layer 4 inside the preview tree, never the live vault.
        preview: at_sequence.is_some(),
    };
    // The marker becomes durable at the final safe point before a live adapter
    // can mutate the workspace. All earlier failures leave base/cursor alone;
    // every later failure leaves this guard for sync to observe.
    let mut restore_marker = cloud_base
        .as_ref()
        .map(|cloud| {
            make_restore_inflight(
                agent_id,
                runtime,
                head_workspace
                    .clone()
                    .expect("head restore has a workspace binding"),
                cloud,
            )
        })
        .transpose()?;
    if let Some(marker) = &restore_marker {
        save_restore_inflight(marker)?;
        fault_after_restore_importing();
    }

    let mut import_report = adapt.import_with_options(&temp_alf, &target_dir, import_options)?;
    if let Some(marker) = restore_marker.as_mut() {
        marker.phase = RestoreInflightPhase::Imported;
        save_restore_inflight(marker)?;
        fault_after_restore_imported();
        persist_cloud_base(
            agent_id,
            cloud_base
                .as_ref()
                .expect("restore marker requires cloud base"),
        )?;
        clear_restore_inflight(agent_id)?;
    }

    if let Some(w) = merge_warning {
        import_report.warnings.push(w);
    }
    if at_sequence.is_some() && !with_credentials {
        import_report.warnings.push(
            "credentials were not decrypted into this preview (pass --with-credentials \
             to include them); the live vault is untouched either way"
                .to_string(),
        );
    }
    // Cleanup runs on EVERY restore, not just previews: keep the 3 newest and
    // drop anything older than the TTL, so an inspected preview does not sit
    // on disk indefinitely waiting for two more previews to push it out.
    prune_previews(agent_id, 3);
    Ok((import_report, latest_sequence, target_dir))
}

/// Assemble the JSON `RestoreResult` from a completed restore. Shared by the CLI
/// JSON branch and the MCP `alf_restore` tool.
fn build_restore_result(
    agent_id: Uuid,
    runtime: &str,
    written_to: &Path,
    report: &ImportReport,
    latest_sequence: u64,
    at_sequence: Option<u64>,
) -> RestoreResult {
    RestoreResult {
        ok: true,
        agent_id: agent_id.to_string(),
        agent_name: report.agent_name.clone(),
        sequence: latest_sequence,
        runtime: runtime.to_string(),
        memory_records: report.memory_records,
        workspace: written_to.to_string_lossy().into(),
        preview: at_sequence.is_some(),
        at_sequence,
        preview_path: at_sequence
            .is_some()
            .then(|| written_to.to_string_lossy().into_owned()),
        warnings: report.warnings.clone(),
    }
}

/// Fetch and decode the archive for a dry-run and enumerate what a restore
/// *would* write — touching nothing. Returns the enumeration plus the resolved
/// cloud sequence. Uses [`fetch_restore_payload`] directly (never
/// [`pull_cloud_base`]) so `~/.alf/state/` is untouched.
fn perform_dry_run(
    client: &ApiClient,
    agent_id: Uuid,
    adapt: &dyn Adapter,
    at_sequence: Option<u64>,
    progress: Progress,
) -> Result<(ArchiveEnumeration, u64)> {
    let (final_bytes, latest_sequence) =
        fetch_restore_payload(client, agent_id, at_sequence, progress)?;

    // Decode in a tempdir that is removed on exit — no workspace, no state.
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    let temp_alf = temp_dir.path().join("restored.alf");
    fs::write(&temp_alf, &final_bytes)?;

    let enumeration = adapt.enumerate_archive(&temp_alf)?;
    Ok((enumeration, latest_sequence))
}

/// Assemble the JSON `RestoreDryRunResult` from a dry-run enumeration.
fn build_dry_run_result(
    agent_id: Uuid,
    enumeration: ArchiveEnumeration,
    latest_sequence: u64,
    at_sequence: Option<u64>,
) -> RestoreDryRunResult {
    RestoreDryRunResult {
        ok: true,
        dry_run: true,
        agent_id: agent_id.to_string(),
        sequence: latest_sequence,
        would_write: enumeration.files,
        at_sequence,
        warnings: enumeration.warnings,
    }
}

/// MCP `alf_restore` seam: resolve, then either a dry-run preview or a real
/// head/PIT restore. Returns the typed result the tool serializes; no stdout.
///
/// M3 will wrap this with the watch-loop pause (the loop must not sync a
/// workspace mid-restore); the seam is factored so that hook has one call site.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_for_mcp(
    runtime: &str,
    workspace_flag: Option<&Path>,
    agent: Option<&str>,
    at_sequence: Option<u64>,
    dry_run: bool,
    mode: RestoreMode,
    key_args: &VaultKeyArgs,
    progress: Progress,
) -> Result<RestoreToolResult> {
    // Over MCP a preview NEVER decrypts (MIN-12): materializing plaintext
    // secrets is a human ceremony (`alf restore --at-sequence N
    // --with-credentials`), so the tool surface deliberately has no opt-in.
    let with_credentials = false;
    let target = resolve_target(runtime, workspace_flag, agent)?;

    // WP1 + RF-012: move any legacy vault/key to the per-agent layout BEFORE the
    // head-restore L3 lock below. Migration takes its own legacy→agent guards, so
    // it must not run under the restore lock (`flock` does not nest, and
    // legacy-after-agent would deadlock a concurrent locked vault mutation).
    // Skipped for a dry-run, which writes nothing.
    if !dry_run {
        vault_migrate::require_migrated_locked(&target.config, runtime)?;
    }

    // L3 (manual §6): a HEAD restore rewrites the live workspace and moves the
    // sync state, so it takes the per-agent advisory lock. Previews and dry
    // runs are read-only with respect to shared state — lock-free.
    let _agent_lock = if !dry_run && at_sequence.is_none() {
        Some(crate::commands::mcp::watch::acquire_agent_lock_timeout(
            target.agent_id,
            std::time::Duration::from_secs(10),
        )?)
    } else {
        None
    };

    if dry_run {
        let (enumeration, latest_sequence) = perform_dry_run(
            &target.client,
            target.agent_id,
            target.adapt.as_ref(),
            at_sequence,
            progress,
        )?;
        return Ok(build_dry_run_result(
            target.agent_id,
            enumeration,
            latest_sequence,
            at_sequence,
        )
        .into());
    }

    let (report, latest_sequence, written_to) = perform_restore(
        &target.client,
        target.agent_id,
        &target.workspace,
        runtime,
        target.adapt.as_ref(),
        at_sequence,
        mode,
        key_args,
        with_credentials,
        progress,
    )?;
    Ok(build_restore_result(
        target.agent_id,
        runtime,
        &written_to,
        &report,
        latest_sequence,
        at_sequence,
    )
    .into())
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    runtime: &str,
    workspace_flag: Option<&Path>,
    agent_arg: Option<&str>,
    at_sequence: Option<u64>,
    dry_run: bool,
    mode: RestoreMode,
    key_args: &VaultKeyArgs,
    with_credentials: bool,
) -> Result<()> {
    let human = output::human_mode();
    let preview = at_sequence.is_some();

    let target = resolve_target(runtime, workspace_flag, agent_arg)?;
    let agent_id = target.agent_id;
    let workspace = target.workspace.as_path();
    let adapt = target.adapt.as_ref();

    if dry_run {
        return run_dry_run(&target.client, agent_id, adapt, at_sequence, human);
    }

    // WP1 + RF-012: move any legacy vault/key to the per-agent layout BEFORE the
    // head-restore L3 lock — otherwise the key leg is missed on the first
    // post-upgrade restore and the legacy file survives as a shadow vault.
    // Migration takes its own legacy→agent guards, so it must run before (never
    // under) the restore lock: `flock` does not nest, and legacy-after-agent
    // would deadlock a concurrent locked vault mutation. (Dry runs returned
    // above and write nothing.)
    vault_migrate::require_migrated_locked(&target.config, runtime)?;

    // L3 (MAJ-6): a CLI HEAD restore rewrites the live workspace and moves the
    // sync state — take the same cross-process advisory lock the MCP tools and
    // the watch loop hold, so a concurrent watch export can never upload a
    // half-restored workspace. Previews are lock-free (dry runs returned
    // above); uncontended (no server running) this costs one open+flock.
    let _agent_lock = if !preview {
        Some(crate::commands::mcp::watch::acquire_agent_lock_timeout(
            agent_id,
            std::time::Duration::from_secs(10),
        )?)
    } else {
        None
    };

    if human {
        if let Some(n) = at_sequence {
            println!(
                "{} Preview: materializing agent {} at sequence {} into a preview directory...",
                "▸".blue().bold(),
                &agent_id.to_string()[..8],
                n,
            );
            println!(
                "  {}",
                "Read-only preview — the live workspace and ~/.alf/state will not be touched."
                    .yellow()
            );
        } else {
            println!(
                "{} Restoring agent {} into {} workspace...",
                "▸".blue().bold(),
                &agent_id.to_string()[..8],
                adapt.name()
            );
        }
        println!("  Agent:     {agent_id}");
        println!("  Runtime:   {}", adapt.name());
        println!("  Workspace: {}", workspace.display());
        println!();
    } else {
        output::progress(&format!(
            "Restoring agent {}...",
            &agent_id.to_string()[..8]
        ));
    }

    let (import_report, latest_sequence, written_to) = perform_restore(
        &target.client,
        agent_id,
        workspace,
        runtime,
        adapt,
        at_sequence,
        mode,
        key_args,
        with_credentials,
        Progress::stderr(),
    )?;

    if human {
        println!();
        if preview {
            println!(
                "{} Preview written to {} (workspace and ~/.alf/state untouched)",
                "✓".green().bold(),
                written_to.display()
            );
        } else {
            let state_path = crate::state::state_file_path(agent_id)?;
            println!("  State file:   {}", state_path.display());
            println!("{} Restore complete", "✓".green().bold());
        }
        println!();
        println!("  Agent:      {}", import_report.agent_name);
        println!("  Memories:   {}", import_report.memory_records);
        if import_report.identity_imported {
            println!("  Identity:   restored");
        }
        if import_report.principals_count > 0 {
            println!("  Principals: {}", import_report.principals_count);
        }
        if import_report.credentials_count > 0 {
            println!("  Credentials: {}", import_report.credentials_count);
        }
        println!("  Sequence:   {latest_sequence}");
        println!();
        if preview {
            println!("  Preview dir: {}", written_to.display());
        } else {
            println!("  Workspace: {}", workspace.display());
        }

        if !import_report.warnings.is_empty() {
            println!();
            println!("  {} Warnings:", "⚠".yellow().bold());
            for w in &import_report.warnings {
                println!("    • {w}");
            }
        }
    } else {
        output::json(&build_restore_result(
            agent_id,
            runtime,
            &written_to,
            &import_report,
            latest_sequence,
            at_sequence,
        ));
    }

    Ok(())
}

/// `alf restore --dry-run` — fetch and decode the archive, list what *would*
/// be written, and touch nothing.
///
/// Makes the same network calls as a real restore, but fetches via
/// [`fetch_restore_payload`] directly (never [`pull_cloud_base`]) so
/// `~/.alf/state/` is untouched. The workspace is never created or written.
/// `at_sequence` composes for free — `--at-sequence N --dry-run` previews the
/// point-in-time view.
fn run_dry_run(
    client: &ApiClient,
    agent_id: Uuid,
    adapt: &dyn Adapter,
    at_sequence: Option<u64>,
    human: bool,
) -> Result<()> {
    if human {
        println!(
            "{} Preview: restoring agent {} ({} dry run — workspace and ~/.alf/state untouched)...",
            "▸".blue().bold(),
            &agent_id.to_string()[..8],
            adapt.name()
        );
    } else {
        output::progress(&format!(
            "Previewing restore of agent {} (dry run)...",
            &agent_id.to_string()[..8]
        ));
    }

    let (enumeration, latest_sequence) =
        perform_dry_run(client, agent_id, adapt, at_sequence, Progress::stderr())?;

    if human {
        println!();
        println!("{} Dry run complete — nothing written", "✓".green().bold());
        println!();
        println!("  Agent:     {agent_id}");
        println!("  Sequence:  {latest_sequence}");
        println!("  Would write {} file(s):", enumeration.files.len());
        for f in &enumeration.files {
            println!("    {}", f.path);
        }
        if !enumeration.warnings.is_empty() {
            println!();
            println!("  {} Warnings:", "⚠".yellow().bold());
            for w in &enumeration.warnings {
                println!("    • {w}");
            }
        }
    } else {
        output::json(&build_dry_run_result(
            agent_id,
            enumeration,
            latest_sequence,
            at_sequence,
        ));
    }

    Ok(())
}

/// Validate the service metadata and downloaded archive manifests before using
/// their cursor for local optimistic concurrency.
fn validated_restore_sequence(
    snapshot_bytes: &[u8],
    snapshot_sequence: u64,
    deltas: &[RestoreDelta],
    delta_byte_vecs: &[Vec<u8>],
    up_to_sequence: Option<u64>,
) -> Result<u64> {
    if deltas.len() != delta_byte_vecs.len() {
        anyhow::bail!(
            "restore response has {} delta metadata entries but {} downloaded delta archives",
            deltas.len(),
            delta_byte_vecs.len()
        );
    }

    if let Some(bound) = up_to_sequence {
        if snapshot_sequence > bound {
            anyhow::bail!(
                "restore snapshot sequence {} exceeds requested point-in-time bound {}",
                snapshot_sequence,
                bound
            );
        }
    }

    let snapshot = AlfReader::new(Cursor::new(snapshot_bytes))?
        .manifest()
        .clone();
    let mut cursor = snapshot_sequence;

    for (delta_info, delta_bytes) in deltas.iter().zip(delta_byte_vecs) {
        if let Some(bound) = up_to_sequence {
            if delta_info.sequence > bound {
                anyhow::bail!(
                    "restore delta sequence {} exceeds requested point-in-time bound {}",
                    delta_info.sequence,
                    bound
                );
            }
        }

        let expected = cursor
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("restore sequence overflow after {}", cursor))?;
        if delta_info.sequence != expected {
            anyhow::bail!(
                "restore delta sequence {} is not the expected next sequence {}",
                delta_info.sequence,
                expected
            );
        }

        let delta = DeltaReader::new(Cursor::new(delta_bytes))?;
        let manifest = delta.manifest();
        if manifest.agent.id != snapshot.agent.id {
            anyhow::bail!(
                "restore delta sequence {} belongs to a different agent",
                delta_info.sequence
            );
        }
        if manifest.sync.base_sequence != cursor
            || (manifest.sync.new_sequence != 0
                && manifest.sync.new_sequence != delta_info.sequence)
        {
            anyhow::bail!(
                "restore delta sequence {} has inconsistent base/new cursor {}/{}",
                delta_info.sequence,
                manifest.sync.base_sequence,
                manifest.sync.new_sequence
            );
        }
        cursor = delta_info.sequence;
    }

    if let Some(sync) = snapshot.sync {
        if sync.last_sequence > cursor {
            anyhow::bail!(
                "restore snapshot cursor {} is ahead of validated service head {}",
                sync.last_sequence,
                cursor
            );
        }
    }

    Ok(cursor)
}
#[cfg(test)]
mod tests {
    use super::*;
    use alf_core::archive::AlfReader;
    use std::io::Cursor;

    /// MIN-12: a preview is inspection scratch — it expires, regardless of how
    /// few previews exist. (The keep-N cap alone let one sit forever until two
    /// more previews pushed it out.)
    #[test]
    fn prune_sweeps_previews_older_than_the_ttl() {
        let base = tempfile::tempdir().unwrap();
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        let ttl = std::time::Duration::from_secs(3600);
        for (n, age) in [(1u64, 7200u64), (2, 600)] {
            let d = base.path().join(format!("seq-{n}"));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::File::open(&d)
                .unwrap()
                .set_modified(now - std::time::Duration::from_secs(age))
                .unwrap();
        }
        // keep = 3, so nothing is dropped by the cap: only the TTL can act.
        prune_seq_dirs_at(base.path(), 3, now, ttl);
        assert!(
            !base.path().join("seq-1").exists(),
            "a preview older than the TTL is swept even when under the keep cap"
        );
        assert!(base.path().join("seq-2").exists(), "a fresh preview stays");
    }

    /// MIN-12: the preview tree is created 0700 — its contents are historical
    /// agent memory/identity (and, opted in, decrypted secrets).
    #[test]
    #[cfg(unix)]
    fn preview_dirs_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("preview").join("seq-3");
        alf_core::fs_atomic::create_dir_private(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "preview dir must be owner-only, got {mode:o}");
    }

    #[test]
    fn preview_dir_layout_and_prune_keeps_three() {
        // Path shape (relative components — no env mutation, works on any HOME).
        let id = Uuid::nil();
        let dir = preview_dir(id, 7).unwrap();
        let tail: Vec<_> = dir
            .components()
            .rev()
            .take(4)
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            tail,
            vec![
                "seq-7".to_string(),
                id.to_string(),
                "preview".to_string(),
                ".alf".to_string()
            ],
            "preview dir must be ~/.alf/preview/{{agent}}/seq-{{N}}"
        );

        // Prune keeps the 3 newest seq-* dirs (mtime order) and nothing else.
        let base = tempfile::tempdir().unwrap();
        for n in 1..=4u64 {
            let d = base.path().join(format!("seq-{n}"));
            std::fs::create_dir_all(&d).unwrap();
            let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(n * 1000);
            let f = std::fs::File::open(&d).unwrap();
            f.set_modified(t).unwrap();
        }
        std::fs::create_dir_all(base.path().join("not-a-preview")).unwrap();
        // Clock injected: `now` sits just after the fixture mtimes and the TTL
        // is effectively infinite, so this asserts the keep-N cap alone (the
        // TTL sweep has its own test below).
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(4001);
        prune_seq_dirs_at(
            base.path(),
            3,
            now,
            std::time::Duration::from_secs(u32::MAX as u64),
        );
        assert!(!base.path().join("seq-1").exists(), "oldest pruned");
        for n in 2..=4u64 {
            assert!(
                base.path().join(format!("seq-{n}")).exists(),
                "seq-{n} kept"
            );
        }
        assert!(
            base.path().join("not-a-preview").exists(),
            "non seq-* siblings are never touched"
        );
    }

    /// Offline pin for the backend-only `alf_restore` success shapes: the stdio
    /// harness only exercises restore's error path (no backend), so validate
    /// both `RestoreToolResult` variants against the declared schema here — this
    /// catches the `#[serde(default)]`/skip drift the harness cannot reach (M2a §2).
    #[test]
    fn restore_tool_result_matches_declared_output_schema() {
        let schema = serde_json::to_value(schemars::schema_for!(RestoreToolResult)).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();

        // Real restore: dry-run-only fields omitted, real-restore fields present.
        let restored: RestoreToolResult = RestoreResult {
            ok: true,
            agent_id: "a1b2c3d4".into(),
            agent_name: "Agent".into(),
            sequence: 3,
            runtime: "generic".into(),
            memory_records: 5,
            workspace: "/ws".into(),
            preview: false,
            at_sequence: None,
            preview_path: None,
            warnings: vec![],
        }
        .into();
        let instance = serde_json::to_value(&restored).unwrap();
        assert!(
            instance.get("would_write").is_none() && instance.get("warnings").is_none(),
            "empty/None fields must be omitted, not over-required: {instance}"
        );
        assert!(
            validator.is_valid(&instance),
            "restored variant must validate: {instance}"
        );

        // Dry-run: would_write + warnings populated, real-restore fields omitted.
        let dry: RestoreToolResult = RestoreDryRunResult {
            ok: true,
            dry_run: true,
            agent_id: "a1b2c3d4".into(),
            sequence: 2,
            would_write: vec![FileEntry {
                path: "SOUL.md".into(),
                size: 12,
            }],
            at_sequence: Some(2),
            warnings: vec!["approximate".into()],
        }
        .into();
        let instance = serde_json::to_value(&dry).unwrap();
        assert!(
            instance.get("agent_name").is_none(),
            "real-restore fields must be omitted on a dry-run: {instance}"
        );
        assert!(
            validator.is_valid(&instance),
            "dry-run variant must validate: {instance}"
        );
    }

    use alf_core::archive::{AlfWriter, DeltaMemoryEntry, DeltaWriter};
    use alf_core::manifest::{
        AgentMetadata, ChangeInventory, DeltaAgentRef, DeltaManifest, DeltaOperation,
        DeltaSyncCursor, LayerInventory, Manifest, SyncCursor,
    };
    use alf_core::memory::{
        MemoryRecord, MemoryStatus, MemoryType, SourceProvenance, TemporalMetadata,
    };
    use chrono::{TimeZone, Utc};
    use std::collections::HashMap;

    fn make_agent_id() -> uuid::Uuid {
        uuid::Uuid::parse_str("a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d").unwrap()
    }

    fn make_manifest() -> Manifest {
        Manifest {
            alf_version: "1.0.0".into(),
            created_at: Utc::now(),
            agent: AgentMetadata {
                id: make_agent_id(),
                name: "Test Agent".into(),
                source_runtime: "test".into(),
                source_runtime_version: None,
                extra: HashMap::new(),
            },
            layers: LayerInventory {
                identity: None,
                principals: None,
                credentials: None,
                memory: None,
                attachments: None,
                extra: HashMap::new(),
            },
            runtime_hints: None,
            sync: None,
            raw_sources: vec![],
            checksum: None,
            extra: HashMap::new(),
        }
    }

    fn make_record(id_suffix: u8, content: &str) -> MemoryRecord {
        let mut id_bytes = [0u8; 16];
        id_bytes[15] = id_suffix;
        MemoryRecord {
            id: uuid::Uuid::from_bytes(id_bytes),
            agent_id: make_agent_id(),
            content: content.into(),
            memory_type: MemoryType::Semantic,
            source: SourceProvenance {
                runtime: "test".into(),
                runtime_version: None,
                origin: None,
                origin_file: None,
                extraction_method: None,
                session_id: None,
                interaction_id: None,
                identity_version: None,
                extra: HashMap::new(),
            },
            temporal: TemporalMetadata {
                created_at: Utc.with_ymd_and_hms(2026, 1, 15, 10, 0, 0).unwrap(),
                updated_at: None,
                observed_at: Some(Utc.with_ymd_and_hms(2026, 1, 15, 10, 0, 0).unwrap()),
                valid_from: None,
                valid_until: None,
                last_accessed_at: None,
                access_count: None,
                extra: HashMap::new(),
            },
            status: MemoryStatus::Active,
            namespace: "default".into(),
            category: None,
            supersedes: None,
            confidence: None,
            entities: vec![],
            tags: vec![],
            embeddings: vec![],
            related_records: vec![],
            raw_source_format: None,
            extra: HashMap::new(),
        }
    }

    fn build_minimal_snapshot(records: &[MemoryRecord]) -> Vec<u8> {
        use alf_core::manifest::MemoryPartitionInfo;
        use alf_core::partition::PartitionAssigner;
        use std::collections::BTreeMap;

        let buf = Cursor::new(Vec::new());
        let mut writer = AlfWriter::new(buf, make_manifest()).unwrap();

        let mut groups: BTreeMap<String, Vec<MemoryRecord>> = BTreeMap::new();
        for r in records {
            let path = PartitionAssigner::partition_for_record(r);
            groups.entry(path).or_default().push(r.clone());
        }
        for (file_path, group_records) in &groups {
            let (from, to) = PartitionAssigner::date_range_for_partition(file_path).unwrap();
            let info = MemoryPartitionInfo {
                file: file_path.clone(),
                from,
                to: Some(to),
                record_count: group_records.len() as u64,
                sealed: false,
                extra: HashMap::new(),
            };
            writer.add_memory_partition(info, group_records).unwrap();
        }

        writer.finish().unwrap().into_inner()
    }

    fn build_delta(base_sequence: u64, entries: &[DeltaMemoryEntry]) -> Vec<u8> {
        let delta_manifest = DeltaManifest {
            alf_version: "1.0.0".into(),
            created_at: Utc::now(),
            agent: DeltaAgentRef {
                id: make_agent_id(),
                source_runtime: Some("test".into()),
                extra: HashMap::new(),
            },
            sync: DeltaSyncCursor {
                base_sequence,
                new_sequence: base_sequence + 1,
                base_timestamp: None,
                new_timestamp: None,
                extra: HashMap::new(),
            },
            changes: ChangeInventory {
                identity: None,
                principals: None,
                credentials: None,
                memory: None,
                raw: None,
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        };

        let buf = Cursor::new(Vec::new());
        let mut writer = DeltaWriter::new(buf, delta_manifest).unwrap();
        if !entries.is_empty() {
            writer.add_memory_deltas(entries).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn read_record_contents(alf: &[u8]) -> Vec<String> {
        let mut r = AlfReader::new(Cursor::new(alf)).unwrap();
        r.read_all_memory()
            .unwrap()
            .into_iter()
            .map(|rec| rec.content)
            .collect()
    }

    fn build_empty_snapshot_with_cursor(cursor: Option<u64>) -> Vec<u8> {
        let mut manifest = make_manifest();
        manifest.sync = cursor.map(|last_sequence| SyncCursor {
            last_sequence,
            last_sync_at: None,
            extra: HashMap::new(),
        });
        let writer = AlfWriter::new(Cursor::new(Vec::new()), manifest).unwrap();
        writer.finish().unwrap().into_inner()
    }

    fn restore_delta(sequence: u64) -> RestoreDelta {
        RestoreDelta {
            url: format!("https://example.invalid/delta/{sequence}"),
            sequence,
            size_bytes: 0,
            created_at: "2026-07-30T00:00:00Z".into(),
        }
    }

    #[test]
    fn merge_with_deltas_includes_delta_memory_changes() {
        let a = make_record(1, "A");
        let b = make_record(2, "B");
        let snap = build_minimal_snapshot(&[a.clone(), b.clone()]);

        let c = make_record(3, "C");
        let delta = build_delta(
            0,
            &[DeltaMemoryEntry {
                operation: DeltaOperation::Create,
                record: c,
            }],
        );

        let merged = merge_snapshot_with_deltas(&snap, &[delta]).unwrap();
        let contents = read_record_contents(&merged);

        assert_eq!(contents.len(), 3);
        assert!(contents.contains(&"A".into()));
        assert!(contents.contains(&"B".into()));
        assert!(contents.contains(&"C".into()));
    }

    #[test]
    fn merge_with_no_deltas_keeps_records() {
        let a = make_record(1, "A");
        let snap = build_minimal_snapshot(std::slice::from_ref(&a));
        let merged = merge_snapshot_with_deltas(&snap, &[]).unwrap();
        let contents = read_record_contents(&merged);
        assert_eq!(contents, vec!["A".to_string()]);
    }

    #[test]
    fn merge_rejects_corrupt_delta() {
        let a = make_record(1, "A");
        let snap = build_minimal_snapshot(&[a]);
        let bad = vec![1u8, 2, 3];
        assert!(merge_snapshot_with_deltas(&snap, &[bad]).is_err());
    }

    #[test]
    fn merge_with_subset_of_deltas_yields_point_in_time() {
        // Simulate a point-in-time restore: the service returns a snapshot at
        // sequence 0 plus only the first M deltas (those with sequence <= N).
        // The merged result should reflect the cloud state at sequence M, not
        // include any later changes.
        let a = make_record(1, "A");
        let snap = build_minimal_snapshot(&[a]);

        let b = make_record(2, "B");
        let d1 = build_delta(
            0,
            &[DeltaMemoryEntry {
                operation: DeltaOperation::Create,
                record: b,
            }],
        );
        let c = make_record(3, "C");
        let d2 = build_delta(
            1,
            &[DeltaMemoryEntry {
                operation: DeltaOperation::Create,
                record: c,
            }],
        );

        // Head (snapshot + d1 + d2) has A, B, C.
        let head = merge_snapshot_with_deltas(&snap, &[d1.clone(), d2.clone()]).unwrap();
        let head_contents = read_record_contents(&head);
        assert_eq!(head_contents.len(), 3);

        // PIT at sequence 1 (snapshot + d1 only) has A, B — no C.
        let pit = merge_snapshot_with_deltas(&snap, &[d1]).unwrap();
        let pit_contents = read_record_contents(&pit);
        assert_eq!(pit_contents.len(), 2);
        assert!(pit_contents.contains(&"A".into()));
        assert!(pit_contents.contains(&"B".into()));
        assert!(!pit_contents.contains(&"C".into()));

        // PIT at sequence 0 (snapshot only) has just A.
        let pit0 = merge_snapshot_with_deltas(&snap, &[]).unwrap();
        let pit0_contents = read_record_contents(&pit0);
        assert_eq!(pit0_contents, vec!["A".to_string()]);
    }

    #[test]
    fn validated_restore_sequence_uses_service_metadata_and_rejects_bad_chains() {
        let uncursored = build_empty_snapshot_with_cursor(None);
        assert_eq!(
            validated_restore_sequence(&uncursored, 7, &[], &[], None).unwrap(),
            7
        );

        let legacy_cursor = build_empty_snapshot_with_cursor(Some(0));
        assert_eq!(
            validated_restore_sequence(&legacy_cursor, 7, &[], &[], None).unwrap(),
            7
        );

        let delta_8 = build_delta(7, &[]);
        let delta_9 = build_delta(8, &[]);
        let deltas = vec![restore_delta(8), restore_delta(9)];
        assert_eq!(
            validated_restore_sequence(
                &uncursored,
                7,
                &deltas,
                &[delta_8.clone(), delta_9.clone()],
                None,
            )
            .unwrap(),
            9
        );

        let future_cursor = build_empty_snapshot_with_cursor(Some(99));
        assert!(validated_restore_sequence(&future_cursor, 7, &[], &[], None).is_err());
        assert!(validated_restore_sequence(
            &uncursored,
            7,
            &[restore_delta(9), restore_delta(8)],
            &[delta_8.clone(), delta_9.clone()],
            None,
        )
        .is_err());
        assert!(validated_restore_sequence(
            &uncursored,
            7,
            &[restore_delta(8)],
            &[build_delta(8, &[])],
            None,
        )
        .is_err());
        assert!(validated_restore_sequence(
            &uncursored,
            7,
            &[restore_delta(9)],
            &[delta_9],
            Some(8),
        )
        .is_err());
    }
}
