//! `alf purge` — delete cloud sync data and agent registration for an agent.

use crate::adapter;
use crate::api_client::ApiClient;
use crate::config::Config;
use crate::output;
use crate::state::{resolve_agent_id, AgentState};

use anyhow::Result;
use colored::Colorize;
use serde::Serialize;
use std::fs;
use std::path::Path;

#[derive(Serialize)]
struct PurgeResult {
    ok: bool,
    agent_id: String,
    deleted: bool,
    objects_removed: u32,
}

pub fn run(runtime: &str, workspace: &Path, agent_arg: Option<&str>) -> Result<()> {
    let human = output::human_mode();

    let _ = adapter::get_adapter(runtime).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown runtime '{}'. Supported: {}",
            runtime,
            adapter::supported_runtimes()
        )
    })?;

    if !workspace.exists() {
        anyhow::bail!(
            "Workspace directory does not exist: {}",
            workspace.display()
        );
    }

    let config = Config::load()?;
    let client = ApiClient::from_config(&config)?;

    let agent_id = resolve_agent_id(agent_arg)?;

    if human {
        println!(
            "{} Purging cloud sync data for agent {}...",
            "▸".blue().bold(),
            &agent_id.to_string()[..8]
        );
        println!("  Agent:     {agent_id}");
        println!("  Runtime:   {runtime}");
        println!("  Workspace: {}", workspace.display());
        println!();
    } else {
        output::progress(&format!(
            "Purging agent {} from service...",
            &agent_id.to_string()[..8]
        ));
    }

    let del = client.delete_agent(agent_id)?;

    AgentState::delete(agent_id)?;
    let snapshot_path = AgentState::state_dir()?.join(format!("{agent_id}-snapshot.alf"));
    if snapshot_path.exists() {
        fs::remove_file(&snapshot_path).map_err(|e| {
            anyhow::anyhow!(
                "Removed agent state but failed to delete snapshot {}: {}",
                snapshot_path.display(),
                e
            )
        })?;
    }

    if human {
        println!(
            "{} Purge complete — {} object(s) removed from storage",
            "✓".green().bold(),
            del.objects_removed
        );
        println!();
        println!("  Local sync state under ~/.alf/state/ was reset for this agent.");
        println!("  The workspace on disk was not modified. Run `alf sync` to upload again.");
        println!();
    } else {
        output::json(&PurgeResult {
            ok: true,
            agent_id: agent_id.to_string(),
            deleted: del.deleted,
            objects_removed: del.objects_removed,
        });
    }

    Ok(())
}
