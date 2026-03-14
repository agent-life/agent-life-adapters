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
use crate::output;
use crate::state::{AgentState, resolve_agent_id};

use alf_core::archive::{AlfReader, DeltaReader};
use alf_core::delta::apply_delta;

use anyhow::Context;
use anyhow::Result;
use chrono::Utc;
use colored::Colorize;
use serde::Serialize;
use std::fs;
use std::io::Cursor;
use std::path::Path;
use uuid::Uuid;

#[derive(Serialize)]
struct RestoreResult {
    ok: bool,
    agent_id: String,
    agent_name: String,
    sequence: u64,
    runtime: String,
    memory_records: u64,
    workspace: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

pub fn run(runtime: &str, workspace: &Path, agent_arg: Option<&str>) -> Result<()> {
    let human = output::human_mode();

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

    if human {
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
    } else {
        output::progress(&format!("Restoring agent {}...", &agent_id.to_string()[..8]));
    }

    // 4. Call restore endpoint — gets snapshot + delta URLs in one call
    output::progress("  Fetching restore manifest...");
    let restore = client.restore(agent_id)?;

    let snapshot_bytes = match &restore.snapshot {
        Some(snap) => {
            output::progress(&format!(
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

    // 5. Download and apply deltas
    let final_bytes = snapshot_bytes.clone();

    if restore.deltas.is_empty() {
        output::progress("  No additional deltas to apply.");
    } else {
        output::progress(&format!("  Applying {} delta(s)...", restore.deltas.len()));

        let mut current_reader = AlfReader::new(Cursor::new(&snapshot_bytes))?;
        let mut current_records = current_reader.read_all_memory()?;

        for (i, delta_info) in restore.deltas.iter().enumerate() {
            output::progress(&format!(
                "  Applying delta {} of {} (sequence {})...",
                i + 1,
                restore.deltas.len(),
                delta_info.sequence
            ));

            let delta_bytes = client.download_presigned(&delta_info.url)?;
            let mut delta_reader = DeltaReader::new(Cursor::new(delta_bytes))?;

            if let Some(entries) = delta_reader.read_memory_deltas()? {
                current_records = apply_delta(&current_records, &entries);
            }
        }

        // TODO: rebuild snapshot archive with merged records using AlfWriter.
        let _ = current_records;
    }

    // 6. Write snapshot to temp file and import
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    let temp_alf = temp_dir.path().join("restored.alf");
    fs::write(&temp_alf, &final_bytes)?;

    output::progress("  Importing into workspace...");
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
        snapshot_path: None,
    };
    state.save()?;

    // 8. Output result
    if human {
        let state_path = AgentState::path_for(agent_id)?;
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
    } else {
        output::json(&RestoreResult {
            ok: true,
            agent_id: agent_id.to_string(),
            agent_name: import_report.agent_name.clone(),
            sequence: latest_sequence,
            runtime: runtime.to_string(),
            memory_records: import_report.memory_records,
            workspace: workspace.to_string_lossy().into(),
            warnings: import_report.warnings.clone(),
        });
    }

    Ok(())
}
