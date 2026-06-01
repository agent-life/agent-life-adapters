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
use crate::commands::restore::pull_cloud_base;
use crate::config::Config;
use crate::output;
use crate::state::{local_base_exists, local_base_path, state_file_path, AgentState};

use alf_core::archive::{AlfReader, DeltaWriter};
use alf_core::delta::{compute_delta, diff_credentials};
use alf_core::manifest::{ChangeInventory, DeltaAgentRef, DeltaManifest, DeltaSyncCursor};
use alf_core::CredentialsDocument;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use colored::Colorize;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{Cursor, Read, Seek};
use std::path::Path;

#[derive(Serialize)]
struct SyncResult {
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
}

#[derive(Serialize)]
struct SyncChanges {
    creates: usize,
    updates: usize,
    deletes: usize,
    /// Layer 4 (credentials) changes carried by this delta. Omitted when the
    /// vault was unchanged.
    #[serde(skip_serializing_if = "CredentialChanges::is_zero")]
    credentials: CredentialChanges,
}

#[derive(Serialize)]
struct CredentialChanges {
    creates: usize,
    updates: usize,
    deletes: usize,
}

impl CredentialChanges {
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
pub(crate) fn decide_sync_mode(
    state: &AgentState,
    base_present: bool,
    recover: bool,
) -> SyncMode {
    match (state.last_synced_sequence, base_present, recover) {
        (None, _, _) => SyncMode::FirstSync,
        (Some(n), true, _) => SyncMode::Delta { base_sequence: n },
        (Some(n), false, true) => SyncMode::Recover { base_sequence: n },
        (Some(n), false, false) => SyncMode::BailMissingBase {
            last_synced_sequence: n,
        },
    }
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
             Either run `alf restore -r {} -w <workspace> -a {}` first to hydrate state, \
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
    fs::copy(temp_alf, snapshot_path).with_context(|| {
        format!("Failed to persist snapshot at {}", snapshot_path.display())
    })?;

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
    /// State says we have synced before, the local base is missing, and
    /// `--recover` was passed. Pull the cloud base, then take the delta
    /// path at `base_sequence`.
    Recover { base_sequence: u64 },
}

pub fn run(
    runtime: &str,
    workspace: &Path,
    recover: bool,
    force_first_sync: bool,
) -> Result<()> {
    let human = output::human_mode();

    let config = Config::load()?;
    let client = ApiClient::from_config(&config)?;

    let adapt = adapter::get_adapter(runtime).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown runtime '{}'. Supported: {}",
            runtime,
            adapter::supported_runtimes()
        )
    })?;

    if !workspace.exists() {
        bail!(
            "Workspace directory does not exist: {}",
            workspace.display()
        );
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
        output::progress(&format!("Syncing {} workspace...", adapt.name()));
        output::progress(&format!("  Workspace: {}", workspace.display()));
    }

    // WP3: prune tracked files (added via `alf add`) that the agent has since
    // deleted, recording each removal in `.alf-sync-log.md` — BEFORE export so
    // the cleaned include list and the log are captured in this sync. The
    // include list is a runtime-agnostic workspace convention (alf_core), so
    // this applies to every runtime whose adapter packs the tracked files.
    let removed = alf_core::prune_and_log_missing(workspace)?;
    for rel in &removed {
        output::progress(&format!(
            "  Removed {rel} from sync (file no longer present; logged to {})",
            alf_core::SYNC_LOG_FILE
        ));
    }

    // Export workspace to a temp file to discover the agent ID.
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    let temp_alf = temp_dir.path().join("snapshot.alf");

    output::progress("  Exporting workspace...");
    let report = adapt.export(workspace, &temp_alf)?;
    output::progress(&format!(
        "  Exported {} memory records",
        report.memory_records
    ));

    let alf_bytes = fs::read(&temp_alf).context("Failed to read temp .alf file")?;
    let reader = AlfReader::new(Cursor::new(&alf_bytes))?;
    let agent_id = reader.manifest().agent.id;

    // Decide the sync mode strictly from (sequence, base_present, recover).
    let state = AgentState::load(agent_id)?;
    let base_present = local_base_exists(agent_id)?;
    let mode = decide_sync_mode(&state, base_present, recover);

    let snapshot_path = local_base_path(agent_id)?;
    if let Some(parent) = snapshot_path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("Failed to create state directory {}", parent.display())
        })?;
    }

    match mode {
        SyncMode::FirstSync => execute_first_sync(
            &client,
            agent_id,
            runtime,
            &report.agent_name,
            &alf_bytes,
            &temp_alf,
            &snapshot_path,
            force_first_sync,
            human,
        ),
        SyncMode::Delta { base_sequence } => execute_delta(
            &client,
            agent_id,
            runtime,
            base_sequence,
            state.last_synced_at,
            &alf_bytes,
            &temp_alf,
            &snapshot_path,
            /* recovered: */ false,
            human,
        ),
        SyncMode::Recover { base_sequence } => {
            output::progress(&format!(
                "  Local base missing — recovering from cloud (base sequence {base_sequence})..."
            ));
            // pull_cloud_base writes base.alf and state.toml under ~/.alf/state/.
            let cloud = pull_cloud_base(&client, agent_id)?;
            output::progress(&format!(
                "  Recovered local base at sequence {} ({})",
                cloud.latest_sequence,
                cloud.local_base.display()
            ));
            execute_delta(
                &client,
                agent_id,
                runtime,
                cloud.latest_sequence,
                Some(Utc::now()),
                &alf_bytes,
                &temp_alf,
                &snapshot_path,
                /* recovered: */ true,
                human,
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
    human: bool,
) -> Result<()> {
    output::progress("  First sync — registering agent and uploading snapshot...");

    let outcome = client.register_agent(agent_id, agent_name, runtime)?;

    check_first_sync_safety(agent_id, runtime, &outcome, force_first_sync)?;

    let upload = client.upload_snapshot(agent_id, alf_bytes)?;

    persist_local(agent_id, upload.sequence, temp_alf, snapshot_path)?;

    if human {
        let state_path = state_file_path(agent_id)?;
        println!(
            "{} Snapshot uploaded (sequence: {})",
            "✓".green().bold(),
            upload.sequence
        );
        println!("  Snapshot base: {}", snapshot_path.display());
        println!("  State file:    {}", state_path.display());
    } else {
        output::json(&SyncResult {
            ok: true,
            sequence: upload.sequence,
            delta: false,
            changes: None,
            snapshot_path: snapshot_path.to_string_lossy().into(),
            no_changes: false,
            recovered: false,
        });
    }

    Ok(())
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
    human: bool,
) -> Result<()> {
    output::progress(&format!(
        "  Computing delta since sequence {base_sequence}..."
    ));

    let prev_bytes = fs::read(snapshot_path).with_context(|| {
        format!(
            "Failed to read previous snapshot at {}",
            snapshot_path.display()
        )
    })?;
    let mut prev_reader = AlfReader::new(Cursor::new(&prev_bytes))?;
    let prev_records = prev_reader.read_all_memory()?;
    let prev_creds = prev_reader.read_credentials()?;

    let mut curr_reader = AlfReader::new(Cursor::new(alf_bytes))?;
    let curr_records = curr_reader.read_all_memory()?;
    // The freshly-exported archive already carries the live vault (Layer 4),
    // so we diff it here against the previous base — never re-reading the vault
    // file and never decrypting. Diff is by credential `id` (see
    // `diff_credentials`), which is what defeats the fresh-nonce-per-encryption
    // churn that would otherwise re-upload everything each sync.
    let curr_creds = curr_reader.read_credentials()?;

    // WP3: arbitrary tracked files (added via `alf add`) are opaque bytes the
    // delta format can't carry. If any tracked file — or the include list / sync
    // log — changed vs the base snapshot, upload a full snapshot instead of a
    // delta. The service treats this as a clean, non-destructive rollover (new
    // base at the current sequence; prior deltas retained for point-in-time).
    if tracked_files_changed(runtime, &mut prev_reader, &mut curr_reader)? {
        output::progress("  Tracked workspace files changed — uploading full snapshot...");
        let upload = client.upload_snapshot(agent_id, alf_bytes)?;
        persist_local(agent_id, upload.sequence, temp_alf, snapshot_path)?;
        if human {
            let state_path = state_file_path(agent_id)?;
            let label = if recovered {
                "Re-snapshot uploaded (recovered; tracked files changed)"
            } else {
                "Re-snapshot uploaded (tracked files changed)"
            };
            println!("{} {} (sequence: {})", "✓".green().bold(), label, upload.sequence);
            println!("  Snapshot base: {}", snapshot_path.display());
            println!("  State file:    {}", state_path.display());
        } else {
            output::json(&SyncResult {
                ok: true,
                sequence: upload.sequence,
                delta: false,
                changes: None,
                snapshot_path: snapshot_path.to_string_lossy().into(),
                no_changes: false,
                recovered,
            });
        }
        return Ok(());
    }

    let delta_entries = compute_delta(&prev_records, &curr_records);
    let cred_diff = diff_credentials(prev_creds.as_ref(), curr_creds.as_ref());

    if delta_entries.is_empty() && cred_diff.is_empty() {
        if human {
            println!(
                "{} No changes detected — already up to date",
                "✓".green().bold()
            );
        } else {
            output::json(&SyncResult {
                ok: true,
                sequence: base_sequence,
                delta: false,
                changes: None,
                snapshot_path: snapshot_path.to_string_lossy().into(),
                no_changes: true,
                recovered,
            });
        }
        return Ok(());
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

    output::progress(&format!(
        "  Delta: {creates} creates, {updates} updates, {deletes} deletes"
    ));
    if !cred_diff.is_empty() {
        output::progress(&format!(
            "  Credentials: {} creates, {} updates, {} deletes",
            cred_diff.created.len(),
            cred_diff.updated.len(),
            cred_diff.deleted.len()
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
    let delta_buf = delta_writer.finish()?;
    let delta_bytes = delta_buf.into_inner();

    output::progress(&format!(
        "  Uploading delta ({} bytes)...",
        delta_bytes.len()
    ));
    let upload = client.push_delta(agent_id, base_sequence, &delta_bytes)?;

    persist_local(agent_id, upload.sequence, temp_alf, snapshot_path)?;

    if human {
        let state_path = state_file_path(agent_id)?;
        let label = if recovered {
            "Delta uploaded (recovered)"
        } else {
            "Delta uploaded"
        };
        println!(
            "{} {} (sequence: {})",
            "✓".green().bold(),
            label,
            upload.sequence
        );
        println!("  Snapshot base: {}", snapshot_path.display());
        println!("  State file:    {}", state_path.display());
    } else {
        output::json(&SyncResult {
            ok: true,
            sequence: upload.sequence,
            delta: true,
            changes: Some(SyncChanges {
                creates,
                updates,
                deletes,
                credentials: CredentialChanges {
                    creates: cred_diff.created.len(),
                    updates: cred_diff.updated.len(),
                    deletes: cred_diff.deleted.len(),
                },
            }),
            snapshot_path: snapshot_path.to_string_lossy().into(),
            no_changes: false,
            recovered,
        });
    }

    Ok(())
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

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

    /// Branch B — synced + base present: delta path. --recover is a no-op.
    #[test]
    fn decide_delta_when_base_present() {
        let s = state_with(Some(7));
        assert_eq!(
            decide_sync_mode(&s, true, false),
            SyncMode::Delta { base_sequence: 7 }
        );
        assert_eq!(
            decide_sync_mode(&s, true, true),
            SyncMode::Delta { base_sequence: 7 },
            "--recover should be a no-op when the local base is healthy"
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
        assert!(!changed(&base, &base), "identical archives: no tracked change");

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
}
