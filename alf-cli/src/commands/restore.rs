//! `alf restore` — download and restore from the cloud.
//!
//! Flow (head restore, default):
//! 1. Load config (check API key)
//! 2. Parse agent ID
//! 3. [`pull_cloud_base`] — fetch snapshot + deltas, merge, persist `~/.alf/state/{id}-snapshot.alf`,
//!    write state.toml. Atomic-write order: base.alf BEFORE state.toml.
//! 4. Resolve adapter, import the merged archive into the workspace
//!
//! Flow (point-in-time restore, `--at-sequence N`):
//! 1-2. As above.
//! 3. [`fetch_point_in_time`] — fetch snapshot + deltas bounded by `N`, merge in memory.
//!    **Does not touch `~/.alf/state/`** — PIT restores are a read-only preview.
//! 4. Import into the workspace as usual.
//!
//! `pull_cloud_base` is also reused by `alf sync --recover` (commands/sync.rs).

use crate::adapter;
use crate::api_client::ApiClient;
use crate::config::Config;
use crate::output;
use crate::output::Progress;
use crate::selector;
use crate::state::{local_base_path, AgentState};
use crate::vault_key::{self, VaultKeyArgs};
use crate::vault_migrate;

use alf_core::archive::AlfReader;
use alf_core::rebuild::rebuild_snapshot;
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
    /// `true` when invoked with `--at-sequence N`. Indicates the local sync
    /// state (`~/.alf/state/`) was deliberately not touched.
    preview: bool,
    /// Echoes the `--at-sequence N` flag; `None` for a head restore.
    #[serde(skip_serializing_if = "Option::is_none")]
    at_sequence: Option<u64>,
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
            would_write: Some(d.would_write),
            warnings: d.warnings,
        }
    }
}

/// Result of a successful [`pull_cloud_base`] call.
pub(crate) struct CloudBase {
    /// The merged archive bytes (snapshot + applied deltas).
    pub final_bytes: Vec<u8>,
    /// The cloud sequence after applying any deltas.
    pub latest_sequence: u64,
    /// Path the merged archive was written to (`~/.alf/state/{id}-snapshot.alf`).
    pub local_base: PathBuf,
}

/// Merge a base snapshot with zero or more delta archives (same semantics as cloud restore).
pub(crate) fn merge_snapshot_with_deltas(
    snapshot_bytes: &[u8],
    delta_byte_vecs: &[Vec<u8>],
) -> Result<Vec<u8>> {
    let delta_refs: Vec<&[u8]> = delta_byte_vecs.iter().map(|v| v.as_slice()).collect();
    rebuild_snapshot(snapshot_bytes, &delta_refs).map_err(|e| anyhow::anyhow!(e))
}

fn merged_last_sequence(merged_bytes: &[u8], snapshot_sequence: u64) -> Result<u64> {
    let reader = AlfReader::new(Cursor::new(merged_bytes))
        .context("Failed to read merged restore archive")?;
    Ok(reader
        .manifest()
        .sync
        .as_ref()
        .map(|s| s.last_sequence)
        .unwrap_or(snapshot_sequence))
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

    let final_bytes = merge_snapshot_with_deltas(&snapshot_bytes, &delta_byte_vecs)
        .context("Failed to merge snapshot and deltas for restore")?;

    let latest_sequence = merged_last_sequence(&final_bytes, snapshot_sequence)?;
    Ok((final_bytes, latest_sequence))
}

/// Download the cloud snapshot + deltas (head) for `agent_id`, merge them, and
/// write the result to `~/.alf/state/{agent_id}-snapshot.alf`. Then update the
/// state file with `Some(latest_sequence)` and a fresh `last_synced_at`.
///
/// **Does not touch the workspace.** Use [`run`] when the workspace itself
/// needs to be rebuilt; reuse this helper from `alf sync --recover` to repair
/// a missing local base without disturbing live workspace state.
///
/// # Atomic-write invariant
///
/// `base.alf` is written **before** `state.toml`. This guarantees that
/// state.toml-present ⇒ base.alf-present at the moment of the last successful
/// write. See [`docs/how_alf_syncs.md`].
pub(crate) fn pull_cloud_base(
    client: &ApiClient,
    agent_id: Uuid,
    progress: Progress,
) -> Result<CloudBase> {
    let (final_bytes, latest_sequence) = fetch_restore_payload(client, agent_id, None, progress)?;

    let local_base = local_base_path(agent_id)?;
    if let Some(parent) = local_base.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create state directory {}", parent.display()))?;
    }
    fs::write(&local_base, &final_bytes).with_context(|| {
        format!(
            "Failed to write restored snapshot base at {}",
            local_base.display()
        )
    })?;

    let state = AgentState {
        agent_id,
        last_synced_sequence: Some(latest_sequence),
        last_synced_at: Some(Utc::now()),
    };
    state.save()?;

    Ok(CloudBase {
        final_bytes,
        latest_sequence,
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
    progress: Progress,
) -> Result<(ImportReport, u64)> {
    // Branch on at_sequence:
    //   None    → head restore: pull_cloud_base writes base.alf + state.toml.
    //   Some(n) → PIT preview: fetch only, leave ~/.alf/state untouched.
    let (final_bytes, latest_sequence) = match at_sequence {
        None => {
            let base = pull_cloud_base(client, agent_id, progress)?;
            (base.final_bytes, base.latest_sequence)
        }
        Some(n) => fetch_point_in_time(client, agent_id, n, progress)?,
    };

    // Import the merged archive into the workspace.
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    let temp_alf = temp_dir.path().join("restored.alf");
    fs::write(&temp_alf, &final_bytes)?;

    progress.emit("  Importing into workspace...");
    let resolved_key = vault_key::resolve(key_args, runtime, Some(agent_id))?;
    if let Some((_, source)) = &resolved_key {
        progress.emit(&format!(
            "Using vault key from {} — credentials will be decrypted and restored",
            source.label()
        ));
    }
    let import_options = ImportOptions {
        vault_key: resolved_key.as_ref().map(|(k, _)| k),
        mode,
    };
    let import_report = adapt.import_with_options(&temp_alf, workspace, import_options)?;
    Ok((import_report, latest_sequence))
}

/// Assemble the JSON `RestoreResult` from a completed restore. Shared by the CLI
/// JSON branch and the MCP `alf_restore` tool.
fn build_restore_result(
    agent_id: Uuid,
    runtime: &str,
    workspace: &Path,
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
        workspace: workspace.to_string_lossy().into(),
        preview: at_sequence.is_some(),
        at_sequence,
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
    let target = resolve_target(runtime, workspace_flag, agent)?;

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

    // WP1: move any legacy vault/key to the per-agent layout before the adapter
    // writes Layer 4 (skipped for a dry-run, which writes nothing).
    vault_migrate::require_migrated(&target.config, runtime)?;

    let (report, latest_sequence) = perform_restore(
        &target.client,
        target.agent_id,
        &target.workspace,
        runtime,
        target.adapt.as_ref(),
        at_sequence,
        mode,
        key_args,
        progress,
    )?;
    Ok(build_restore_result(
        target.agent_id,
        runtime,
        &target.workspace,
        &report,
        latest_sequence,
        at_sequence,
    )
    .into())
}

pub fn run(
    runtime: &str,
    workspace_flag: Option<&Path>,
    agent_arg: Option<&str>,
    at_sequence: Option<u64>,
    dry_run: bool,
    mode: RestoreMode,
    key_args: &VaultKeyArgs,
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

    // WP1: move any legacy vault/key to the per-agent layout before the
    // adapter writes Layer 4 — otherwise the key leg is missed on the first
    // post-upgrade restore and the legacy file survives as a shadow vault.
    // (After the dry-run gate: --dry-run writes nothing.)
    vault_migrate::require_migrated(&target.config, runtime)?;

    if human {
        if let Some(n) = at_sequence {
            println!(
                "{} Preview: restoring agent {} at sequence {} into {} workspace...",
                "▸".blue().bold(),
                &agent_id.to_string()[..8],
                n,
                adapt.name()
            );
            println!(
                "  {}",
                "Read-only preview — ~/.alf/state will not be touched.".yellow()
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

    let (import_report, latest_sequence) = perform_restore(
        &target.client,
        agent_id,
        workspace,
        runtime,
        adapt,
        at_sequence,
        mode,
        key_args,
        Progress::stderr(),
    )?;

    if human {
        println!();
        if preview {
            println!(
                "{} Preview restore complete (state untouched)",
                "✓".green().bold()
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
        println!("  Workspace: {}", workspace.display());

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
            workspace,
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

#[cfg(test)]
mod tests {
    use super::*;

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
        DeltaSyncCursor, LayerInventory, Manifest,
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
}
