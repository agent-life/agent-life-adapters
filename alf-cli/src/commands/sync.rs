//! `alf sync` — incremental sync to the cloud.
//!
//! The sole sync-control variable is `state.last_synced_sequence: Option<u64>`.
//! Together with whether `~/.alf/state/{id}-snapshot.alf` exists on disk, it
//! decides which path the sync takes. See [`docs/how_alf_syncs.md`] for the
//! full model and `decide_sync_mode` for the pure decision function.
//!
//! Flow:
//! 1. Load config, check API key
//! 2. Resolve adapter, export workspace to a temp .alf
//! 3. Read the manifest to get the agent ID
//! 4. Load `~/.alf/state/{id}.toml` and check for `{id}-snapshot.alf`
//! 5. [`decide_sync_mode`] picks one of: FirstSync / Delta / BailMissingBase / Recover
//! 6. Execute the chosen mode, persisting (in this order) base.alf, then state.toml.
//!
//! Atomic-write invariant: base.alf is always written **before** state.toml,
//! both in the first-sync and delta paths.

use crate::adapter;
use crate::api_client::{ApiClient, RegisterAgentOutcome};
use crate::commands::restore::{ensure_sync_not_during_restore, pull_cloud_base};
use crate::config::Config;
use crate::errors::{codes, CliError};
use crate::output;
use crate::selector::{self, SelectedAgent, SelectorSource};
use crate::state::{local_base_exists, local_base_path, state_file_path, AgentState};

use alf_core::archive::{AlfReader, DeltaWriter};
use alf_core::delta::{compute_delta, diff_credentials, diff_principals, identity_changed};
use alf_core::manifest::{ChangeInventory, DeltaAgentRef, DeltaManifest, DeltaSyncCursor};
use alf_core::{CredentialsDocument, PrincipalsDocument};

use crate::output::Progress;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use colored::Colorize;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// The single-agent `alf sync` JSON result. Printed by the CLI and returned as
/// the MCP `alf_sync` tool's structured content (built via [`build_sync_result`]).
#[derive(Serialize, JsonSchema)]
pub(crate) struct SyncResult {
    ok: bool,
    sequence: u64,
    delta: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    changes: Option<SyncChanges>,
    snapshot_path: String,
    no_changes: bool,
    /// True iff this sync invocation pulled the cloud base because the local
    /// base was missing (i.e. went through the `--recover` path).
    recovered: bool,
    /// The agent this sync operated on and how it was selected (WP0).
    agent: SyncAgentRef,
}

#[derive(Serialize, JsonSchema)]
struct SyncAgentRef {
    runtime_agent: String,
    alf_agent_id: Uuid,
    source: SelectorSource,
}

/// One JSON object for `alf sync --all`: per-agent results, never fail-fast.
#[derive(Serialize)]
struct SyncAllResult {
    ok: bool,
    all: bool,
    results: Vec<SyncAllEntry>,
}

#[derive(Serialize)]
struct SyncAllEntry {
    runtime_agent: String,
    alf_agent_id: Uuid,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    sequence: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    no_changes: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
}

/// Result of one agent's sync, decoupled from output so `--all` can collect
/// several and the single-agent path emits exactly one JSON object. `pub(crate)`
/// so the MCP `alf_sync` tool can drive [`run_one_agent`] and render the outcome.
pub(crate) struct SyncOutcome {
    sequence: u64,
    delta: bool,
    changes: Option<SyncChanges>,
    no_changes: bool,
    recovered: bool,
    /// Full snapshot forced on the delta path (tracked files changed).
    resnapshot: bool,
    snapshot_path: PathBuf,
}

#[derive(Serialize, JsonSchema)]
struct SyncChanges {
    creates: usize,
    updates: usize,
    deletes: usize,
    /// Layer 4 (credentials) changes carried by this delta. Omitted when the
    /// vault was unchanged.
    #[serde(default, skip_serializing_if = "LayerChanges::is_zero")]
    credentials: LayerChanges,
    /// Layer 2 (principals) changes carried by this delta. Omitted when
    /// unchanged.
    #[serde(default, skip_serializing_if = "LayerChanges::is_zero")]
    principals: LayerChanges,
    /// Whether Layer 1 (identity) changed in this delta. Omitted when false.
    #[serde(default, skip_serializing_if = "is_false")]
    identity: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Serialize, JsonSchema, Default)]
struct LayerChanges {
    creates: usize,
    updates: usize,
    deletes: usize,
}

impl LayerChanges {
    fn is_zero(&self) -> bool {
        self.creates == 0 && self.updates == 0 && self.deletes == 0
    }
}

/// Pure decision function for `alf sync` branching.
///
/// Inputs are the only two pieces of state that gate behaviour:
/// - `state.last_synced_sequence` — `None` ⇒ never synced; `Some(N)` ⇒ synced at sequence N.
/// - `base_present` — whether `~/.alf/state/{id}-snapshot.alf` exists on disk.
/// - `recover` — whether `--recover` was passed on the CLI.
///
/// `last_synced_at` is **deliberately not** an input. It is informational
/// metadata; branching on it has historically caused ambiguity (see commit
/// log around the E4 failure) and is forbidden by the sync-control invariant.
pub(crate) fn decide_sync_mode(state: &AgentState, base_present: bool, recover: bool) -> SyncMode {
    match (state.last_synced_sequence, base_present, recover) {
        (None, _, _) => SyncMode::FirstSync,
        // `--recover` wins even when a local base is present: it re-pulls the
        // cloud-reconstructed base and re-derives the delta against cloud truth.
        // This is the unattended self-heal for a diverged/poisoned local base
        // (case E9) — no operator `rm base` step needed. Non-destructive: the
        // workspace is untouched and the base is only overwritten after a
        // successful cloud fetch.
        (Some(n), _, true) => SyncMode::Recover { base_sequence: n },
        (Some(n), true, false) => SyncMode::Delta { base_sequence: n },
        (Some(n), false, false) => SyncMode::BailMissingBase {
            last_synced_sequence: n,
        },
    }
}

/// What a first sync should do when the cloud already knows this agent id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FirstSyncConflict {
    /// No conflict (or the operator forced it, or nothing is in the cloud to
    /// overwrite): upload the snapshot as planned.
    Upload,
    /// **Our own** first sync was interrupted after its upload landed (MIN-3):
    /// adopt the cloud snapshot as the local base and land the current
    /// workspace on top of it — the same ladder E7 recovery uses.
    AdoptCloudBase,
    /// A genuine fork (E3): the cloud has history this machine never wrote.
    /// Park and ask a human.
    Park,
}

/// Decide the first-sync branch. Pure so the matrix is unit-testable
/// (mirrors [`decide_sync_mode`]).
///
/// The discriminator for MIN-3 is the **in-flight marker**: `alf` writes
/// `~/.alf/state/{id}.first-sync-inflight` immediately before a first-sync
/// upload and removes it once the state file lands. Its presence is local,
/// per-agent proof that THIS machine was mid-first-sync for THIS id — a fork
/// created on another machine leaves no such marker. Without that evidence a
/// "the agent exists in the cloud" conflict stays a park, exactly as before.
///
/// `cloud_latest_sequence == 0` with a marker means the register call landed
/// but the upload did not: there is no cloud history to overwrite, so the
/// upload simply proceeds.
pub(crate) fn decide_first_sync_conflict(
    already_existed: bool,
    cloud_latest_sequence: u64,
    inflight_marker: bool,
    force_first_sync: bool,
) -> FirstSyncConflict {
    if !already_existed || force_first_sync {
        return FirstSyncConflict::Upload;
    }
    match (inflight_marker, cloud_latest_sequence) {
        (true, 0) => FirstSyncConflict::Upload,
        (true, _) => FirstSyncConflict::AdoptCloudBase,
        (false, _) => FirstSyncConflict::Park,
    }
}

/// The in-flight marker path for `agent_id` (see [`decide_first_sync_conflict`]).
fn first_sync_marker_path(agent_id: uuid::Uuid) -> Result<std::path::PathBuf> {
    Ok(AgentState::state_dir()?.join(format!("{agent_id}.first-sync-inflight")))
}

/// Record that a first-sync upload is about to start. Best-effort by design:
/// if the marker cannot be written the sync still proceeds — the only cost is
/// that a crash in the upload window falls back to the pre-MIN-3 park.
fn mark_first_sync_inflight(agent_id: uuid::Uuid) {
    if let Ok(path) = first_sync_marker_path(agent_id) {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&path, "in-flight first sync; see sync.rs MIN-3\n");
    }
}

/// Clear the in-flight marker once the sync state is durable.
fn clear_first_sync_inflight(agent_id: uuid::Uuid) {
    if let Ok(path) = first_sync_marker_path(agent_id) {
        let _ = fs::remove_file(path);
    }
}

fn first_sync_marker_present(agent_id: uuid::Uuid) -> bool {
    first_sync_marker_path(agent_id).is_ok_and(|p| p.exists())
}

/// E3 guard: refuse to upload an empty/local-only workspace as a "first sync"
/// when an agent with this ID already exists in the cloud, unless the operator
/// explicitly opts in via `--force-first-sync`. See `docs/how_alf_syncs.md`.
pub(crate) fn check_first_sync_safety(
    agent_id: uuid::Uuid,
    runtime: &str,
    outcome: &RegisterAgentOutcome,
    force_first_sync: bool,
) -> Result<()> {
    if outcome.already_existed && !force_first_sync {
        bail!(
            "Agent {} already exists in the cloud (latest_sequence = {}), \
             but no local sync state was found at ~/.alf/state/. \
             Refusing to upload as first sync to avoid overwriting cloud history. \
             Either run `alf restore -r {} -w <workspace> --agent {}` first to hydrate state, \
             or pass --force-first-sync to overwrite the cloud agent with the current workspace. \
             See docs/how_alf_syncs.md (case E3).",
            agent_id,
            outcome.info.latest_sequence,
            runtime,
            agent_id
        );
    }
    Ok(())
}

/// Persist a successful sync to disk: copy the freshly-exported archive over
/// the local base, then save the state file.
///
/// **Atomic-write invariant.** `base.alf` is written **before** `state.toml`.
/// This guarantees that, at the moment of the last successful write,
/// `state.toml` exists ⇒ `base.alf` exists. A future `alf sync` invocation
/// reading these two files will therefore never see the orphan-state-file
/// state described as case E4 in `docs/how_alf_syncs.md`.
fn persist_local(
    agent_id: uuid::Uuid,
    sequence: u64,
    temp_alf: &Path,
    snapshot_path: &Path,
) -> Result<()> {
    // Atomic base write (WP-M3 review B1): copy into a sibling temp, fsync, then
    // rename over the base. A SIGKILL mid-write (design §5.3 treats it as normal,
    // and the watch loop makes autonomous syncs reachable) then leaves either the
    // old base or the new base — never a truncated `base.alf` that the next start
    // would read as a corrupt/short archive.
    if let Some(parent) = snapshot_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }
    let tmp = snapshot_path.with_extension("alf.tmp");
    fs::copy(temp_alf, &tmp)
        .with_context(|| format!("Failed to stage snapshot at {}", tmp.display()))?;
    {
        // fsync the staged file so its bytes are durable before the rename.
        let f = fs::File::open(&tmp)
            .with_context(|| format!("Failed to reopen staged snapshot {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("Failed to fsync staged snapshot {}", tmp.display()))?;
    }
    fs::rename(&tmp, snapshot_path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        anyhow::anyhow!(
            "Failed to persist snapshot at {}: {e}",
            snapshot_path.display()
        )
    })?;

    // State is written AFTER the base (invariant: state ⇒ base), atomically.
    let new_state = AgentState {
        agent_id,
        last_synced_sequence: Some(sequence),
        last_synced_at: Some(Utc::now()),
    };
    new_state.save()?;
    Ok(())
}

/// Outcome of [`decide_sync_mode`]. Each variant maps to a row of the branch
/// table in [`docs/how_alf_syncs.md`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SyncMode {
    /// First sync ever for this agent (`last_synced_sequence: None`).
    /// Register the agent and upload a full snapshot at sequence 0.
    FirstSync,
    /// Subsequent sync. Compute a delta against `base_sequence` and push.
    Delta { base_sequence: u64 },
    /// State says we have synced before but the local base is missing and
    /// `--recover` was not passed. Bail with an actionable error pointing to
    /// `alf sync --recover`.
    BailMissingBase { last_synced_sequence: u64 },
    /// State says we have synced before and `--recover` was passed (whether or
    /// not a local base exists). Pull the cloud-reconstructed base — overwriting
    /// any stale/diverged local base — then take the delta path at
    /// `base_sequence`. This is the unattended self-heal for case E9.
    Recover { base_sequence: u64 },
}

pub fn run(
    runtime: &str,
    workspace_flag: Option<&Path>,
    agent: Option<&str>,
    all: bool,
    recover: bool,
    force_first_sync: bool,
) -> Result<()> {
    let human = output::human_mode();

    // clap's conflicts_with cannot see the global --agent when it is matched
    // before the subcommand (`alf --agent x sync --all`), so enforce the
    // conflict here too — --agent must never be silently ignored.
    if all && agent.is_some() {
        bail!(
            "--all cannot be combined with --agent: --all syncs every enabled agent. \
             Drop --all to sync only the selected agent, or drop --agent."
        );
    }

    if all {
        let mut config = Config::load()?;
        let adapt = adapter::get_adapter(runtime).ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown runtime '{}'. Supported: {}",
                runtime,
                adapter::supported_runtimes()
            )
        })?;
        // Install root for discovery/lazy-init: -w flag → [defaults].workspace →
        // the runtime's own configured/default location (same order alf check
        // uses). Generic requires an explicit workspace — fail here rather than
        // let an empty path fall through to a stray CWD `.alf-agent-id` write (R1).
        let install =
            crate::commands::check::resolve_workspace_required(workspace_flag, &config, runtime)?;
        return run_all(
            &mut config,
            adapt.as_ref(),
            runtime,
            &install,
            recover,
            force_first_sync,
            human,
        );
    }

    // L3 (MAJ-6): hold the per-agent advisory lock across the whole sync — the
    // same lock the MCP server and the watch loop take — so a CLI sync cannot
    // interleave with a watch export/restore on this agent. (The lock lives
    // OUTSIDE `run_one_agent`: the MCP tool and the watch loop already hold it
    // when they enter that seam, and flock does not nest.) Cross-machine races
    // remain the service sequence CAS's job (E7).
    let _agent_lock = crate::commands::mcp::acquire_agent_lock(runtime, workspace_flag, agent)?;

    // CLI single-agent path: progress to stderr (unchanged), render the outcome.
    let (outcome, selected) = run_one_agent(
        runtime,
        workspace_flag,
        agent,
        recover,
        force_first_sync,
        human,
        Progress::stderr(),
    )?;

    if human {
        print_human_outcome(&outcome, selected.alf_agent_id)?;
    } else {
        output::json(&build_sync_result(outcome, &selected));
    }

    Ok(())
}

/// Sync exactly one selected agent, end to end: load config, resolve the
/// adapter + workspace, run the selector (+ enabled gate), migrate the vault,
/// build the API client, and drive [`sync_one`]. No stdout output — the caller
/// renders. Extracted as the MCP `alf_sync` seam so the tool reuses the whole
/// single-agent pipeline (never the printing `run`). `progress` routes the
/// interleaved status lines: [`Progress::stderr`] for the CLI, a callback for
/// the MCP progress-notification bridge.
pub(crate) fn run_one_agent(
    runtime: &str,
    workspace_flag: Option<&Path>,
    agent: Option<&str>,
    recover: bool,
    force_first_sync: bool,
    human: bool,
    progress: Progress,
) -> Result<(SyncOutcome, SelectedAgent)> {
    let mut config = Config::load()?;

    let adapt = adapter::get_adapter(runtime).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown runtime '{}'. Supported: {}",
            runtime,
            adapter::supported_runtimes()
        )
    })?;

    // Install root for discovery/lazy-init: -w flag → [defaults].workspace →
    // the runtime's own configured/default location (same order alf check uses).
    // Generic requires an explicit workspace — fail here rather than let an
    // empty path fall through to a stray CWD `.alf-agent-id` write (R1).
    let install = crate::commands::check::resolve_workspace_or_mapped(
        workspace_flag,
        &config,
        runtime,
        agent,
    )?;

    // Selection (and its enabled gate) runs BEFORE ApiClient::from_config so
    // selection errors are observable without an API key.
    let selected =
        selector::select_current_agent(&mut config, adapt.as_ref(), runtime, &install, agent)?;
    selector::require_enabled_for_sync(&selected)?;

    // Check before vault migration too: an interrupted restore must not let a
    // manual/MCP sync perform unrelated mutation before reporting its park.
    ensure_sync_not_during_restore(selected.alf_agent_id, runtime, &selected.workspace)?;

    // WP1: move any legacy vault/key to the per-agent layout before export —
    // adapters read only per-agent vault paths (no legacy fallback), so an
    // unmigrated vault would silently drop Layer 4 from the upload.
    crate::vault_migrate::require_migrated(&config, &selected.runtime)?;

    let client = ApiClient::from_config(&config)?;
    let outcome = sync_one(
        &client,
        adapt.as_ref(),
        runtime,
        &selected,
        workspace_flag,
        recover,
        force_first_sync,
        human,
        progress,
    )?;

    Ok((outcome, selected))
}

/// Assemble the single-agent `SyncResult` JSON from a completed outcome and its
/// selected agent. Shared by the CLI's `run` JSON branch and the MCP `alf_sync`
/// tool so the two emit the identical structure.
pub(crate) fn build_sync_result(outcome: SyncOutcome, selected: &SelectedAgent) -> SyncResult {
    SyncResult {
        ok: true,
        sequence: outcome.sequence,
        delta: outcome.delta,
        changes: outcome.changes,
        snapshot_path: outcome.snapshot_path.to_string_lossy().into(),
        no_changes: outcome.no_changes,
        recovered: outcome.recovered,
        agent: SyncAgentRef {
            runtime_agent: selected.alias.clone(),
            alf_agent_id: selected.alf_agent_id,
            source: selected.source,
        },
    }
}

/// `alf sync --all`: sync every enabled agent sequentially, collecting
/// per-agent results — one agent's failure must not block the others'
/// backups. Emits ONE JSON object and exits 1 itself when any agent failed
/// (avoids a second JSON object from main's error path).
#[allow(clippy::too_many_arguments)]
fn run_all(
    config: &mut Config,
    adapt: &dyn adapter::Adapter,
    runtime: &str,
    install: &Path,
    recover: bool,
    force_first_sync: bool,
    human: bool,
) -> Result<()> {
    let selected = selector::select_all_enabled(config, adapt, runtime, install)?;

    // WP1: one migration pass for the runtime before any agent exports.
    crate::vault_migrate::require_migrated(config, runtime)?;

    let client = ApiClient::from_config(config)?;

    let mut results = Vec::with_capacity(selected.len());
    for sel in &selected {
        output::progress(&format!(
            "Syncing agent '{}' ({})...",
            sel.alias, sel.alf_agent_id
        ));
        // L3 (MAJ-6): per-agent lock held across this agent's sync (`_lock`
        // lives for the closure). A busy agent becomes its `agent_busy` error
        // row below while the rest of --all proceeds.
        match crate::commands::mcp::watch::acquire_agent_lock_timeout(
            sel.alf_agent_id,
            std::time::Duration::from_secs(10),
        )
        .and_then(|_lock| {
            sync_one(
                &client,
                adapt,
                runtime,
                sel,
                None,
                recover,
                force_first_sync,
                /* human: */ false,
                Progress::stderr(),
            )
        }) {
            Ok(outcome) => results.push(SyncAllEntry {
                runtime_agent: sel.alias.clone(),
                alf_agent_id: sel.alf_agent_id,
                ok: true,
                sequence: Some(outcome.sequence),
                no_changes: outcome.no_changes.then_some(true),
                error: None,
                code: None,
                hint: None,
            }),
            Err(err) => {
                let (code, hint) = match err.downcast_ref::<CliError>() {
                    Some(c) => (Some(c.code.to_string()), Some(c.remedy.clone())),
                    None => {
                        let h = output::error_hint(&err);
                        (None, (!h.is_empty()).then_some(h))
                    }
                };
                results.push(SyncAllEntry {
                    runtime_agent: sel.alias.clone(),
                    alf_agent_id: sel.alf_agent_id,
                    ok: false,
                    sequence: None,
                    no_changes: None,
                    error: Some(format!("{err:#}")),
                    code,
                    hint,
                });
            }
        }
    }

    let all_ok = results.iter().all(|r| r.ok);
    if human {
        for r in &results {
            if r.ok {
                println!(
                    "{} {}  sequence={}{}",
                    "✓".green().bold(),
                    r.runtime_agent,
                    r.sequence.unwrap_or(0),
                    if r.no_changes == Some(true) {
                        "  (no changes)"
                    } else {
                        ""
                    }
                );
            } else {
                println!(
                    "{} {}  {}",
                    "✗".red().bold(),
                    r.runtime_agent,
                    r.error.as_deref().unwrap_or("failed")
                );
            }
        }
    } else {
        output::json(&SyncAllResult {
            ok: all_ok,
            all: true,
            results,
        });
    }
    if !all_ok {
        std::process::exit(1);
    }
    Ok(())
}

/// Sync one selected agent: export via the agent-aware seam, decide the mode,
/// execute it. No stdout output — callers render the outcome. `progress`
/// receives the interleaved status lines ([`Progress::stderr`] for the CLI, a
/// callback for the MCP bridge); the human-mode `println!` path is CLI-only and
/// never reached by the MCP server (which always passes `human: false`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn sync_one(
    client: &ApiClient,
    adapt: &dyn adapter::Adapter,
    runtime: &str,
    selected: &SelectedAgent,
    workspace_flag: Option<&Path>,
    recover: bool,
    force_first_sync: bool,
    human: bool,
    progress: Progress,
) -> Result<SyncOutcome> {
    // Fail closed on identity drift BEFORE any network traffic: the workspace
    // and the mapping must agree on who this agent is.
    check_identity_drift(selected)?;

    let (workspace, adhoc) = selector::effective_workspace(selected, workspace_flag);

    // A mapped per-agent workspace is adapter-owned and may not exist yet
    // A head restore can leave the workspace partially imported while its
    // old base/cursor remain on disk. Never prune, export, recover, or upload
    // until that workspace-bound transaction has been completed.
    ensure_sync_not_during_restore(selected.alf_agent_id, runtime, &workspace)?;

    // (`export_agent` creates it); only validate an explicit -w target. Coded
    // (`workspace_missing`) so the watch loop parks instead of retrying forever.
    if adhoc && !workspace.exists() {
        return Err(CliError {
            code: codes::WORKSPACE_MISSING,
            cause: format!(
                "Workspace directory does not exist: {}",
                workspace.display()
            ),
            remedy: "restore the workspace or fix the -w path, then re-run alf sync".to_string(),
        }
        .into());
    }

    if human {
        println!(
            "{} Syncing {} workspace...",
            "▸".blue().bold(),
            adapt.name()
        );
        println!("  Workspace: {}", workspace.display());
        println!();
    } else {
        progress.emit(&format!("Syncing {} workspace...", adapt.name()));
        progress.emit(&format!("  Workspace: {}", workspace.display()));
    }

    // WP3: prune tracked files (added via `alf add`) that the agent has since
    // deleted, recording each removal in `.alf-sync-log.md` — BEFORE export so
    // the cleaned include list and the log are captured in this sync. The
    // include list is a runtime-agnostic workspace convention (alf_core), so
    // this applies to every runtime whose adapter packs the tracked files.
    // The RMW is serialized against alf_track/alf add (innermost lock, §6);
    // the guard drops before export so nothing nests inside it.
    let removed = {
        let _include_lock = crate::commands::add::lock_include_list(&workspace)?;
        alf_core::prune_and_log_missing(&workspace)?
    };
    for rel in &removed {
        progress.emit(&format!(
            "  Removed {rel} from sync (file no longer present; logged to {})",
            alf_core::SYNC_LOG_FILE
        ));
    }

    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    let temp_alf = temp_dir.path().join("snapshot.alf");

    progress.emit("  Exporting workspace...");
    // Export through the agent-aware seam: the mapping's id is written through
    // to the workspace, so manifest.agent.id == selected.alf_agent_id.
    let mut binding = selected.binding.clone();
    binding.workspace = workspace.clone();
    let report = adapt.export_agent(&binding, selected.alf_agent_id, &temp_alf)?;
    progress.emit(&format!(
        "  Exported {} memory records",
        report.memory_records
    ));
    // Surface adapter advisories (e.g. Hermes's un-vaulted `.env` notice, D4).
    for w in &report.warnings {
        progress.emit(&format!("  ! {w}"));
    }

    let alf_bytes = fs::read(&temp_alf).context("Failed to read temp .alf file")?;
    let reader = AlfReader::new(Cursor::new(&alf_bytes))?;
    // Contract assertion (the WP0 seam): the archive identity must be the
    // selected agent — everything downstream (state, register, upload) keys
    // off selected.alf_agent_id.
    let manifest_id = reader.manifest().agent.id;
    if manifest_id != selected.alf_agent_id {
        bail!(
            "Export produced agent id {} but the selected agent is {} — the \
             workspace and the [[agents]] mapping disagree. Run `alf check` to reconcile.",
            manifest_id,
            selected.alf_agent_id
        );
    }
    let agent_id = selected.alf_agent_id;

    // Decide the sync mode strictly from (sequence, base_present, recover).
    let state = AgentState::load(agent_id)?;
    let base_present = local_base_exists(agent_id)?;
    let mode = decide_sync_mode(&state, base_present, recover);

    let snapshot_path = local_base_path(agent_id)?;
    if let Some(parent) = snapshot_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create state directory {}", parent.display()))?;
    }

    match mode {
        SyncMode::FirstSync => execute_first_sync(
            client,
            agent_id,
            runtime,
            &report.agent_name,
            &alf_bytes,
            &temp_alf,
            &snapshot_path,
            force_first_sync,
            progress,
        ),
        SyncMode::Delta { base_sequence } => execute_delta(
            client,
            agent_id,
            runtime,
            base_sequence,
            state.last_synced_at,
            &alf_bytes,
            &temp_alf,
            &snapshot_path,
            /* recovered: */ false,
            progress,
        ),
        SyncMode::Recover { base_sequence } => {
            progress.emit(&format!(
                "  Local base missing — recovering from cloud (base sequence {base_sequence})..."
            ));
            // pull_cloud_base writes base.alf and state.toml under ~/.alf/state/.
            let cloud = pull_cloud_base(client, agent_id, progress)?;
            progress.emit(&format!(
                "  Recovered local base at sequence {} ({})",
                cloud.latest_sequence,
                cloud.local_base.display()
            ));
            execute_delta(
                client,
                agent_id,
                runtime,
                cloud.latest_sequence,
                Some(Utc::now()),
                &alf_bytes,
                &temp_alf,
                &snapshot_path,
                /* recovered: */ true,
                progress,
            )
        }
        SyncMode::BailMissingBase {
            last_synced_sequence,
        } => bail!(
            "Local delta base missing at {} (state says last synced at sequence {}). \
             Run `alf sync --recover -r {} -w {}` to pull the cloud snapshot and rebuild the base. \
             See docs/how_alf_syncs.md (case E4) for details.",
            snapshot_path.display(),
            last_synced_sequence,
            runtime,
            workspace.display()
        ),
    }
}

/// Fail-closed pre-network drift guard: a workspace `.alf-agent-id` that
/// disagrees with the mapping means syncing would upload one agent's data
/// under another's identity. Coded so the agent LLM can heal it.
fn check_identity_drift(selected: &SelectedAgent) -> Result<()> {
    let id_file = selected.workspace.join(alf_core::AGENT_ID_FILE);
    if !id_file.is_file() {
        return Ok(());
    }
    let raw = fs::read_to_string(&id_file)
        .with_context(|| format!("Failed to read {}", id_file.display()))?;
    if let Ok(ws_id) = Uuid::parse_str(raw.trim()) {
        if ws_id != selected.alf_agent_id {
            return Err(CliError {
                code: codes::AGENT_ID_DRIFT,
                cause: format!(
                    "Agent identity drift: {} contains {} but the mapping for '{}' expects {}.",
                    id_file.display(),
                    ws_id,
                    selected.alias,
                    selected.alf_agent_id
                ),
                remedy: format!(
                    "Run: echo {} > {} to keep the mapped history, or run 'alf check' \
                     after intentional identity changes.",
                    selected.alf_agent_id,
                    id_file.display()
                ),
            }
            .into());
        }
    }
    Ok(())
}

/// Human rendering for a single-agent sync outcome (labels match the pre-WP0
/// output).
fn print_human_outcome(outcome: &SyncOutcome, agent_id: Uuid) -> Result<()> {
    if outcome.no_changes {
        println!(
            "{} No changes detected — already up to date",
            "✓".green().bold()
        );
        return Ok(());
    }
    let label = match (outcome.delta, outcome.resnapshot, outcome.recovered) {
        (true, _, false) => "Delta uploaded",
        (true, _, true) => "Delta uploaded (recovered)",
        (false, true, false) => "Re-snapshot uploaded (tracked files changed)",
        (false, true, true) => "Re-snapshot uploaded (recovered; tracked files changed)",
        (false, false, _) => "Snapshot uploaded",
    };
    let state_path = state_file_path(agent_id)?;
    println!(
        "{} {} (sequence: {})",
        "✓".green().bold(),
        label,
        outcome.sequence
    );
    println!("  Snapshot base: {}", outcome.snapshot_path.display());
    println!("  State file:    {}", state_path.display());
    Ok(())
}

/// True when an error string carries an HTTP auth rejection. Shared by the
/// registration/upload wrappers so 401/403 classify as `auth_failed`
/// (manual §4.2: the watch loop parks on auth instead of retrying forever).
fn is_auth_rejection(cause: &str) -> bool {
    let lc = cause.to_lowercase();
    lc.contains("http 401") || lc.contains("http 403") || lc.contains("authentication failed")
}

/// Wrap a registration failure as a coded, machine-distinguishable error.
/// HTTP 402 (subscription/agent limit) and HTTP 401/403 (bad key) get dedicated
/// codes — both are permanent classes the watch loop must park on, not retry.
fn wrap_registration(err: anyhow::Error) -> anyhow::Error {
    let cause = format!("{err:#}");
    if cause.contains("HTTP 402") {
        return CliError {
            code: codes::SUBSCRIPTION_DENIED,
            cause,
            remedy: "The service refused registration (subscription/agent limit — see the \
                 server message in the error). Upgrade the subscription or `alf purge` an \
                 unused agent, then re-run alf sync."
                .to_string(),
        }
        .into();
    }
    if is_auth_rejection(&cause) {
        return CliError {
            code: codes::AUTH_FAILED,
            cause,
            remedy: "the service rejected this API key; fix it (alf login, or \
                 service.api_key in ~/.alf/config.toml), then re-run alf sync"
                .to_string(),
        }
        .into();
    }
    CliError {
        code: codes::REGISTRATION_FAILED,
        cause,
        remedy: "check your API key (alf login) and network, then re-run alf sync; \
             registration is the one-time backend step before first upload — nothing \
             was uploaded"
            .to_string(),
    }
    .into()
}

/// Wrap a snapshot/delta upload failure. The local base was not advanced, so
/// a re-run retries the same upload — except on a sequence conflict, where a
/// plain retry pushes the same stale base and fails identically: the remedy
/// must point at restore (how_alf_syncs.md E7), not a retry loop.
fn wrap_upload(err: anyhow::Error) -> anyhow::Error {
    let cause = format!("{err:#}");
    if cause.contains("Sequence conflict") {
        return CliError {
            code: codes::SYNC_UPLOAD_FAILED,
            cause,
            remedy: "another host advanced this agent's cloud state; run the 'alf restore' \
                 command shown in the error to pull the latest state, then re-run alf sync \
                 — a plain retry hits the same conflict"
                .to_string(),
        }
        .into();
    }
    if is_auth_rejection(&cause) {
        return CliError {
            code: codes::AUTH_FAILED,
            cause,
            remedy: "the service rejected this API key; fix it (alf login, or \
                 service.api_key in ~/.alf/config.toml), then re-run alf sync"
                .to_string(),
        }
        .into();
    }
    CliError {
        code: codes::SYNC_UPLOAD_FAILED,
        cause,
        remedy: "check network connectivity and re-run alf sync; the local delta base \
             was not advanced, so the next sync retries this upload"
            .to_string(),
    }
    .into()
}

/// Wrap a local-base read/parse failure as `sync_base_unreadable` — a corrupt
/// or truncated `{id}-snapshot.alf`. Coded so the watch loop classifies it
/// `MissingBase` (recover-once self-heals by re-pulling the base from cloud
/// truth) instead of retrying the same broken read forever (manual §4.2).
fn base_unreadable(err: anyhow::Error, path: &Path) -> anyhow::Error {
    CliError {
        code: codes::SYNC_BASE_UNREADABLE,
        cause: format!("local delta base {} is unreadable: {err:#}", path.display()),
        // Phrased as the exact CLI invocation so the MCP rewrite's
        // longest-match rule turns it into `alf_sync with recover:true`
        // (hand-annotating the MCP form here would double up — MIN-2).
        remedy: "re-run alf sync --recover to re-pull the base from cloud truth".to_string(),
    }
    .into()
}

/// Crash seam for the WP-M4 live kill/restart gate (brief task 9): abort the
/// process **before** any upload advances the cloud sequence — simulating a
/// SIGKILL at the moment the design's crash-safety argument covers (§5.3). A
/// restart's catch-up scan must then produce exactly one correct delta (base +
/// state were never advanced, so the re-run derives the identical upload).
///
/// **Gated behind the `fault-injection` build feature (WP-M3 review D1).** The
/// default/release binary compiles the no-op below and does not read any env, so
/// a stray/leftover env var can never abort a real `alf sync`. The WP-M4 harness
/// builds `alf` with `--features fault-injection` explicitly.
#[cfg(feature = "fault-injection")]
fn fault_before_upload() {
    if std::env::var_os("ALF_WATCH_FAULT_BEFORE_UPLOAD").is_some() {
        eprintln!("alf: ALF_WATCH_FAULT_BEFORE_UPLOAD set — aborting before upload (test seam)");
        std::process::exit(137); // 128 + SIGKILL(9)
    }
}

/// The default build: the fault seam does not exist — no env is read.
#[cfg(not(feature = "fault-injection"))]
fn fault_before_upload() {}

/// Crash seam for the window MIN-3 covers: the snapshot upload has landed in
/// the cloud but `persist_local` has not yet written the state file. Same
/// `fault-injection` gating as [`fault_before_upload`].
#[cfg(feature = "fault-injection")]
fn fault_after_upload() {
    if std::env::var_os("ALF_WATCH_FAULT_AFTER_UPLOAD").is_some() {
        eprintln!("alf: ALF_WATCH_FAULT_AFTER_UPLOAD set — aborting after upload (test seam)");
        std::process::exit(137); // 128 + SIGKILL(9)
    }
}

#[cfg(not(feature = "fault-injection"))]
fn fault_after_upload() {}

#[allow(clippy::too_many_arguments)]
fn execute_first_sync(
    client: &ApiClient,
    agent_id: uuid::Uuid,
    runtime: &str,
    agent_name: &str,
    alf_bytes: &[u8],
    temp_alf: &Path,
    snapshot_path: &Path,
    force_first_sync: bool,
    progress: Progress,
) -> Result<SyncOutcome> {
    progress.emit("  First sync — registering agent and uploading snapshot...");

    // Lazy provisioning: the client-supplied id is the mapping id; a 409 feeds
    // the E3 guard below.
    let outcome = client
        .register_agent(agent_id, agent_name, runtime)
        .map_err(wrap_registration)?;

    match decide_first_sync_conflict(
        outcome.already_existed,
        outcome.info.latest_sequence,
        first_sync_marker_present(agent_id),
        force_first_sync,
    ) {
        FirstSyncConflict::Upload => {}
        FirstSyncConflict::AdoptCloudBase => {
            // MIN-3: this machine's own first sync was killed between a
            // successful upload and the state write, so the "already exists"
            // conflict is our own snapshot, not a fork. Adopt cloud truth as
            // the base and land the current workspace on it — the workspace
            // may have moved on since the crash, so this must be a real delta
            // (pull + diff), never a blind state stamp.
            progress.emit(
                "  Interrupted first sync detected (upload landed, state did not) — \
                 adopting the uploaded snapshot as the base...",
            );
            let cloud = pull_cloud_base(client, agent_id, progress)?;
            // Base the delta on the SERVER's head (from the registration
            // probe), not on the sequence derived from the adopted archive:
            // a first-sync snapshot carries a sync cursor written at export
            // time — before the service assigned it a sequence — so the
            // archive reads back as 0 and the delta's CAS would 409.
            let base_sequence = outcome.info.latest_sequence.max(cloud.latest_sequence);
            let result = execute_delta(
                client,
                agent_id,
                runtime,
                base_sequence,
                Some(Utc::now()),
                alf_bytes,
                temp_alf,
                snapshot_path,
                /* recovered: */ true,
                progress,
            );
            if result.is_ok() {
                clear_first_sync_inflight(agent_id);
            }
            return result;
        }
        FirstSyncConflict::Park => {
            check_first_sync_safety(agent_id, runtime, &outcome, force_first_sync)?;
        }
    }

    fault_before_upload();
    // The marker must be durable BEFORE the upload: it is the only local
    // evidence that a cloud agent appearing at the next start is ours.
    mark_first_sync_inflight(agent_id);
    let upload = client
        .upload_snapshot(agent_id, alf_bytes)
        .map_err(wrap_upload)?;
    fault_after_upload();

    persist_local(agent_id, upload.sequence, temp_alf, snapshot_path)?;
    clear_first_sync_inflight(agent_id);

    Ok(SyncOutcome {
        sequence: upload.sequence,
        delta: false,
        changes: None,
        no_changes: false,
        recovered: false,
        resnapshot: false,
        snapshot_path: snapshot_path.to_path_buf(),
    })
}

#[allow(clippy::too_many_arguments)]
fn execute_delta(
    client: &ApiClient,
    agent_id: uuid::Uuid,
    runtime: &str,
    base_sequence: u64,
    base_timestamp: Option<chrono::DateTime<Utc>>,
    alf_bytes: &[u8],
    temp_alf: &Path,
    snapshot_path: &Path,
    recovered: bool,
    progress: Progress,
) -> Result<SyncOutcome> {
    progress.emit(&format!(
        "  Computing delta since sequence {base_sequence}..."
    ));

    let prev_bytes =
        fs::read(snapshot_path).map_err(|e| base_unreadable(e.into(), snapshot_path))?;
    let mut prev_reader = AlfReader::new(Cursor::new(&prev_bytes))
        .map_err(|e| base_unreadable(e.into(), snapshot_path))?;
    let prev_records = prev_reader.read_all_memory()?;
    let prev_creds = prev_reader.read_credentials()?;
    let prev_identity = prev_reader.read_identity()?;
    let prev_principals = prev_reader.read_principals()?;

    // WP4.1: reconcile the fresh export against the base BEFORE anything is
    // diffed, uploaded, or persisted. Matched records carry their id and
    // created_at/observed_at forward, so in-place curation (OpenClaw rewriting
    // MEMORY.md) becomes clean updates instead of id churn, and mtime-only
    // re-stamps produce no delta at all.
    let exported_records = {
        let mut reader = AlfReader::new(Cursor::new(alf_bytes))?;
        reader.read_all_memory()?
    };
    let reconciled = alf_core::reconcile::reconcile(&prev_records, exported_records);
    let effective_bytes: std::borrow::Cow<'_, [u8]> = if reconciled.rewritten {
        progress.emit(&format!(
            "  Reconciled memory identities: {} carried, {} updated in place, {} new, {} removed",
            reconciled.stats.carried,
            reconciled.stats.heading_matched + reconciled.stats.id_matched,
            reconciled.stats.created,
            reconciled.stats.deleted
        ));
        let bytes = alf_core::replace_memory_records(alf_bytes, &reconciled.records)
            .context("Failed to rewrite archive with reconciled memory records")?;
        // ONE buffer feeds everything downstream: the re-snapshot upload uses
        // `effective_bytes` and `persist_local` copies `temp_alf` — if they
        // ever diverged, local base and cloud record ids would part ways
        // permanently on the re-snapshot path.
        fs::write(temp_alf, &bytes).context("Failed to persist reconciled archive")?;
        std::borrow::Cow::Owned(bytes)
    } else {
        std::borrow::Cow::Borrowed(alf_bytes)
    };
    let curr_records = reconciled.records;

    let mut curr_reader = AlfReader::new(Cursor::new(effective_bytes.as_ref()))?;
    // The freshly-exported archive already carries the live vault (Layer 4),
    // so we diff it here against the previous base — never re-reading the vault
    // file and never decrypting. Diff is by credential `id` (see
    // `diff_credentials`), which is what defeats the fresh-nonce-per-encryption
    // churn that would otherwise re-upload everything each sync.
    let curr_creds = curr_reader.read_credentials()?;
    // Layers 1 (identity) and 2 (principals) can also change between syncs.
    // Diffs ignore the `updated_at` the adapter re-stamps on every export, so an
    // unchanged identity/principals set does not produce a spurious delta.
    let curr_identity = curr_reader.read_identity()?;
    let curr_principals = curr_reader.read_principals()?;

    // WP3: arbitrary tracked files (added via `alf add`) are opaque bytes the
    // delta format can't carry. If any tracked file — or the include list / sync
    // log — changed vs the base snapshot, upload a full snapshot instead of a
    // delta. The service treats this as a clean, non-destructive rollover (new
    // base at the current sequence; prior deltas retained for point-in-time).
    if tracked_files_changed(runtime, &mut prev_reader, &mut curr_reader)? {
        progress.emit("  Tracked workspace files changed — uploading full snapshot...");
        fault_before_upload();
        let upload = client
            .upload_snapshot(agent_id, effective_bytes.as_ref())
            .map_err(wrap_upload)?;
        persist_local(agent_id, upload.sequence, temp_alf, snapshot_path)?;
        return Ok(SyncOutcome {
            sequence: upload.sequence,
            delta: false,
            changes: None,
            no_changes: false,
            recovered,
            resnapshot: true,
            snapshot_path: snapshot_path.to_path_buf(),
        });
    }

    let delta_entries = compute_delta(&prev_records, &curr_records);
    let cred_diff = diff_credentials(prev_creds.as_ref(), curr_creds.as_ref());
    let princ_diff = diff_principals(prev_principals.as_ref(), curr_principals.as_ref());
    let id_changed = identity_changed(prev_identity.as_ref(), curr_identity.as_ref());

    // Diff the verbatim `raw/{runtime}/` tree so the delta carries the changed
    // source files. The structured diffs above keep the cloud dashboard correct;
    // this keeps a same-runtime `alf restore` correct, which rebuilds the
    // workspace from the raw tree and would otherwise see only the frozen
    // snapshot. Tracked files (`alf add`) already forced a re-snapshot above, so
    // they compare equal here and contribute nothing.
    let prev_raw = read_runtime_raw_map(&mut prev_reader, runtime)?;
    let curr_raw = read_runtime_raw_map(&mut curr_reader, runtime)?;
    let (raw_changed, raw_deleted) = diff_raw_trees(&prev_raw, &curr_raw);

    if delta_entries.is_empty()
        && cred_diff.is_empty()
        && princ_diff.is_empty()
        && !id_changed
        && raw_changed.is_empty()
        && raw_deleted.is_empty()
    {
        return Ok(SyncOutcome {
            sequence: base_sequence,
            delta: false,
            changes: None,
            no_changes: true,
            recovered,
            resnapshot: false,
            snapshot_path: snapshot_path.to_path_buf(),
        });
    }

    let creates = delta_entries
        .iter()
        .filter(|e| e.operation == alf_core::manifest::DeltaOperation::Create)
        .count();
    let updates = delta_entries
        .iter()
        .filter(|e| e.operation == alf_core::manifest::DeltaOperation::Update)
        .count();
    let deletes = delta_entries
        .iter()
        .filter(|e| e.operation == alf_core::manifest::DeltaOperation::Delete)
        .count();

    progress.emit(&format!(
        "  Delta: {creates} creates, {updates} updates, {deletes} deletes"
    ));
    if !cred_diff.is_empty() {
        progress.emit(&format!(
            "  Credentials: {} creates, {} updates, {} deletes",
            cred_diff.created.len(),
            cred_diff.updated.len(),
            cred_diff.deleted.len()
        ));
    }
    if !princ_diff.is_empty() {
        progress.emit(&format!(
            "  Principals: {} creates, {} updates, {} deletes",
            princ_diff.created.len(),
            princ_diff.updated.len(),
            princ_diff.deleted.len()
        ));
    }
    if id_changed {
        progress.emit("  Identity changed");
    }
    if !raw_changed.is_empty() || !raw_deleted.is_empty() {
        progress.emit(&format!(
            "  Raw sources: {} changed, {} removed",
            raw_changed.len(),
            raw_deleted.len()
        ));
    }

    let delta_manifest = DeltaManifest {
        alf_version: "1.0.0".into(),
        created_at: Utc::now(),
        agent: DeltaAgentRef {
            id: agent_id,
            source_runtime: Some(runtime.into()),
            extra: HashMap::new(),
        },
        sync: DeltaSyncCursor {
            base_sequence,
            new_sequence: 0,
            // base_timestamp is informational metadata propagated from the
            // previous successful sync. Never read by control flow.
            base_timestamp,
            new_timestamp: None,
            extra: HashMap::new(),
        },
        // This inventory is a placeholder: DeltaWriter::finish() rebuilds it
        // from whatever was actually written below (see set_credentials /
        // add_memory_deltas).
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

    let delta_buf = Cursor::new(Vec::new());
    let mut delta_writer = DeltaWriter::new(delta_buf, delta_manifest)?;
    if !delta_entries.is_empty() {
        delta_writer.add_memory_deltas(&delta_entries)?;
    }
    if !cred_diff.is_empty() {
        // Design: carry the full current Layer 4 whenever any credential
        // changed; rebuild does a full replace. The by-id diff above gates
        // this, so steady-state syncs attach nothing.
        match &curr_creds {
            Some(doc) => delta_writer.set_credentials(doc)?,
            // Every credential was deleted: the exported archive omits the
            // layer, so attach an empty document to make rebuild replace the
            // base set with nothing.
            None => delta_writer.set_credentials(&CredentialsDocument {
                credentials: Vec::new(),
                extra: HashMap::new(),
            })?,
        }
    }
    // Layer 1: carry the full current identity when it changed (single-doc
    // replace on rebuild). A `None` curr_identity (identity removed) is left
    // out — the format has no "delete identity" and every agent has one.
    if id_changed {
        if let Some(doc) = &curr_identity {
            delta_writer.set_identity(doc, doc.version)?;
        }
    }
    // Layer 2: same full-replace design as credentials — carry the whole current
    // principals set whenever any principal changed; empty doc when all removed.
    // `changed_ids` records which principals moved (created/updated/deleted).
    if !princ_diff.is_empty() {
        let changed_ids: Vec<uuid::Uuid> = princ_diff
            .created
            .iter()
            .chain(princ_diff.updated.iter())
            .chain(princ_diff.deleted.iter())
            .copied()
            .collect();
        match &curr_principals {
            Some(doc) => delta_writer.set_principals(doc, changed_ids)?,
            None => delta_writer.set_principals(
                &PrincipalsDocument {
                    principals: Vec::new(),
                    extra: HashMap::new(),
                },
                changed_ids,
            )?,
        }
    }
    // Raw source overlay: carry each changed file verbatim and record removals.
    // `rebuild` replays these onto the snapshot's raw tree so a same-runtime
    // restore sees the current workspace, not the frozen base.
    for path in &raw_changed {
        let data = curr_raw
            .get(path)
            .expect("raw_changed path is taken from curr_raw");
        delta_writer.add_raw_change(path, data)?;
    }
    for path in &raw_deleted {
        delta_writer.add_raw_deletion(path);
    }
    let delta_buf = delta_writer.finish()?;
    let delta_bytes = delta_buf.into_inner();

    progress.emit(&format!(
        "  Uploading delta ({} bytes)...",
        delta_bytes.len()
    ));
    fault_before_upload();
    let upload = client
        .push_delta(agent_id, base_sequence, &delta_bytes)
        .map_err(wrap_upload)?;

    persist_local(agent_id, upload.sequence, temp_alf, snapshot_path)?;

    Ok(SyncOutcome {
        sequence: upload.sequence,
        delta: true,
        changes: Some(SyncChanges {
            creates,
            updates,
            deletes,
            credentials: LayerChanges {
                creates: cred_diff.created.len(),
                updates: cred_diff.updated.len(),
                deletes: cred_diff.deleted.len(),
            },
            principals: LayerChanges {
                creates: princ_diff.created.len(),
                updates: princ_diff.updated.len(),
                deletes: princ_diff.deleted.len(),
            },
            identity: id_changed,
        }),
        no_changes: false,
        recovered,
        resnapshot: false,
        snapshot_path: snapshot_path.to_path_buf(),
    })
}

/// Whether any agent-tracked file (`alf add`) — or the include list / sync log
/// itself — differs between the base snapshot and the current export. Tracked
/// files are opaque bytes the delta format can't carry, so a change here means
/// `alf sync` must re-snapshot rather than push a delta. The include list is a
/// runtime-agnostic convention, so this applies to every runtime; an archive
/// with no include list simply yields two empty maps that compare equal.
fn tracked_files_changed<P, C>(
    runtime: &str,
    prev: &mut AlfReader<P>,
    curr: &mut AlfReader<C>,
) -> Result<bool>
where
    P: Read + Seek,
    C: Read + Seek,
{
    Ok(read_tracked_map(prev, runtime)? != read_tracked_map(curr, runtime)?)
}

/// Build `{ tracked-relative-path -> bytes }` from an archive: the files listed
/// in its `raw/{runtime}/.alf-include.json`, plus the include list and sync log
/// themselves. Comparing two such maps detects added/modified/removed tracked
/// files and any change to the tracked set or removal history.
fn read_tracked_map<R: Read + Seek>(
    reader: &mut AlfReader<R>,
    runtime: &str,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let names: HashSet<String> = reader.file_names().into_iter().collect();
    let prefix = format!("raw/{runtime}/");

    let mut rels: Vec<String> = Vec::new();
    let include_archive_path = format!("{prefix}{}", alf_core::INCLUDE_FILE);
    if names.contains(&include_archive_path) {
        let bytes = reader.read_raw_entry(&include_archive_path)?;
        if let Ok(list) = serde_json::from_slice::<alf_core::IncludeList>(&bytes) {
            rels = list.paths();
        }
    }
    // Track the include list + sync log themselves so any edit to the tracked
    // set or removal history also refreshes the cloud copy.
    rels.push(alf_core::INCLUDE_FILE.to_string());
    rels.push(alf_core::SYNC_LOG_FILE.to_string());

    let mut map = BTreeMap::new();
    for rel in rels {
        let archive_path = format!("{prefix}{rel}");
        if names.contains(&archive_path) {
            let bytes = reader.read_raw_entry(&archive_path)?;
            map.insert(rel, bytes);
        }
    }
    Ok(map)
}

/// Read every `raw/{runtime}/` source file from an archive, keyed by full
/// archive path. Backs the raw-source delta: comparing the base snapshot's tree
/// with the current export's tree yields the files a delta must carry so a
/// same-runtime restore stays current.
fn read_runtime_raw_map<R: Read + Seek>(
    reader: &mut AlfReader<R>,
    runtime: &str,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let prefix = format!("raw/{runtime}/");
    let names: Vec<String> = reader
        .file_names()
        .into_iter()
        .filter(|p| p.starts_with(&prefix) && !p.ends_with('/'))
        .collect();
    let mut map = BTreeMap::new();
    for name in names {
        let bytes = reader.read_raw_entry(&name)?;
        map.insert(name, bytes);
    }
    Ok(map)
}

/// Diff two `raw/{runtime}/` trees (full archive path -> bytes). Returns
/// `(changed, deleted)`: `changed` are paths present in `curr` that are new or
/// differ byte-for-byte from `prev`; `deleted` are paths in `prev` absent from
/// `curr`. Both are full `raw/...` archive paths.
fn diff_raw_trees(
    prev: &BTreeMap<String, Vec<u8>>,
    curr: &BTreeMap<String, Vec<u8>>,
) -> (Vec<String>, Vec<String>) {
    let changed: Vec<String> = curr
        .iter()
        .filter(|(path, bytes)| prev.get(*path).map(|p| p != *bytes).unwrap_or(true))
        .map(|(path, _)| path.clone())
        .collect();
    let deleted: Vec<String> = prev
        .keys()
        .filter(|path| !curr.contains_key(*path))
        .cloned()
        .collect();
    (changed, deleted)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// Review D1: in the default build (no `fault-injection` feature) the upload
    /// fault seam is a no-op — setting the env must NOT abort the process. If this
    /// test's process survives the call, the seam is inert.
    #[cfg(not(feature = "fault-injection"))]
    #[test]
    fn fault_before_upload_is_inert_without_the_feature() {
        std::env::set_var("ALF_WATCH_FAULT_BEFORE_UPLOAD", "1");
        fault_before_upload(); // would std::process::exit(137) if the seam were live
        std::env::remove_var("ALF_WATCH_FAULT_BEFORE_UPLOAD");
        // Reaching here proves the default build ignores the var.
    }

    /// Schema conformance (offline): a populated `SyncResult` — whose partial
    /// `SyncChanges` omits the zero credential/principal/identity layers —
    /// validates against the same schemars schema the MCP `alf_sync` tool
    /// declares as its `outputSchema`. This pins the `skip_serializing_if` +
    /// `#[serde(default)]` reconciliation so a real (backend) sync result can
    /// never drift from its declared schema. The MCP stdout harness only
    /// exercises `alf_sync`'s error path offline, so this fills the success gap.
    #[test]
    fn sync_result_matches_declared_output_schema() {
        let result = SyncResult {
            ok: true,
            sequence: 7,
            delta: true,
            changes: Some(SyncChanges {
                creates: 1,
                updates: 2,
                deletes: 0,
                credentials: LayerChanges::default(),
                principals: LayerChanges::default(),
                identity: false,
            }),
            snapshot_path: "/x/base.alf".into(),
            no_changes: false,
            recovered: false,
            agent: SyncAgentRef {
                runtime_agent: "main".into(),
                alf_agent_id: Uuid::nil(),
                source: SelectorSource::Flag,
            },
        };
        let instance = serde_json::to_value(&result).unwrap();
        // The zero layers must actually be omitted (the drift we're guarding).
        let changes = &instance["changes"];
        assert!(changes.get("credentials").is_none());
        assert!(changes.get("principals").is_none());
        assert!(changes.get("identity").is_none());

        let schema = serde_json::to_value(schemars::schema_for!(SyncResult)).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(
            validator.is_valid(&instance),
            "SyncResult must validate against its declared schema; instance = {instance}"
        );
    }

    /// A 409 sequence conflict must steer to restore, not a retry loop — a
    /// plain re-run pushes the same stale base and fails identically.
    #[test]
    fn wrap_upload_sequence_conflict_points_at_restore() {
        let err = wrap_upload(anyhow::anyhow!(
            "Sequence conflict: your local state is at sequence 3 but the \
             server is at 4. Pull the latest changes first:\n  \
             alf restore -r <runtime> -w <workspace> --agent <id>"
        ));
        let cli = err.downcast_ref::<CliError>().unwrap();
        assert_eq!(cli.code, codes::SYNC_UPLOAD_FAILED);
        assert!(cli.remedy.contains("alf restore"), "remedy: {}", cli.remedy);
        assert!(
            !cli.remedy.contains("re-run alf sync;"),
            "must not suggest a blind retry: {}",
            cli.remedy
        );

        // Any other upload error keeps the retry remedy.
        let err = wrap_upload(anyhow::anyhow!("connection reset by peer"));
        let cli = err.downcast_ref::<CliError>().unwrap();
        assert!(cli.remedy.contains("re-run alf sync"));
    }

    #[test]
    fn wrap_registration_maps_402_and_401() {
        let err = wrap_registration(anyhow::anyhow!(
            "registration rejected (HTTP 402): agent limit reached"
        ));
        let cli = err.downcast_ref::<CliError>().unwrap();
        assert_eq!(cli.code, codes::SUBSCRIPTION_DENIED);

        let err = wrap_registration(anyhow::anyhow!("HTTP 401: bad api key"));
        let cli = err.downcast_ref::<CliError>().unwrap();
        assert_eq!(cli.code, codes::AUTH_FAILED);

        let err = wrap_registration(anyhow::anyhow!("connection refused"));
        let cli = err.downcast_ref::<CliError>().unwrap();
        assert_eq!(cli.code, codes::REGISTRATION_FAILED);
    }

    #[test]
    fn wrap_upload_maps_401_to_auth_failed() {
        let err = wrap_upload(anyhow::anyhow!("authentication failed (HTTP 403)"));
        let cli = err.downcast_ref::<CliError>().unwrap();
        assert_eq!(cli.code, codes::AUTH_FAILED);
        assert!(cli.remedy.contains("alf login"), "remedy: {}", cli.remedy);
    }

    #[test]
    fn base_unreadable_is_coded() {
        let err = base_unreadable(
            anyhow::anyhow!("invalid zip: unexpected EOF"),
            Path::new("/home/x/.alf/state/abc-snapshot.alf"),
        );
        let cli = err.downcast_ref::<CliError>().unwrap();
        assert_eq!(cli.code, codes::SYNC_BASE_UNREADABLE);
        assert!(cli.cause.contains("abc-snapshot.alf"));
        assert!(cli.remedy.contains("--recover"), "remedy: {}", cli.remedy);
    }

    fn state_with(seq: Option<u64>) -> AgentState {
        AgentState {
            agent_id: Uuid::new_v4(),
            last_synced_sequence: seq,
            last_synced_at: None,
        }
    }

    /// Branch A — never synced: first sync regardless of base presence or --recover.
    #[test]
    fn decide_first_sync_when_sequence_is_none() {
        let s = state_with(None);
        assert_eq!(decide_sync_mode(&s, false, false), SyncMode::FirstSync);
        assert_eq!(decide_sync_mode(&s, true, false), SyncMode::FirstSync);
        assert_eq!(decide_sync_mode(&s, false, true), SyncMode::FirstSync);
        assert_eq!(decide_sync_mode(&s, true, true), SyncMode::FirstSync);
    }

    /// Branch B — synced + base present, no --recover: delta path.
    #[test]
    fn decide_delta_when_base_present() {
        let s = state_with(Some(7));
        assert_eq!(
            decide_sync_mode(&s, true, false),
            SyncMode::Delta { base_sequence: 7 }
        );
    }

    // -- MIN-3: the first-sync conflict matrix ------------------------------

    #[test]
    fn first_sync_without_a_cloud_agent_uploads() {
        for marker in [false, true] {
            assert_eq!(
                decide_first_sync_conflict(false, 0, marker, false),
                FirstSyncConflict::Upload
            );
        }
    }

    #[test]
    fn first_sync_conflict_without_a_marker_parks() {
        // A genuine fork: the cloud has history this machine never wrote. The
        // E3 guard is unchanged — this is the case it exists for.
        assert_eq!(
            decide_first_sync_conflict(true, 4, false, false),
            FirstSyncConflict::Park
        );
    }

    #[test]
    fn first_sync_conflict_with_a_marker_adopts_the_cloud_base() {
        // Our own upload landed before the crash (cloud has history) →
        // self-heal instead of asking a human.
        assert_eq!(
            decide_first_sync_conflict(true, 1, true, false),
            FirstSyncConflict::AdoptCloudBase
        );
    }

    #[test]
    fn first_sync_conflict_with_a_marker_but_no_cloud_history_uploads() {
        // The register call landed, the upload did not: nothing in the cloud
        // to overwrite, so the retry just uploads.
        assert_eq!(
            decide_first_sync_conflict(true, 0, true, false),
            FirstSyncConflict::Upload
        );
    }

    #[test]
    fn force_first_sync_always_uploads() {
        // The operator's explicit override outranks every conflict signal.
        assert_eq!(
            decide_first_sync_conflict(true, 9, false, true),
            FirstSyncConflict::Upload
        );
        assert_eq!(
            decide_first_sync_conflict(true, 9, true, true),
            FirstSyncConflict::Upload
        );
    }

    /// Branch B' — synced + base present + --recover: self-heal (RC-11). Recover
    /// wins even with a healthy base so a diverged/poisoned base can be repaired
    /// unattended, without an operator deleting the base file first.
    #[test]
    fn decide_recover_when_base_present_and_recover() {
        let s = state_with(Some(7));
        assert_eq!(
            decide_sync_mode(&s, true, true),
            SyncMode::Recover { base_sequence: 7 },
            "--recover must re-pull cloud truth even when a local base exists"
        );
    }

    /// Branch C — synced but base missing, no --recover: bail.
    #[test]
    fn decide_bail_when_base_missing_and_no_recover() {
        let s = state_with(Some(3));
        assert_eq!(
            decide_sync_mode(&s, false, false),
            SyncMode::BailMissingBase {
                last_synced_sequence: 3,
            }
        );
    }

    /// Branch D — synced but base missing, --recover passed: recover.
    #[test]
    fn decide_recover_when_base_missing_and_recover() {
        let s = state_with(Some(5));
        assert_eq!(
            decide_sync_mode(&s, false, true),
            SyncMode::Recover { base_sequence: 5 }
        );
    }

    /// Some(0) is the post-first-sync state, NOT a fresh state. The
    /// `Option<u64>` typing keeps the two distinct.
    #[test]
    fn decide_some_zero_with_base_is_delta_not_first_sync() {
        let s = state_with(Some(0));
        assert_eq!(
            decide_sync_mode(&s, true, false),
            SyncMode::Delta { base_sequence: 0 }
        );
    }

    /// last_synced_at is informational metadata only — flipping it does not
    /// change the chosen sync mode.
    #[test]
    fn decide_ignores_last_synced_at() {
        let mut s = state_with(Some(2));
        let m1 = decide_sync_mode(&s, true, false);
        s.last_synced_at = Some(Utc::now());
        let m2 = decide_sync_mode(&s, true, false);
        assert_eq!(m1, m2);
    }

    fn make_outcome(already_existed: bool, latest_sequence: u64) -> RegisterAgentOutcome {
        RegisterAgentOutcome {
            info: crate::api_client::AgentInfo {
                id: Uuid::new_v4(),
                name: "Test".into(),
                source_runtime: Some("openclaw".into()),
                created_at: "2026-05-09T00:00:00Z".into(),
                latest_sequence,
                layer_counts: None,
            },
            already_existed,
        }
    }

    /// Branch E — first sync but the cloud already has this agent: bail unless
    /// --force-first-sync is set. (E3 guard.)
    #[test]
    fn first_sync_safety_bails_on_409_without_force_flag() {
        let outcome = make_outcome(true, 7);
        let result = check_first_sync_safety(Uuid::new_v4(), "openclaw", &outcome, false);
        let err = result.expect_err("E3 guard must reject already-registered agent");
        let msg = format!("{err}");
        assert!(
            msg.contains("already exists in the cloud"),
            "error must explain why: {msg}"
        );
        assert!(
            msg.contains("--force-first-sync"),
            "error must mention the override flag: {msg}"
        );
        assert!(
            msg.contains("alf restore"),
            "error must offer the safe alternative: {msg}"
        );
    }

    #[test]
    fn first_sync_safety_allows_409_with_force_flag() {
        let outcome = make_outcome(true, 7);
        check_first_sync_safety(Uuid::new_v4(), "openclaw", &outcome, true)
            .expect("--force-first-sync must override the guard");
    }

    #[test]
    fn first_sync_safety_passes_for_freshly_created_agent() {
        let outcome = make_outcome(false, 0);
        check_first_sync_safety(Uuid::new_v4(), "openclaw", &outcome, false)
            .expect("freshly created agent must not trip the guard");
    }

    /// Atomic-write invariant: persist_local writes base.alf BEFORE state.toml.
    /// We assert this via mtime ordering on a HOME-redirected temp dir.
    #[test]
    fn persist_local_writes_base_before_state() {
        let _guard = crate::context::tests::HOME_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());

        let agent_id = Uuid::new_v4();
        let snapshot_path = crate::state::local_base_path(agent_id).unwrap();
        fs::create_dir_all(snapshot_path.parent().unwrap()).unwrap();

        let temp_alf = tmp.path().join("export.alf");
        fs::write(&temp_alf, b"fake-alf-bytes").unwrap();

        persist_local(agent_id, 42, &temp_alf, &snapshot_path).unwrap();

        let state_path = crate::state::state_file_path(agent_id).unwrap();
        let base_mtime = fs::metadata(&snapshot_path).unwrap().modified().unwrap();
        let state_mtime = fs::metadata(&state_path).unwrap().modified().unwrap();

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }

        assert!(snapshot_path.is_file());
        assert!(state_path.is_file());
        assert!(
            base_mtime <= state_mtime,
            "atomic-write invariant: base.alf must be written before state.toml \
             (base_mtime={:?}, state_mtime={:?})",
            base_mtime,
            state_mtime
        );

        // And, rounding out the invariant: the saved sequence must be Some(42).
        let saved = AgentState::load_from(&state_path, agent_id).unwrap();
        assert_eq!(saved.last_synced_sequence, Some(42));
        assert!(saved.last_synced_at.is_some());

        // Review B1: the base is written whole (atomic temp+rename), and no
        // `.alf.tmp` staging file survives.
        assert_eq!(fs::read(&snapshot_path).unwrap(), b"fake-alf-bytes");
        let leftovers: Vec<_> = fs::read_dir(snapshot_path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no staging temp file may survive persist_local: {leftovers:?}"
        );
    }

    /// WP3: tracked-file change detection drives the re-snapshot decision —
    /// a modified tracked file flips it on; a memory-only change does not.
    #[test]
    fn tracked_change_detection_distinguishes_tracked_vs_memory() {
        use adapter_openclaw::{IncludeList, OpenClawAdapter};
        use alf_core::Adapter;

        let _guard = crate::context::tests::HOME_LOCK.lock().unwrap();
        let prev_home = std::env::var_os("HOME");
        let home = tempfile::TempDir::new().unwrap();
        std::env::set_var("HOME", home.path());

        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("SOUL.md"), "# A\n\nsoul").unwrap();
        fs::write(ws.join("MEMORY.md"), "## Fact\n\nv1").unwrap();
        fs::write(ws.join("notes.txt"), "v1").unwrap();
        let mut list = IncludeList::default();
        list.add("notes.txt");
        list.save(&ws).unwrap();

        let adapter = OpenClawAdapter;
        let export_to = |name: &str| -> Vec<u8> {
            let p = tmp.path().join(name);
            adapter.export(&ws, &p).unwrap();
            fs::read(&p).unwrap()
        };
        let changed = |a: &[u8], b: &[u8]| -> bool {
            tracked_files_changed(
                "openclaw",
                &mut AlfReader::new(Cursor::new(a)).unwrap(),
                &mut AlfReader::new(Cursor::new(b)).unwrap(),
            )
            .unwrap()
        };

        let base = export_to("base.alf");
        assert!(
            !changed(&base, &base),
            "identical archives: no tracked change"
        );

        // Modify the tracked file → re-snapshot.
        fs::write(ws.join("notes.txt"), "v2-modified").unwrap();
        let modified = export_to("modified.alf");
        assert!(changed(&base, &modified), "modified tracked file → change");

        // Memory-only change (tracked file identical) → NO re-snapshot.
        fs::write(ws.join("MEMORY.md"), "## Fact\n\nv2-memory-only").unwrap();
        let mem_only = export_to("mem.alf");
        assert!(
            !changed(&modified, &mem_only),
            "memory-only change must not trigger a re-snapshot"
        );

        // Non-openclaw runtime never re-snapshots on this path.
        assert!(!tracked_files_changed(
            "zeroclaw",
            &mut AlfReader::new(Cursor::new(&base)).unwrap(),
            &mut AlfReader::new(Cursor::new(&modified)).unwrap(),
        )
        .unwrap());

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    fn raw_map(pairs: &[(&str, &[u8])]) -> BTreeMap<String, Vec<u8>> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_vec()))
            .collect()
    }

    #[test]
    fn diff_raw_trees_detects_add_update_delete() {
        let prev = raw_map(&[
            ("raw/openclaw/SOUL.md", b"v0"),
            ("raw/openclaw/memory/2026-01-15.md", b"day 15"),
            ("raw/openclaw/gone.md", b"bye"),
        ]);
        // SOUL.md updated, 2026-01-15 unchanged, 2026-01-16 added, gone.md removed.
        let curr = raw_map(&[
            ("raw/openclaw/SOUL.md", b"v1"),
            ("raw/openclaw/memory/2026-01-15.md", b"day 15"),
            ("raw/openclaw/memory/2026-01-16.md", b"day 16"),
        ]);

        let (changed, deleted) = diff_raw_trees(&prev, &curr);

        assert_eq!(
            changed,
            vec![
                "raw/openclaw/SOUL.md".to_string(),
                "raw/openclaw/memory/2026-01-16.md".to_string(),
            ]
        );
        assert_eq!(deleted, vec!["raw/openclaw/gone.md".to_string()]);
    }

    #[test]
    fn diff_raw_trees_identical_trees_is_empty() {
        let tree = raw_map(&[
            ("raw/openclaw/SOUL.md", b"same"),
            ("raw/openclaw/memory/2026-01-15.md", b"day 15"),
        ]);
        let (changed, deleted) = diff_raw_trees(&tree, &tree);
        assert!(
            changed.is_empty() && deleted.is_empty(),
            "an unchanged workspace must not churn a raw delta (re-sync no-op)"
        );
    }
}
