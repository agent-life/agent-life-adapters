//! `alf sync` — incremental sync to the cloud.
//!
//! Flow:
//! 1. Load config (check API key present)
//! 2. Resolve adapter, export workspace to a temp .alf
//! 3. Read the manifest to get the agent ID
//! 4. Load agent state from ~/.alf/state/
//! 5. If first sync → upload full snapshot
//! 6. If subsequent → load previous snapshot, compute delta, upload delta
//! 7. Update state with new sequence number

use crate::adapter;
use crate::api_client::ApiClient;
use crate::config::Config;
use crate::output;
use crate::state::AgentState;

use alf_core::archive::{AlfReader, DeltaWriter};
use alf_core::delta::compute_delta;
use alf_core::manifest::{DeltaManifest, DeltaAgentRef, DeltaSyncCursor, ChangeInventory};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use colored::Colorize;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

#[derive(Serialize)]
struct SyncResult {
    ok: bool,
    sequence: u64,
    delta: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    changes: Option<SyncChanges>,
    snapshot_path: String,
    no_changes: bool,
}

#[derive(Serialize)]
struct SyncChanges {
    creates: usize,
    updates: usize,
    deletes: usize,
}

pub fn run(runtime: &str, workspace: &Path) -> Result<()> {
    let human = output::human_mode();

    // 1. Load config and create API client
    let config = Config::load()?;
    let client = ApiClient::from_config(&config)?;

    // 2. Resolve adapter
    let adapt = adapter::get_adapter(runtime).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown runtime '{}'. Supported: {}",
            runtime,
            adapter::supported_runtimes()
        )
    })?;

    if !workspace.exists() {
        bail!("Workspace directory does not exist: {}", workspace.display());
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

    // 3. Export workspace to a temp file
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    let temp_alf = temp_dir.path().join("snapshot.alf");

    output::progress("  Exporting workspace...");
    let report = adapt.export(workspace, &temp_alf)?;
    output::progress(&format!("  Exported {} memory records", report.memory_records));

    // 4. Read the exported archive to get agent ID
    let alf_bytes = fs::read(&temp_alf).context("Failed to read temp .alf file")?;
    let reader = AlfReader::new(Cursor::new(&alf_bytes))?;
    let agent_id = reader.manifest().agent.id;

    // Stable snapshot path under ~/.alf/state for future delta computation
    let mut snapshot_path: PathBuf = AgentState::state_dir()?;
    if !snapshot_path.exists() {
        fs::create_dir_all(&snapshot_path)
            .with_context(|| format!("Failed to create state directory {}", snapshot_path.display()))?;
    }
    snapshot_path.push(format!("{agent_id}-snapshot.alf"));

    // 5. Load agent state
    let state = AgentState::load(agent_id)?;

    if !state.has_synced() {
        // First sync: upload full snapshot
        output::progress("  First sync — registering agent and uploading snapshot...");

        let _agent_info = client.register_agent(
            agent_id,
            &report.agent_name,
            runtime,
        )?;

        let upload = client.upload_snapshot(agent_id, &alf_bytes)?;

        fs::copy(&temp_alf, &snapshot_path)
            .with_context(|| format!("Failed to persist snapshot at {}", snapshot_path.display()))?;

        let new_state = AgentState {
            agent_id,
            last_synced_sequence: upload.sequence,
            last_synced_at: Some(Utc::now()),
            snapshot_path: Some(snapshot_path.to_string_lossy().into()),
        };
        new_state.save()?;

        if human {
            let state_path = AgentState::path_for(agent_id)?;
            println!("{} Snapshot uploaded (sequence: {})", "✓".green().bold(), upload.sequence);
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
            });
        }
    } else {
        // Subsequent sync: compute and upload delta
        output::progress(&format!(
            "  Computing delta since sequence {}...",
            state.last_synced_sequence
        ));

        let prev_path: PathBuf = if let Some(p) = &state.snapshot_path {
            p.into()
        } else {
            snapshot_path.clone()
        };

        let prev_bytes =
            fs::read(&prev_path).with_context(|| format!("Failed to read previous snapshot at {}", prev_path.display()))?;
        let mut prev_reader = AlfReader::new(Cursor::new(&prev_bytes))?;
        let prev_records = prev_reader.read_all_memory()?;

        let mut curr_reader = AlfReader::new(Cursor::new(&alf_bytes))?;
        let curr_records = curr_reader.read_all_memory()?;

        let delta_entries = compute_delta(&prev_records, &curr_records);

        if delta_entries.is_empty() {
            if human {
                println!("{} No changes detected — already up to date", "✓".green().bold());
            } else {
                output::json(&SyncResult {
                    ok: true,
                    sequence: state.last_synced_sequence,
                    delta: false,
                    changes: None,
                    snapshot_path: snapshot_path.to_string_lossy().into(),
                    no_changes: true,
                });
            }
            return Ok(());
        }

        let creates = delta_entries.iter().filter(|e| e.operation == alf_core::manifest::DeltaOperation::Create).count();
        let updates = delta_entries.iter().filter(|e| e.operation == alf_core::manifest::DeltaOperation::Update).count();
        let deletes = delta_entries.iter().filter(|e| e.operation == alf_core::manifest::DeltaOperation::Delete).count();

        output::progress(&format!("  Delta: {creates} creates, {updates} updates, {deletes} deletes"));

        let delta_manifest = DeltaManifest {
            alf_version: "1.0.0".into(),
            created_at: Utc::now(),
            agent: DeltaAgentRef {
                id: agent_id,
                source_runtime: Some(runtime.into()),
                extra: HashMap::new(),
            },
            sync: DeltaSyncCursor {
                base_sequence: state.last_synced_sequence,
                new_sequence: 0,
                base_timestamp: state.last_synced_at,
                new_timestamp: None,
                extra: HashMap::new(),
            },
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
        delta_writer.add_memory_deltas(&delta_entries)?;
        let delta_buf = delta_writer.finish()?;
        let delta_bytes = delta_buf.into_inner();

        output::progress(&format!("  Uploading delta ({} bytes)...", delta_bytes.len()));
        let upload = client.push_delta(agent_id, state.last_synced_sequence, &delta_bytes)?;

        fs::copy(&temp_alf, &snapshot_path)
            .with_context(|| format!("Failed to persist snapshot at {}", snapshot_path.display()))?;

        let new_state = AgentState {
            agent_id,
            last_synced_sequence: upload.sequence,
            last_synced_at: Some(Utc::now()),
            snapshot_path: Some(snapshot_path.to_string_lossy().into()),
        };
        new_state.save()?;

        if human {
            let state_path = AgentState::path_for(agent_id)?;
            println!(
                "{} Delta uploaded (sequence: {})",
                "✓".green().bold(),
                upload.sequence
            );
            println!("  Snapshot base: {}", snapshot_path.display());
            println!("  State file:    {}", state_path.display());
        } else {
            output::json(&SyncResult {
                ok: true,
                sequence: upload.sequence,
                delta: true,
                changes: Some(SyncChanges { creates, updates, deletes }),
                snapshot_path: snapshot_path.to_string_lossy().into(),
                no_changes: false,
            });
        }
    }

    Ok(())
}
