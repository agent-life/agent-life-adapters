//! `alf purge` — delete cloud sync data and agent registration for an agent.

use crate::adapter;
use crate::api_client::ApiClient;
use crate::config::Config;
use crate::output;
use crate::selector;
use crate::state::{local_base_path, AgentState};

use anyhow::Result;
use colored::Colorize;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize)]
struct PurgeResult {
    ok: bool,
    agent_id: String,
    deleted: bool,
    objects_removed: u32,
}

/// The agent's vault file if one exists — per-agent path first, then the
/// legacy install-scoped path (mapping-less hosts). Display-only.
fn existing_vault_path(agent_id: uuid::Uuid) -> Option<PathBuf> {
    let per_agent = crate::vault_key::default_vault_path(Some(agent_id)).ok()?;
    if per_agent.is_file() {
        return Some(per_agent);
    }
    let legacy = crate::vault_key::default_vault_path(None).ok()?;
    legacy.is_file().then_some(legacy)
}

pub fn run(runtime: &str, workspace_flag: Option<&Path>, agent_arg: Option<&str>) -> Result<()> {
    let human = output::human_mode();

    let adapt = adapter::get_adapter(runtime).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown runtime '{}'. Supported: {}",
            runtime,
            adapter::supported_runtimes()
        )
    })?;

    let mut config = Config::load()?;

    // Alias-or-id via the mapping; UUID passthrough; legacy sole-state-file
    // fallback when the mapping is empty (same rules as restore).
    let agent_id = selector::resolve_for_cloud_op(
        &mut config,
        adapt.as_ref(),
        runtime,
        workspace_flag,
        agent_arg,
    )?;

    // Workspace: -w flag → the agent's mapped workspace → [defaults].workspace.
    // Used for CLI consistency only; never modified.
    let workspace: PathBuf = match workspace_flag {
        Some(w) => w.to_path_buf(),
        None => match config.agents.iter().find(|a| a.alf_agent_id == agent_id) {
            Some(row) => PathBuf::from(&row.workspace),
            None => config.resolve_workspace(None)?,
        },
    };
    let workspace = workspace.as_path();

    if !workspace.exists() {
        anyhow::bail!(
            "Workspace directory does not exist: {}",
            workspace.display()
        );
    }

    let client = ApiClient::from_config(&config)?;

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
    // Previews are derived copies of this agent's history — decommissioning it
    // must not leave them (with any decrypted credentials) behind (MIN-12).
    crate::commands::restore::purge_previews(agent_id);
    let snapshot_path = local_base_path(agent_id)?;
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
        println!("  Point-in-time previews under ~/.alf/preview/ were removed.");
        println!("  The workspace on disk was not modified. Run `alf sync` to upload again.");
        // D7: purge never touches the vault — deleting the last ciphertext
        // copy right after deleting the cloud copy would be the worst moment.
        if let Some(vault_path) = existing_vault_path(agent_id) {
            println!(
                "  The local vault at {} was kept — purge never deletes secrets. \
                 Remove the file manually if you intend to destroy them.",
                vault_path.display()
            );
        }
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
