//! Help subcommand: overview, status, files, troubleshoot, and per-command help.

use anyhow::Result;
use serde::Serialize;
use std::process::Command;

use crate::api_client::ApiClient;
use crate::config::Config;
use crate::context;

/// JSON-serializable status (paths as strings).
#[derive(Serialize)]
struct StatusJson {
    config_path: String,
    config_exists: bool,
    api_key_set: bool,
    state_dir: String,
    state_dir_exists: bool,
    /// True if API key is set and at least one tracked agent is reachable on the service.
    service_reachable: bool,
    agents: Vec<AgentJson>,
    /// Per-agent service status (only present when API key set and we queried).
    agent_service_status: Vec<AgentServiceStatusJson>,
}

#[derive(Serialize)]
struct AgentJson {
    agent_id: String,
    last_synced_sequence: u64,
    last_synced_at: Option<String>,
    snapshot_exists: bool,
}

#[derive(Serialize)]
struct AgentServiceStatusJson {
    agent_id: String,
    online: bool,
    name: Option<String>,
    server_latest_sequence: Option<u64>,
    error: Option<String>,
}

pub fn run(topic: Option<&str>, json: bool) -> Result<()> {
    let topic = topic.unwrap_or("overview").trim().to_lowercase();
    let topic = if topic.is_empty() { "overview" } else { topic.as_str() };

    if json && topic != "status" {
        eprintln!("--json is only valid for 'alf help status'");
        std::process::exit(1);
    }

    match topic {
        "overview" => print_overview(),
        "status" => print_status(json),
        "files" => print_files(),
        "troubleshoot" => print_troubleshoot(),
        "export" | "import" | "validate" | "sync" | "restore" | "login" => {
            delegate_command_help(topic)
        }
        _ => {
            eprintln!("Unknown topic: {}", topic);
            eprintln!("Topics: overview, status, files, troubleshoot, export, import, validate, sync, restore, login");
            std::process::exit(1);
        }
    }
}

/// If API key is set and we have tracked agents, call GET /agents/:id for each and return
/// (service_reachable, per-agent status). Otherwise (false, empty).
fn fetch_service_status(
    config: &Config,
    agents: &[context::AgentSummary],
) -> (bool, Vec<AgentServiceStatusJson>) {
    if agents.is_empty() || config.service.api_key.is_empty() {
        return (false, Vec::new());
    }
    let client = match ApiClient::from_config(config) {
        Ok(c) => c,
        Err(_) => return (false, Vec::new()),
    };
    let mut statuses = Vec::with_capacity(agents.len());
    for a in agents {
        match client.get_agent(a.agent_id) {
            Ok(info) => statuses.push(AgentServiceStatusJson {
                agent_id: a.agent_id.to_string(),
                online: true,
                name: Some(info.name),
                server_latest_sequence: Some(info.latest_sequence),
                error: None,
            }),
            Err(e) => statuses.push(AgentServiceStatusJson {
                agent_id: a.agent_id.to_string(),
                online: false,
                name: None,
                server_latest_sequence: None,
                error: Some(e.to_string()),
            }),
        }
    }
    let service_reachable = statuses.iter().any(|s| s.online);
    (service_reachable, statuses)
}

fn print_overview() -> Result<()> {
    let status = context::gather_status()?;

    println!("alf — Agent Life Format");
    println!("  Portable backup, sync, and migration for AI agents.");
    println!();
    println!("Commands:");
    println!("  export     Export an agent workspace to an .alf archive");
    println!("  import     Import an .alf archive into an agent workspace");
    println!("  validate   Validate an .alf archive against the ALF specification");
    println!("  sync       Incremental sync to the cloud");
    println!("  restore    Download and restore from the cloud");
    println!("  login      Authenticate with the agent-life service");
    println!("  help       Show this help (alf help [topic])");
    println!();
    println!("Where alf stores data:");
    println!("  Config:  {}", status.config_path.display());
    println!("  State:   {}", status.state_dir.display());
    println!();
    println!("Current status:");
    println!(
        "  Config file:  {}",
        if status.config_exists { "present" } else { "missing" }
    );
    println!(
        "  API key set:  {}",
        if status.api_key_set { "yes" } else { "no" }
    );
    println!("  Tracked agents: {}", status.agents.len());
    println!();
    println!("Run 'alf help status' for full environment details.");
    println!("Run 'alf help files' for directory layout.");
    println!("Run 'alf help troubleshoot' for common fixes.");
    println!();
    println!("Agent-friendly: use 'alf help status --json' for machine-readable status (config path, api_key set, tracked agents).");
    println!();
    println!("Documentation: https://agent-life.ai");
    println!("Specification: https://github.com/agent-life/agent-life-data-format");
    Ok(())
}

fn print_status(json_output: bool) -> Result<()> {
    let status = context::gather_status()?;
    let config = Config::load()?;
    let (service_reachable, agent_service_status) = fetch_service_status(&config, &status.agents);

    if json_output {
        let json_status = StatusJson {
            config_path: status.config_path.to_string_lossy().into_owned(),
            config_exists: status.config_exists,
            api_key_set: status.api_key_set,
            state_dir: status.state_dir.to_string_lossy().into_owned(),
            state_dir_exists: status.state_dir_exists,
            service_reachable,
            agents: status
                .agents
                .into_iter()
                .map(|a| AgentJson {
                    agent_id: a.agent_id.to_string(),
                    last_synced_sequence: a.last_synced_sequence,
                    last_synced_at: a.last_synced_at,
                    snapshot_exists: a.snapshot_exists,
                })
                .collect(),
            agent_service_status,
        };
        println!("{}", serde_json::to_string_pretty(&json_status)?);
        return Ok(());
    }

    println!("Config:");
    println!("  Path:   {}", status.config_path.display());
    println!("  Exists: {}", status.config_exists);
    println!("  API key set: {}", status.api_key_set);
    println!();
    println!("State directory:");
    println!("  Path:   {}", status.state_dir.display());
    println!("  Exists: {}", status.state_dir_exists);
    println!();

    if status.agents.is_empty() {
        println!("Tracked agents: (none)");
    } else {
        println!("Tracked agents:");
        for a in &status.agents {
            println!("  {}  sequence={}  last_synced={}  snapshot={}",
                a.agent_id,
                a.last_synced_sequence,
                a.last_synced_at.as_deref().unwrap_or("(never)"),
                if a.snapshot_exists { "yes" } else { "no" }
            );
        }
    }

    println!();
    println!("Service (agent-life API):");
    if !status.api_key_set {
        println!("  Status: not checked (no API key)");
    } else if status.agents.is_empty() {
        println!("  Status: not checked (no tracked agents)");
    } else if agent_service_status.is_empty() {
        println!("  Status: unreachable (could not create client)");
    } else {
        println!(
            "  Status: {}",
            if service_reachable { "reachable" } else { "unreachable or auth failed" }
        );
        for s in &agent_service_status {
            if s.online {
                println!(
                    "  {}  online  name={}  server_sequence={}",
                    s.agent_id,
                    s.name.as_deref().unwrap_or("—"),
                    s.server_latest_sequence.map(|n| n.to_string()).as_deref().unwrap_or("—")
                );
            } else {
                println!("  {}  offline  error={}", s.agent_id, s.error.as_deref().unwrap_or("unknown"));
            }
        }
    }

    println!();
    print_next_steps(&status);
    println!();
    println!("Use 'alf help status --json' for the same information in JSON (for agents and scripts).");
    Ok(())
}

fn print_next_steps(status: &context::StatusSummary) {
    let mut steps = Vec::<&str>::new();
    if !status.config_exists || !status.api_key_set {
        steps.push("Run 'alf login' to set an API key.");
    }
    if status.agents.is_empty() {
        steps.push("Run 'alf sync -r <runtime> -w <workspace>' to track an agent.");
    }
    if status.agents.len() > 1 {
        steps.push("Multiple agents tracked; use '-a <agent-id>' for restore. List IDs above.");
    }
    if steps.is_empty() {
        println!("Next steps: You're set. Use 'alf sync' or 'alf restore' as needed.");
    } else {
        println!("Suggested next steps:");
        for s in steps {
            println!("  {}", s);
        }
    }
}

fn print_files() -> Result<()> {
    let status = context::gather_status()?;

    println!("alf file layout (no sensitive data):");
    println!();
    println!("  {}          Config directory", status.config_dir.display());
    println!("  {}/config.toml   User config (API URL, API key)", status.config_dir.display());
    println!("  {}/state/        Per-agent sync state", status.config_dir.display());
    println!("  {}/state/{{agent-id}}.toml       Agent sync cursor (sequence, last sync time)", status.config_dir.display());
    println!("  {}/state/{{agent-id}}-snapshot.alf   Last exported snapshot (delta base)", status.config_dir.display());
    println!();
    println!("Config and state are created when you run 'alf login' and 'alf sync'.");
    Ok(())
}

fn print_troubleshoot() -> Result<()> {
    println!("Common issues and fixes:");
    println!();
    println!("  No API key");
    println!("    Run 'alf login' or set service.api_key in ~/.alf/config.toml.");
    println!();
    println!("  No previous snapshot / state");
    println!("    Run a full sync first: 'alf sync -r <runtime> -w <workspace>'.");
    println!();
    println!("  Multiple agents — which to restore?");
    println!("    Pass '-a <agent-id>'. List IDs with 'alf help status'.");
    println!();
    println!("  Workspace not found");
    println!("    Ensure the path exists and is the agent workspace (e.g. contains SOUL.md or config.toml).");
    println!();
    println!("  Unknown runtime");
    println!("    Supported runtimes: openclaw, zeroclaw.");
    Ok(())
}

fn delegate_command_help(cmd: &str) -> Result<()> {
    let exe = std::env::current_exe()
        .unwrap_or_else(|_| std::path::PathBuf::from("alf"));
    let status = Command::new(&exe)
        .arg(cmd)
        .arg("--help")
        .status()?;
    std::process::exit(status.code().unwrap_or(1));
}
