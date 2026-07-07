//! `alf agents` — list the discovered-agent mapping and manage which agents
//! are enabled for sync.
//!
//! The mapping (`[[agents]]` in ~/.alf/config.toml) is maintained by
//! `alf check` / selector first-contact; this command is the explicit
//! enable/disable surface (discovery never flips `enabled`).
//!
//! Rows are runtime-tagged. Without `-r` the list spans every runtime and
//! enable/disable resolve the name across all runtimes (erroring when the
//! alias is ambiguous) — the remedies sync emits (`alf agents enable <alias>`)
//! must work whatever the [defaults].runtime is.

use crate::config::{AgentEntry, Config};
use crate::errors::{codes, CliError};
use crate::output;
use crate::selector;
use crate::state::{local_base_exists, AgentState};

use anyhow::Result;
use colored::Colorize;
use schemars::JsonSchema;
use serde::Serialize;
use uuid::Uuid;

/// The `alf agents list` result. Also the `alf_agents_list` MCP tool result
/// (hence `JsonSchema`).
#[derive(Serialize, JsonSchema)]
pub(crate) struct ListResult {
    ok: bool,
    /// The `-r` filter when one was given; absent ⇒ all runtimes.
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime: Option<String>,
    mapping_path: String,
    agents: Vec<AgentListRow>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct AgentListRow {
    runtime: String,
    runtime_agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_agent_id: Option<String>,
    alf_agent_id: Uuid,
    workspace: String,
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_synced_sequence: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_synced_at: Option<String>,
    snapshot_exists: bool,
}

#[derive(Serialize)]
struct ToggleResult {
    ok: bool,
    runtime: String,
    runtime_agent: String,
    alf_agent_id: Uuid,
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'static str>,
}

/// Build the mapping-rows-joined-with-sync-state listing — no stdout. Shared by
/// the CLI [`list`] and the MCP `alf_agents_list` tool.
pub(crate) fn list_result(runtime_filter: Option<&str>) -> Result<ListResult> {
    let config = Config::load()?;
    let rows: Vec<&AgentEntry> = match runtime_filter {
        Some(r) => config.agents_for_runtime(r),
        None => config.agents.iter().collect(),
    };
    if rows.is_empty() {
        let cause = match runtime_filter {
            Some(r) if !config.agents.is_empty() => {
                format!("No agents are mapped for runtime '{r}'.")
            }
            _ => "No agents are mapped yet.".to_string(),
        };
        return Err(CliError {
            code: codes::NO_AGENTS,
            cause,
            remedy: "Run 'alf check' to discover agents.".into(),
        }
        .into());
    }

    let mut agents = Vec::with_capacity(rows.len());
    for row in rows {
        let state = AgentState::load(row.alf_agent_id)
            .unwrap_or_else(|_| AgentState::new(row.alf_agent_id));
        agents.push(AgentListRow {
            runtime: row_runtime(row, &config),
            runtime_agent: row.runtime_agent.clone(),
            runtime_agent_id: row.runtime_agent_id.clone(),
            alf_agent_id: row.alf_agent_id,
            workspace: row.workspace.clone(),
            enabled: row.enabled,
            last_synced_sequence: state.last_synced_sequence,
            last_synced_at: state.last_synced_at.map(|dt| dt.to_rfc3339()),
            snapshot_exists: local_base_exists(row.alf_agent_id)?,
        });
    }

    Ok(ListResult {
        ok: true,
        runtime: runtime_filter.map(str::to_string),
        mapping_path: Config::path()?.to_string_lossy().into_owned(),
        agents,
    })
}

/// `alf agents` / `alf agents list` — mapping rows joined with sync state.
/// `runtime_filter` scopes to one runtime; `None` lists every row.
pub fn list(runtime_filter: Option<&str>) -> Result<()> {
    let result = list_result(runtime_filter)?;
    let agents = &result.agents;

    if output::human_mode() {
        match runtime_filter {
            Some(r) => println!("Agents ({r}):"),
            None => println!("Agents:"),
        }
        for a in agents {
            println!(
                "  {}  [{}]  {}  {}  seq={}  last_synced={}  snapshot={}",
                a.runtime_agent,
                a.runtime,
                a.alf_agent_id,
                if a.enabled { "enabled" } else { "disabled" },
                a.last_synced_sequence
                    .map(|n| n.to_string())
                    .as_deref()
                    .unwrap_or("-"),
                a.last_synced_at.as_deref().unwrap_or("(never)"),
                if a.snapshot_exists { "yes" } else { "no" }
            );
            println!("      workspace: {}", a.workspace);
        }
        println!();
        println!("Mapping: {}", result.mapping_path);
    } else {
        output::json(&result);
    }
    Ok(())
}

/// `alf agents enable <agent>` — idempotent; registration stays lazy.
pub fn enable(runtime_filter: Option<&str>, agent: &str) -> Result<()> {
    toggle(runtime_filter, agent, true)
}

/// `alf agents disable <agent>` — idempotent; cloud archive and local state
/// under ~/.alf/state/ are kept.
pub fn disable(runtime_filter: Option<&str>, agent: &str) -> Result<()> {
    toggle(runtime_filter, agent, false)
}

fn toggle(runtime_filter: Option<&str>, agent: &str, enabled: bool) -> Result<()> {
    let mut config = Config::load()?;

    let row = match runtime_filter {
        Some(r) => {
            if config.find_agent(r, agent).is_none() {
                return Err(selector::agent_not_found(r, agent, &config).into());
            }
            config.set_agent_enabled(r, agent, enabled)?
        }
        None => {
            let id = resolve_any_runtime(&config, agent)?;
            let row = config
                .agents
                .iter_mut()
                .find(|a| a.alf_agent_id == id)
                .expect("row found above");
            row.enabled = enabled;
            row.clone()
        }
    };
    config.save()?;

    let note = enabled.then_some(
        "Registration is lazy: this agent registers with the service on its first alf sync.",
    );
    if output::human_mode() {
        println!(
            "{} Agent '{}' {}",
            "✓".green().bold(),
            row.runtime_agent,
            if enabled { "enabled" } else { "disabled" }
        );
        if let Some(note) = note {
            println!("  {note}");
        }
    } else {
        output::json(&ToggleResult {
            ok: true,
            runtime: row_runtime(&row, &config),
            runtime_agent: row.runtime_agent,
            alf_agent_id: row.alf_agent_id,
            enabled,
            note,
        });
    }
    Ok(())
}

/// Resolve a name across ALL runtimes' rows: a UUID matches `alf_agent_id`,
/// an alias matches `runtime_agent`. Exactly one match, or a coded error
/// (ambiguous ⇒ ask for `-r`).
fn resolve_any_runtime(config: &Config, agent: &str) -> Result<Uuid> {
    let matches: Vec<&AgentEntry> = match Uuid::parse_str(agent) {
        Ok(id) => config
            .agents
            .iter()
            .filter(|a| a.alf_agent_id == id)
            .collect(),
        Err(_) => config
            .agents
            .iter()
            .filter(|a| a.runtime_agent == agent)
            .collect(),
    };
    match matches.len() {
        1 => Ok(matches[0].alf_agent_id),
        0 => {
            let known = if config.agents.is_empty() {
                "(none)".to_string()
            } else {
                config
                    .agents
                    .iter()
                    .map(|r| {
                        format!(
                            "{} [{}] ({})",
                            r.runtime_agent,
                            row_runtime(r, config),
                            if r.enabled { "enabled" } else { "disabled" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            Err(CliError {
                code: codes::AGENT_NOT_FOUND,
                cause: format!("No agent named '{agent}'. Known agents: {known}."),
                remedy: "Run 'alf agents' to list agents, or 'alf check' to re-discover \
                         this install."
                    .into(),
            }
            .into())
        }
        _ => {
            let runtimes = matches
                .iter()
                .map(|r| row_runtime(r, config))
                .collect::<Vec<_>>()
                .join(", ");
            Err(CliError {
                code: codes::AGENT_SELECTION_AMBIGUOUS,
                cause: format!("Agent '{agent}' exists for multiple runtimes ({runtimes})."),
                remedy: format!("Re-run with -r, e.g. 'alf agents -r {} enable {agent}'.", {
                    matches
                        .first()
                        .map(|r| row_runtime(r, config))
                        .unwrap_or_default()
                }),
            }
            .into())
        }
    }
}

/// A row's runtime for display: its own tag, else the config default
/// (legacy/hand-written rows have no tag).
fn row_runtime(row: &AgentEntry, config: &Config) -> String {
    row.runtime
        .clone()
        .unwrap_or_else(|| config.defaults.runtime.clone())
}
