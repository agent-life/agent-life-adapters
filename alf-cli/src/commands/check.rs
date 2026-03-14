//! `alf check` — pre-flight environment diagnostic.
//!
//! Discovers the workspace, verifies resources, and reports readiness to sync.
//! This is the first command an agent should run.

use crate::api_client::ApiClient;
use crate::config::Config;
use crate::context;
use crate::output;

use anyhow::Result;
use colored::Colorize;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// JSON output types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct CheckResult {
    ok: bool,
    runtime: String,
    ready_to_sync: bool,
    workspace: WorkspaceInfo,
    resources: ResourceInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    openclaw: Option<OpenClawInfo>,
    alf: AlfInfo,
    issues: Vec<Issue>,
    suggestions: Vec<String>,
}

#[derive(Serialize)]
struct WorkspaceInfo {
    path: String,
    source: String, // "flag", "alf_config", "openclaw.json", "default"
    exists: bool,
    writable: bool,
}

#[derive(Serialize)]
struct ResourceInfo {
    soul_md: bool,
    identity_md: bool,
    agents_md: bool,
    user_md: bool,
    memory_md: bool,
    memory_dir: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    daily_logs: Option<DailyLogInfo>,
    active_context: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_files: Option<ProjectFileInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_id: Option<String>,
}

#[derive(Serialize)]
struct DailyLogInfo {
    count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest: Option<String>,
}

#[derive(Serialize)]
struct ProjectFileInfo {
    count: usize,
}

#[derive(Serialize)]
struct OpenClawInfo {
    config_found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace_configured: Option<String>,
}

#[derive(Serialize)]
struct AlfInfo {
    config_exists: bool,
    api_key_set: bool,
    agent_tracked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_synced_sequence: Option<u64>,
    service_reachable: bool,
}

#[derive(Serialize)]
struct Issue {
    severity: String, // "error", "warning", "info"
    code: String,
    message: String,
    fix: String,
}

// ---------------------------------------------------------------------------
// Workspace auto-discovery
// ---------------------------------------------------------------------------

struct ResolvedWorkspace {
    path: PathBuf,
    source: String,
    openclaw_configured_path: Option<String>,
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    { std::env::var_os("HOME").map(PathBuf::from) }
    #[cfg(windows)]
    { std::env::var_os("USERPROFILE").map(PathBuf::from) }
    #[cfg(not(any(unix, windows)))]
    { None }
}

fn resolve_workspace(
    flag: Option<&Path>,
    config: &Config,
) -> ResolvedWorkspace {
    // Priority 1: -w flag
    if let Some(ws) = flag {
        return ResolvedWorkspace {
            path: ws.to_path_buf(),
            source: "flag".into(),
            openclaw_configured_path: read_openclaw_workspace(),
        };
    }

    // Priority 2: defaults.workspace in ~/.alf/config.toml
    if let Some(ref ws) = config.defaults.workspace {
        if !ws.is_empty() {
            return ResolvedWorkspace {
                path: PathBuf::from(ws),
                source: "alf_config".into(),
                openclaw_configured_path: read_openclaw_workspace(),
            };
        }
    }

    let oc_path = read_openclaw_workspace();

    // Priority 3: agents.defaults.workspace in ~/.openclaw/openclaw.json
    if let Some(ref ws) = oc_path {
        return ResolvedWorkspace {
            path: PathBuf::from(ws),
            source: "openclaw.json".into(),
            openclaw_configured_path: oc_path,
        };
    }

    // Priority 4: ~/.openclaw/workspace (default)
    let default_path = home_dir()
        .map(|h| h.join(".openclaw").join("workspace"))
        .unwrap_or_else(|| PathBuf::from(".openclaw/workspace"));

    ResolvedWorkspace {
        path: default_path,
        source: "default".into(),
        openclaw_configured_path: oc_path,
    }
}

/// Read `agents.defaults.workspace` from `~/.openclaw/openclaw.json`.
fn read_openclaw_workspace() -> Option<String> {
    let home = home_dir()?;
    let config_path = home.join(".openclaw").join("openclaw.json");
    let content = fs::read_to_string(&config_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    json.get("agents")?
        .get("defaults")?
        .get("workspace")?
        .as_str()
        .map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// Resource checking
// ---------------------------------------------------------------------------

fn check_resources(ws: &Path) -> ResourceInfo {
    let soul_md = ws.join("SOUL.md").is_file();
    let identity_md = ws.join("IDENTITY.md").is_file();
    let agents_md = ws.join("AGENTS.md").is_file();
    let user_md = ws.join("USER.md").is_file();
    let memory_md = ws.join("MEMORY.md").is_file();
    let memory_dir_path = ws.join("memory");
    let memory_dir = memory_dir_path.is_dir();
    let active_context = memory_dir_path.join("active-context.md").is_file();

    let daily_logs = if memory_dir {
        let mut logs: Vec<String> = Vec::new();
        if let Ok(entries) = fs::read_dir(&memory_dir_path) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                // Daily logs match YYYY-MM-DD.md pattern
                if name.len() == 13 && name.ends_with(".md") && name.chars().nth(4) == Some('-') {
                    logs.push(name);
                }
            }
        }
        logs.sort();
        Some(DailyLogInfo {
            count: logs.len(),
            latest: logs.last().cloned(),
        })
    } else {
        None
    };

    let project_files = if memory_dir {
        let mut count = 0usize;
        if let Ok(entries) = fs::read_dir(&memory_dir_path) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("project-") && name.ends_with(".md") {
                    count += 1;
                }
            }
        }
        Some(ProjectFileInfo { count })
    } else {
        None
    };

    let agent_id = fs::read_to_string(ws.join(".alf-agent-id"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    ResourceInfo {
        soul_md,
        identity_md,
        agents_md,
        user_md,
        memory_md,
        memory_dir,
        daily_logs,
        active_context,
        project_files,
        agent_id,
    }
}

// ---------------------------------------------------------------------------
// Issue collection
// ---------------------------------------------------------------------------

fn collect_issues(
    ws: &WorkspaceInfo,
    resources: &ResourceInfo,
    alf: &AlfInfo,
    resolved: &ResolvedWorkspace,
) -> Vec<Issue> {
    let mut issues = Vec::new();

    if !ws.exists {
        issues.push(Issue {
            severity: "error".into(),
            code: "workspace_not_found".into(),
            message: format!("Workspace directory not found at {}", ws.path),
            fix: "Pass the correct workspace path: alf check -r openclaw -w /path/to/workspace".into(),
        });
        return issues; // no point checking resources if workspace doesn't exist
    }

    if !ws.writable {
        issues.push(Issue {
            severity: "warning".into(),
            code: "workspace_not_writable".into(),
            message: format!("Workspace exists but is not writable: {}", ws.path),
            fix: "Check file permissions on the workspace directory".into(),
        });
    }

    // Check if workspace is essentially empty (no .md files in root)
    let has_any_md = resources.soul_md
        || resources.identity_md
        || resources.agents_md
        || resources.user_md
        || resources.memory_md;
    if !has_any_md {
        issues.push(Issue {
            severity: "warning".into(),
            code: "workspace_empty".into(),
            message: "No markdown files found in workspace root".into(),
            fix: "Workspace may not be initialized — check the path".into(),
        });
    }

    if !resources.soul_md {
        issues.push(Issue {
            severity: "warning".into(),
            code: "no_soul_md".into(),
            message: "SOUL.md not found in workspace".into(),
            fix: "Agent has no persona file; export will use a fallback name".into(),
        });
    }

    let has_memory_content = resources.memory_md
        || resources.memory_dir
        && resources.daily_logs.as_ref().is_some_and(|d| d.count > 0);
    if !has_memory_content {
        issues.push(Issue {
            severity: "warning".into(),
            code: "no_memory_content".into(),
            message: "No MEMORY.md and no daily logs in memory/ directory".into(),
            fix: "Nothing to sync — agent has no memories yet".into(),
        });
    }

    if resources.memory_dir {
        if let Some(ref dl) = resources.daily_logs {
            if dl.count == 0 {
                issues.push(Issue {
                    severity: "warning".into(),
                    code: "memory_dir_empty".into(),
                    message: "memory/ directory exists but has no daily log files".into(),
                    fix: "No daily logs yet — memories will accumulate over time".into(),
                });
            }
        }
    }

    if !alf.api_key_set {
        issues.push(Issue {
            severity: "error".into(),
            code: "no_api_key".into(),
            message: "No API key configured".into(),
            fix: "Run: alf login --key <your-api-key>".into(),
        });
    }

    if alf.api_key_set && !alf.service_reachable {
        issues.push(Issue {
            severity: "error".into(),
            code: "service_unreachable".into(),
            message: "API endpoint not responding".into(),
            fix: "Check network connectivity and API URL in ~/.alf/config.toml".into(),
        });
    }

    if resolved.openclaw_configured_path.is_none() {
        let home = home_dir().unwrap_or_default();
        let oc_config = home.join(".openclaw").join("openclaw.json");
        if !oc_config.exists() {
            issues.push(Issue {
                severity: "info".into(),
                code: "openclaw_config_not_found".into(),
                message: "~/.openclaw/openclaw.json not found".into(),
                fix: "OpenClaw may not be installed, or uses a non-standard location".into(),
            });
        }
    }

    // Workspace mismatch: -w differs from openclaw.json configured path
    if ws.source == "flag" {
        if let Some(ref oc_ws) = resolved.openclaw_configured_path {
            let flag_canonical = PathBuf::from(&ws.path);
            let oc_canonical = PathBuf::from(oc_ws);
            if flag_canonical != oc_canonical {
                issues.push(Issue {
                    severity: "warning".into(),
                    code: "workspace_mismatch".into(),
                    message: format!(
                        "-w path ({}) differs from openclaw.json configured path ({})",
                        ws.path, oc_ws
                    ),
                    fix: "May be intentional; noting for awareness".into(),
                });
            }
        }
    }

    issues
}

fn build_suggestions(result: &CheckResult) -> Vec<String> {
    let mut suggestions = Vec::new();

    if result.ready_to_sync {
        suggestions.push(format!(
            "Everything looks good. Run: alf sync -r {} -w {}",
            result.runtime, result.workspace.path
        ));
    } else {
        if result.issues.iter().any(|i| i.code == "no_api_key") {
            suggestions.push("Get an API key at https://agent-life.ai/settings/api-keys".into());
        }
        if result.issues.iter().any(|i| i.code == "workspace_not_found") {
            suggestions.push(
                "The workspace path may be customized in ~/.openclaw/openclaw.json under agents.defaults.workspace".into()
            );
        }
    }

    suggestions
}

// ---------------------------------------------------------------------------
// Command entry point
// ---------------------------------------------------------------------------

pub fn run(runtime: &str, workspace_arg: Option<&Path>) -> Result<()> {
    let human = output::human_mode();
    let config = Config::load()?;

    output::progress(&format!("Checking {} environment...", runtime));

    // Resolve workspace
    let resolved = resolve_workspace(workspace_arg, &config);

    let ws_path = &resolved.path;
    let ws_exists = ws_path.is_dir();
    let ws_writable = if ws_exists {
        // Test writability by checking if we can access metadata
        fs::metadata(ws_path)
            .map(|m| !m.permissions().readonly())
            .unwrap_or(false)
    } else {
        false
    };

    let workspace_info = WorkspaceInfo {
        path: ws_path.to_string_lossy().into(),
        source: resolved.source.clone(),
        exists: ws_exists,
        writable: ws_writable,
    };

    // Check resources
    let resources = if ws_exists {
        check_resources(ws_path)
    } else {
        ResourceInfo {
            soul_md: false,
            identity_md: false,
            agents_md: false,
            user_md: false,
            memory_md: false,
            memory_dir: false,
            daily_logs: None,
            active_context: false,
            project_files: None,
            agent_id: None,
        }
    };

    // OpenClaw info
    let openclaw = if runtime == "openclaw" {
        Some(OpenClawInfo {
            config_found: resolved.openclaw_configured_path.is_some()
                || home_dir()
                    .map(|h| h.join(".openclaw").join("openclaw.json").exists())
                    .unwrap_or(false),
            workspace_configured: resolved.openclaw_configured_path.clone(),
        })
    } else {
        None
    };

    // ALF state
    let status = context::gather_status()?;
    let api_key_set = status.api_key_set;
    let agent_tracked = !status.agents.is_empty();
    let last_synced_sequence = status.agents.first().map(|a| a.last_synced_sequence);

    let service_reachable = if api_key_set && agent_tracked {
        let client = ApiClient::from_config(&config).ok();
        client
            .and_then(|c| {
                status
                    .agents
                    .first()
                    .and_then(|a| c.get_agent(a.agent_id).ok())
            })
            .is_some()
    } else if api_key_set {
        // No agents tracked yet, but key is set — try a simple connectivity check
        // by attempting to create a client (validates config)
        ApiClient::from_config(&config).is_ok()
    } else {
        false
    };

    let alf_info = AlfInfo {
        config_exists: status.config_exists,
        api_key_set,
        agent_tracked,
        last_synced_sequence,
        service_reachable,
    };

    // Collect issues
    let issues = collect_issues(&workspace_info, &resources, &alf_info, &resolved);

    let has_errors = issues.iter().any(|i| i.severity == "error");
    let ready_to_sync = !has_errors && ws_exists;

    let mut result = CheckResult {
        ok: !has_errors,
        runtime: runtime.into(),
        ready_to_sync,
        workspace: workspace_info,
        resources,
        openclaw,
        alf: alf_info,
        issues,
        suggestions: Vec::new(),
    };
    result.suggestions = build_suggestions(&result);

    if human {
        print_human(&result);
    } else {
        output::json(&result);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Human-readable output
// ---------------------------------------------------------------------------

fn print_human(result: &CheckResult) {
    if result.ready_to_sync {
        println!("{} Ready to sync", "✓".green().bold());
    } else {
        println!("{} Not ready to sync", "✗".red().bold());
    }
    println!();

    println!("  Runtime:   {}", result.runtime);
    println!(
        "  Workspace: {} (source: {})",
        result.workspace.path, result.workspace.source
    );
    println!(
        "  Exists:    {}  Writable: {}",
        if result.workspace.exists { "yes" } else { "no" },
        if result.workspace.writable { "yes" } else { "no" }
    );
    println!();

    println!("  Resources:");
    println!("    SOUL.md:     {}", yn(result.resources.soul_md));
    println!("    IDENTITY.md: {}", yn(result.resources.identity_md));
    println!("    AGENTS.md:   {}", yn(result.resources.agents_md));
    println!("    USER.md:     {}", yn(result.resources.user_md));
    println!("    MEMORY.md:   {}", yn(result.resources.memory_md));
    println!("    memory/:     {}", yn(result.resources.memory_dir));
    if let Some(ref dl) = result.resources.daily_logs {
        println!(
            "    Daily logs:  {} (latest: {})",
            dl.count,
            dl.latest.as_deref().unwrap_or("none")
        );
    }
    println!();

    println!("  ALF:");
    println!("    Config:     {}", yn(result.alf.config_exists));
    println!("    API key:    {}", yn(result.alf.api_key_set));
    println!("    Tracked:    {}", yn(result.alf.agent_tracked));
    println!("    Service:    {}", yn(result.alf.service_reachable));
    println!();

    if !result.issues.is_empty() {
        println!("  Issues:");
        for issue in &result.issues {
            let severity_label = match issue.severity.as_str() {
                "error" => "ERROR".red().bold().to_string(),
                "warning" => "WARN".yellow().bold().to_string(),
                _ => "INFO".dimmed().to_string(),
            };
            println!("    [{}] {} ({})", severity_label, issue.message, issue.code);
            println!("      Fix: {}", issue.fix);
        }
        println!();
    }

    if !result.suggestions.is_empty() {
        println!("  Suggestions:");
        for s in &result.suggestions {
            println!("    • {s}");
        }
    }
}

fn yn(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn resolve_workspace_flag_wins() {
        let config = Config::default();
        let flag_path = PathBuf::from("/custom/workspace");
        let resolved = resolve_workspace(Some(&flag_path), &config);

        assert_eq!(resolved.path, PathBuf::from("/custom/workspace"));
        assert_eq!(resolved.source, "flag");
    }

    #[test]
    fn resolve_workspace_alf_config_second() {
        let mut config = Config::default();
        config.defaults.workspace = Some("/alf-configured/workspace".into());
        let resolved = resolve_workspace(None, &config);

        assert_eq!(resolved.path, PathBuf::from("/alf-configured/workspace"));
        assert_eq!(resolved.source, "alf_config");
    }

    #[test]
    fn resolve_workspace_openclaw_json_third() {
        // Uses the context::tests::HOME_LOCK via serial execution
        let _guard = crate::context::tests::HOME_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());

        let oc_dir = tmp.path().join(".openclaw");
        fs::create_dir_all(&oc_dir).unwrap();
        fs::write(
            oc_dir.join("openclaw.json"),
            r#"{"agents":{"defaults":{"workspace":"/from/openclaw"}}}"#,
        )
        .unwrap();

        let config = Config::default();
        let resolved = resolve_workspace(None, &config);

        // Restore HOME before asserting
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }

        assert_eq!(resolved.path, PathBuf::from("/from/openclaw"));
        assert_eq!(resolved.source, "openclaw.json");
    }

    #[test]
    fn resolve_workspace_default_fallback() {
        let _guard = crate::context::tests::HOME_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());

        let config = Config::default();
        let resolved = resolve_workspace(None, &config);

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }

        assert_eq!(
            resolved.path,
            tmp.path().join(".openclaw").join("workspace")
        );
        assert_eq!(resolved.source, "default");
    }

    #[test]
    fn check_resources_full_workspace() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path();

        fs::write(ws.join("SOUL.md"), "# Agent").unwrap();
        fs::write(ws.join("IDENTITY.md"), "# Identity").unwrap();
        fs::write(ws.join("AGENTS.md"), "# Agents").unwrap();
        fs::write(ws.join("USER.md"), "# User").unwrap();
        fs::write(ws.join("MEMORY.md"), "# Memory").unwrap();

        let memory_dir = ws.join("memory");
        fs::create_dir_all(&memory_dir).unwrap();
        fs::write(memory_dir.join("2026-03-12.md"), "log").unwrap();
        fs::write(memory_dir.join("2026-03-13.md"), "log").unwrap();
        fs::write(memory_dir.join("active-context.md"), "ctx").unwrap();
        fs::write(memory_dir.join("project-foo.md"), "proj").unwrap();
        fs::write(ws.join(".alf-agent-id"), "abc-123").unwrap();

        let resources = check_resources(ws);

        assert!(resources.soul_md);
        assert!(resources.identity_md);
        assert!(resources.agents_md);
        assert!(resources.user_md);
        assert!(resources.memory_md);
        assert!(resources.memory_dir);
        assert!(resources.active_context);
        assert_eq!(resources.daily_logs.as_ref().unwrap().count, 2);
        assert_eq!(
            resources.daily_logs.as_ref().unwrap().latest.as_deref(),
            Some("2026-03-13.md")
        );
        assert_eq!(resources.project_files.as_ref().unwrap().count, 1);
        assert_eq!(resources.agent_id.as_deref(), Some("abc-123"));
    }

    #[test]
    fn check_resources_empty_workspace() {
        let tmp = TempDir::new().unwrap();
        let resources = check_resources(tmp.path());

        assert!(!resources.soul_md);
        assert!(!resources.memory_dir);
        assert!(resources.daily_logs.is_none());
        assert!(resources.agent_id.is_none());
    }
}
