//! `alf restore` — download and restore from the cloud.
//!
//! Flow:
//! 1. Load config (check API key)
//! 2. Parse agent ID
//! 3. Call restore endpoint (gets snapshot URL + delta URLs in one call)
//! 4. Download snapshot, download deltas, apply deltas
//! 5. Resolve adapter, import into workspace
//! 6. Save state with latest sequence

use crate::adapter;
use crate::api_client::ApiClient;
use crate::config::Config;
use crate::state::{AgentState, resolve_agent_id};

use anyhow::Context;
use anyhow::Result;
use chrono::Utc;
use colored::Colorize;
use std::fs;
use std::path::Path;
use uuid::Uuid;

pub fn run(runtime: &str, workspace: &Path, agent_arg: Option<&str>) -> Result<()> {
    // 1. Load config and create API client
    let config = Config::load()?;
    let client = ApiClient::from_config(&config)?;

    // 2. Resolve agent ID (CLI arg or ~/.alf/state/*.toml)
    let agent_id: Uuid = resolve_agent_id(agent_arg)?;

    // 3. Resolve adapter
    let adapt = adapter::get_adapter(runtime).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown runtime '{}'. Supported: {}",
            runtime,
            adapter::supported_runtimes()
        )
    })?;

    println!(
        "{} Restoring agent {} into {} workspace...",
        "▸".blue().bold(),
        &agent_id.to_string()[..8],
        adapt.name()
    );
    println!("  Agent:     {agent_id}");
    println!("  Runtime:   {}", adapt.name());
    println!("  Workspace: {}", workspace.display());
    println!();

    // 4. Call restore endpoint — gets snapshot + delta URLs in one call
    println!("  Fetching restore manifest...");
    let restore = client.restore(agent_id)?;

    let snapshot_bytes = match &restore.snapshot {
        Some(snap) => {
            println!(
                "  Downloading snapshot (sequence {})...",
                snap.sequence
            );
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

    // 5. Download deltas and rebuild snapshot with all deltas applied
    let mut delta_bytes_list: Vec<Vec<u8>> = Vec::new();
    if !restore.deltas.is_empty() {
        println!("  Downloading {} delta(s)...", restore.deltas.len());
        for (i, delta_info) in restore.deltas.iter().enumerate() {
            println!(
                "    Delta {} of {} (sequence {})...",
                i + 1, restore.deltas.len(), delta_info.sequence
            );
            delta_bytes_list.push(client.download_presigned(&delta_info.url)?);
        }
    } else {
        println!("  No additional deltas to apply.");
    }

    let refs: Vec<&[u8]> = delta_bytes_list.iter().map(|v| v.as_slice()).collect();
    let final_bytes = if refs.is_empty() {
        snapshot_bytes
    } else {
        println!("  Rebuilding snapshot with {} delta(s) applied...", refs.len());
        alf_core::rebuild_snapshot(&snapshot_bytes, &refs)?
    };

    // 6. Write snapshot to temp file and import
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    let temp_alf = temp_dir.path().join("restored.alf");
    fs::write(&temp_alf, &final_bytes)?;
    println!("  Restored snapshot: {}", temp_alf.display());

    println!("  Importing into workspace...");
    let import_report = adapt.import(&temp_alf, workspace)?;

    // 7. Save state
    let latest_sequence = if !restore.deltas.is_empty() {
        restore.deltas.last().unwrap().sequence
    } else {
        snapshot_sequence
    };

    let state = AgentState {
        agent_id,
        last_synced_sequence: latest_sequence,
        last_synced_at: Some(Utc::now()),
        snapshot_path: None, // restored, not exported locally
    };
    state.save()?;

    let state_path = AgentState::path_for(agent_id)?;

    // 8. Print summary
    println!();
    println!("  State file:   {}", state_path.display());
    println!("{} Restore complete", "✓".green().bold());
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

    Ok(())
}
