//! `alf check` — pre-flight environment diagnostic.
//!
//! Discovers the workspace, verifies resources, and reports readiness to sync.
//! This is the first command an agent should run.

use crate::api_client::{AgentInfo, ApiClient};
use crate::config::Config;
use crate::context;
use crate::output;
use crate::selector;
use crate::vault_migrate::{self, MigrationOutcome};

use alf_core::CredentialsDocument;
use anyhow::Result;
use colored::Colorize;
use schemars::JsonSchema;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// JSON output types
// ---------------------------------------------------------------------------

#[derive(Serialize, JsonSchema)]
pub(crate) struct CheckResult {
    version: String,
    ok: bool,
    runtime: String,
    ready_to_sync: bool,
    workspace: WorkspaceInfo,
    resources: ResourceInfo,
    alfignore: AlfignoreInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    openclaw: Option<OpenClawInfo>,
    alf: AlfInfo,
    /// Discovered-agent mapping section (WP0). Absent for unknown runtimes.
    #[serde(skip_serializing_if = "Option::is_none")]
    agents: Option<AgentsSection>,
    env: EnvInfo,
    vault: VaultInfo,
    issues: Vec<Issue>,
    suggestions: Vec<String>,
}

/// Outcome of discovery + reconcile against the `[[agents]]` mapping.
#[derive(Serialize, JsonSchema)]
struct AgentsSection {
    first_run: bool,
    agents: Vec<AgentRow>,
    /// Aliases discovered this run that were not in the mapping.
    new: Vec<String>,
    /// Aliases in the mapping that were not discovered this run.
    removed: Vec<String>,
    drift: Vec<crate::discovery::DriftWarning>,
}

#[derive(Serialize, JsonSchema)]
struct AgentRow {
    runtime_agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_agent_id: Option<String>,
    alf_agent_id: String,
    workspace: String,
    enabled: bool,
    status: &'static str,
}

#[derive(Serialize, JsonSchema)]
struct AlfignoreInfo {
    /// Whether a `.alfignore` file exists at the workspace root.
    present: bool,
}

#[derive(Serialize, JsonSchema)]
struct WorkspaceInfo {
    path: String,
    source: String, // "flag", "alf_config", "openclaw.json", "default"
    exists: bool,
    writable: bool,
}

#[derive(Serialize, JsonSchema)]
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

#[derive(Serialize, JsonSchema)]
struct DailyLogInfo {
    count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest: Option<String>,
}

#[derive(Serialize, JsonSchema)]
struct ProjectFileInfo {
    count: usize,
}

#[derive(Serialize, JsonSchema)]
struct OpenClawInfo {
    config_found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace_configured: Option<String>,
}

#[derive(Serialize, JsonSchema)]
struct AlfInfo {
    config_exists: bool,
    api_key_set: bool,
    agent_tracked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_synced_sequence: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_synced_at: Option<String>,
    service_reachable: bool,
}

/// Snapshot of environment variables relevant to alf. Secret-bearing vars are
/// reported as presence booleans only — their values are never serialized.
#[derive(Serialize, JsonSchema)]
struct EnvInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    home: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    alf_home: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    alf_human: Option<String>,
    alf_api_key_set: bool,
    alf_vault_key_set: bool,
}

/// Location and state of the agent's credential vault
/// (`~/.alf/vault/{alf_agent_id}/credentials.json`, or the legacy
/// install-scoped path on mapping-less hosts).
#[derive(Serialize, JsonSchema)]
struct VaultInfo {
    path: String,
    exists: bool,
    /// The agent scope the vault path belongs to (WP1). Absent on
    /// mapping-less hosts (legacy install-scoped vault).
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_id: Option<Uuid>,
    /// A pre-WP1 install-scoped vault still exists alongside the per-agent
    /// scope — migration is pending (see the issues list). Omitted when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    legacy_vault_present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    credential_count: Option<usize>,
    /// Server-side credential count (delta-folded) from `GET /v1/agents/:id`.
    /// `None` when the service is unreachable or no agent is tracked.
    #[serde(skip_serializing_if = "Option::is_none")]
    server_credential_count: Option<usize>,
    /// `Some(true)` when the local and server credential counts match,
    /// `Some(false)` on divergence (vault not fully synced), `None` when not
    /// comparable. Lets an agent self-verify and self-heal after a sync.
    #[serde(skip_serializing_if = "Option::is_none")]
    parity_ok: Option<bool>,
}

#[derive(Serialize, JsonSchema)]
struct Issue {
    severity: String, // "error", "warning", "info"
    code: String,
    message: String,
    suggestion: String,
}

// ---------------------------------------------------------------------------
// Workspace auto-discovery
// ---------------------------------------------------------------------------

pub(crate) struct ResolvedWorkspace {
    pub(crate) path: PathBuf,
    pub(crate) source: String,
    /// The workspace path the runtime's own config points at, if any
    /// (openclaw → `~/.openclaw/openclaw.json`; zeroclaw → `~/.zeroclaw/config.toml`).
    /// Used for the workspace-mismatch warning.
    runtime_configured_path: Option<String>,
}

/// Workspace/install discovery: `-w` flag → `[defaults].workspace` → the
/// runtime's own configured/default location. Also reused by the selector-
/// driven commands to resolve the install root for discovery lazy-init.
pub(crate) fn resolve_workspace(
    flag: Option<&Path>,
    config: &Config,
    runtime: &str,
) -> ResolvedWorkspace {
    // The runtime's own configured workspace, for the mismatch diagnostic.
    // Hermes has no separate workspace — HERMES_HOME *is* the workspace.
    let configured = match runtime {
        "zeroclaw" => read_zeroclaw_workspace(),
        "hermes" => None,
        // Generic runtimes have no runtime-side config to auto-discover: the
        // workspace must be given explicitly. Placing this before the `_` arm
        // keeps generic from ever reading `~/.openclaw/openclaw.json` (F12).
        "generic" => None,
        _ => read_openclaw_workspace(),
    };

    // Priority 1: -w flag
    if let Some(ws) = flag {
        return ResolvedWorkspace {
            path: ws.to_path_buf(),
            source: "flag".into(),
            runtime_configured_path: configured,
        };
    }

    // Priority 2: defaults.workspace in ~/.alf/config.toml
    if let Some(ref ws) = config.defaults.workspace {
        if !ws.is_empty() {
            return ResolvedWorkspace {
                path: PathBuf::from(ws),
                source: "alf_config".into(),
                runtime_configured_path: configured,
            };
        }
    }

    // Generic runtimes never auto-discover a workspace: with neither `-w` nor
    // `[defaults].workspace` set there is nothing to resolve, so return an
    // explicit "unresolved" sentinel rather than falling through to OpenClaw's
    // `~/.openclaw/workspace` default (F12). The empty path fails the caller's
    // `is_dir()` check, surfacing a clear "workspace required" error instead of
    // silently pointing a generic agent at the OpenClaw workspace.
    if runtime == "generic" {
        return ResolvedWorkspace {
            path: PathBuf::new(),
            source: "unresolved".into(),
            runtime_configured_path: None,
        };
    }

    // Priority 3 + 4: runtime-specific discovery.
    if runtime == "zeroclaw" {
        // ZeroClaw keeps its workspace at `workspace_dir` in
        // ~/.zeroclaw/config.toml; default to ~/.zeroclaw when unset.
        if let Some(ref ws) = configured {
            return ResolvedWorkspace {
                path: PathBuf::from(ws),
                source: "zeroclaw_config".into(),
                runtime_configured_path: configured,
            };
        }
        let default_path = alf_core::home_dir()
            .map(|h| h.join(".zeroclaw"))
            .unwrap_or_else(|| PathBuf::from(".zeroclaw"));
        return ResolvedWorkspace {
            path: default_path,
            source: "default".into(),
            runtime_configured_path: configured,
        };
    }

    // Hermes: HERMES_HOME is the workspace; honor $HERMES_HOME, else ~/.hermes.
    if runtime == "hermes" {
        let default_path = std::env::var_os("HERMES_HOME")
            .map(PathBuf::from)
            .or_else(|| alf_core::home_dir().map(|h| h.join(".hermes")))
            .unwrap_or_else(|| PathBuf::from(".hermes"));
        return ResolvedWorkspace {
            path: default_path,
            source: if std::env::var_os("HERMES_HOME").is_some() {
                "hermes_env".into()
            } else {
                "default".into()
            },
            runtime_configured_path: configured,
        };
    }

    // OpenClaw: agents.defaults.workspace in ~/.openclaw/openclaw.json
    if let Some(ref ws) = configured {
        return ResolvedWorkspace {
            path: PathBuf::from(ws),
            source: "openclaw.json".into(),
            runtime_configured_path: configured,
        };
    }

    // Default: ~/.openclaw/workspace
    let default_path = alf_core::home_dir()
        .map(|h| h.join(".openclaw").join("workspace"))
        .unwrap_or_else(|| PathBuf::from(".openclaw/workspace"));

    ResolvedWorkspace {
        path: default_path,
        source: "default".into(),
        runtime_configured_path: configured,
    }
}

/// Resolve the install/workspace root, erroring if a runtime that requires an
/// explicit workspace (generic) has none (R1). Without this an unresolved
/// generic workspace flows into the default `export_agent`, which writes a stray
/// `.alf-agent-id` into the CWD and then bails with a blank path. Non-generic
/// runtimes always resolve to a concrete default, so this is a no-op for them.
pub(crate) fn resolve_workspace_required(
    flag: Option<&Path>,
    config: &Config,
    runtime: &str,
) -> Result<PathBuf> {
    let resolved = resolve_workspace(flag, config, runtime);
    if resolved.source == "unresolved" || resolved.path.as_os_str().is_empty() {
        anyhow::bail!(
            "{runtime} runtime requires an explicit workspace. Pass `-w <path>` or set \
             `[defaults].workspace` in ~/.alf/config.toml."
        );
    }
    Ok(resolved.path)
}

/// Read `workspace_dir` from `~/.zeroclaw/config.toml`.
///
/// `workspace_dir` is a top-level key in ZeroClaw's V3 config, so a lightweight
/// line scan is enough — and avoids pulling a TOML parser into `alf-cli` just
/// for this. Stops at the first table header so a same-named key inside a
/// `[section]` can't be misread as the top-level one.
fn read_zeroclaw_workspace() -> Option<String> {
    let home = alf_core::home_dir()?;
    let config_path = home.join(".zeroclaw").join("config.toml");
    let content = fs::read_to_string(&config_path).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            break; // entered a [table]; top-level keys are above this
        }
        if let Some(rest) = trimmed.strip_prefix("workspace_dir") {
            let rest = rest.trim_start();
            if let Some(value) = rest.strip_prefix('=') {
                let value = value.trim().trim_matches('"').trim_matches('\'');
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

/// Read `agents.defaults.workspace` from `~/.openclaw/openclaw.json`.
fn read_openclaw_workspace() -> Option<String> {
    let home = alf_core::home_dir()?;
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
    runtime: &str,
) -> Vec<Issue> {
    let mut issues = Vec::new();

    if !ws.exists {
        issues.push(Issue {
            severity: "error".into(),
            code: "workspace_not_found".into(),
            message: format!("Workspace directory not found at {}", ws.path),
            suggestion: format!(
                "Pass the correct workspace path: alf check -r {runtime} -w /path/to/workspace"
            ),
        });
        return issues; // no point checking resources if workspace doesn't exist
    }

    if !ws.writable {
        issues.push(Issue {
            severity: "warning".into(),
            code: "workspace_not_writable".into(),
            message: format!("Workspace exists but is not writable: {}", ws.path),
            suggestion: "Check file permissions on the workspace directory".into(),
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
            suggestion: "Workspace may not be initialized — check the path".into(),
        });
    }

    if !resources.soul_md {
        issues.push(Issue {
            severity: "warning".into(),
            code: "no_soul_md".into(),
            message: "SOUL.md not found in workspace".into(),
            suggestion: "Agent has no persona file; export will use a fallback name".into(),
        });
    }

    let has_memory_content = resources.memory_md
        || resources.memory_dir && resources.daily_logs.as_ref().is_some_and(|d| d.count > 0);
    if !has_memory_content {
        issues.push(Issue {
            severity: "warning".into(),
            code: "no_memory_content".into(),
            message: "No MEMORY.md and no daily logs in memory/ directory".into(),
            suggestion: "Nothing to sync — agent has no memories yet".into(),
        });
    }

    if resources.memory_dir {
        if let Some(ref dl) = resources.daily_logs {
            if dl.count == 0 {
                issues.push(Issue {
                    severity: "warning".into(),
                    code: "memory_dir_empty".into(),
                    message: "memory/ directory exists but has no daily log files".into(),
                    suggestion: "No daily logs yet — memories will accumulate over time".into(),
                });
            }
        }
    }

    if !alf.api_key_set {
        issues.push(Issue {
            severity: "error".into(),
            code: "no_api_key".into(),
            message: "No API key configured".into(),
            suggestion: "Run: alf login --key <your-api-key>".into(),
        });
    }

    if alf.api_key_set && !alf.service_reachable {
        issues.push(Issue {
            severity: "error".into(),
            code: "service_unreachable".into(),
            message: "API endpoint not responding".into(),
            suggestion: "Check network connectivity and API URL in ~/.alf/config.toml".into(),
        });
    }

    // Runtime config presence diagnostic. Each runtime keeps its own config
    // (openclaw → ~/.openclaw/openclaw.json; zeroclaw → ~/.zeroclaw/config.toml),
    // so this is selected by runtime to avoid a spurious "openclaw not installed"
    // note when checking zeroclaw. `None` for any other runtime.
    let home = alf_core::home_dir().unwrap_or_default();
    let config_issue = match runtime {
        "openclaw"
            if resolved.runtime_configured_path.is_none()
                && !home.join(".openclaw").join("openclaw.json").exists() =>
        {
            Some((
                "openclaw_config_not_found",
                "~/.openclaw/openclaw.json not found",
                "OpenClaw",
            ))
        }
        "zeroclaw" if !home.join(".zeroclaw").join("config.toml").exists() => Some((
            "zeroclaw_config_not_found",
            "~/.zeroclaw/config.toml not found",
            "ZeroClaw",
        )),
        _ => None,
    };
    if let Some((code, message, name)) = config_issue {
        issues.push(Issue {
            severity: "info".into(),
            code: code.into(),
            message: message.into(),
            suggestion: format!("{name} may not be installed, or uses a non-standard location"),
        });
    }

    // Workspace mismatch: -w differs from the runtime's configured workspace.
    // Flattened to a single `if let` (no nested ifs) and compared as `Path`
    // borrows so clippy is happy.
    let mismatch = if ws.source == "flag" {
        resolved
            .runtime_configured_path
            .as_deref()
            .filter(|cfg_ws| Path::new(&ws.path) != Path::new(cfg_ws))
    } else {
        None
    };
    if let Some(cfg_ws) = mismatch {
        issues.push(Issue {
            severity: "warning".into(),
            code: "workspace_mismatch".into(),
            message: format!(
                "-w path ({}) differs from {runtime} configured path ({cfg_ws})",
                ws.path
            ),
            suggestion: "May be intentional; noting for awareness".into(),
        });
    }

    issues
}

/// Where each runtime keeps its workspace, for the `workspace_not_found` hint.
/// Generic has no auto-discovery, so it points at the explicit-workspace knobs.
fn workspace_config_hint(runtime: &str) -> &'static str {
    match runtime {
        "zeroclaw" => {
            "The workspace path may be customized in ~/.zeroclaw/config.toml (workspace_dir)"
        }
        "hermes" => "The workspace is $HERMES_HOME (defaults to ~/.hermes)",
        "generic" => {
            "generic has no auto-discovery: pass -w <path> or set [defaults].workspace in \
             ~/.alf/config.toml"
        }
        _ => {
            "The workspace path may be customized in ~/.openclaw/openclaw.json under \
              agents.defaults.workspace"
        }
    }
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
        if result
            .issues
            .iter()
            .any(|i| i.code == "workspace_not_found")
        {
            suggestions.push(workspace_config_hint(&result.runtime).into());
        }
    }

    suggestions
}

// ---------------------------------------------------------------------------
// Command entry point
// ---------------------------------------------------------------------------

pub fn run(runtime: &str, workspace_arg: Option<&Path>, agent: Option<&str>) -> Result<()> {
    let result = gather(runtime, workspace_arg, agent)?;
    if output::human_mode() {
        print_human(&result);
    } else {
        output::json(&result);
    }
    Ok(())
}

/// Run the full `alf check` diagnostic (discovery + reconcile + persist + vault
/// migration + resource/service probing) and return the structured result
/// **without** printing. Extracted as a seam so the MCP `alf_check` tool
/// returns the byte-identical JSON. Progress lines still go to stderr via
/// `output::progress` (never stdout — safe for the MCP protocol stream).
pub(crate) fn gather(
    runtime: &str,
    workspace_arg: Option<&Path>,
    agent: Option<&str>,
) -> Result<CheckResult> {
    let mut config = Config::load()?;

    output::progress(&format!("Checking {} environment...", runtime));

    // Resolve workspace
    let resolved = resolve_workspace(workspace_arg, &config, runtime);

    let ws_path = &resolved.path;
    let ws_exists = ws_path.is_dir();
    let ws_writable = if ws_exists {
        tempfile::Builder::new()
            .prefix(".alf_check_write_")
            .tempfile_in(ws_path)
            .is_ok()
    } else {
        false
    };

    let workspace_info = WorkspaceInfo {
        path: ws_path.to_string_lossy().into(),
        source: resolved.source.clone(),
        exists: ws_exists,
        writable: ws_writable,
    };

    // Agent discovery + mapping reconcile (WP0). Unknown runtimes keep the
    // legacy output shape (no agents section); a discovery failure becomes a
    // warning issue — check is a diagnostic and must not hard-fail.
    let mut agents_section: Option<AgentsSection> = None;
    let mut agent_issues: Vec<Issue> = Vec::new();
    if let Some(adapt) = crate::adapter::get_adapter(runtime) {
        match crate::discovery::discover_and_reconcile(&config, adapt.as_ref(), runtime, ws_path) {
            Ok(outcome) => {
                // Guard: an ad-hoc `-w` check must not hijack a non-empty
                // mapping that doesn't contain this workspace (an empty
                // mapping may still be seeded via -w).
                let mapped = config.agents_for_runtime(runtime);
                let flag_workspace_unmapped = resolved.source == "flag"
                    && !mapped.is_empty()
                    && !mapped.iter().any(|a| Path::new(&a.workspace) == ws_path);
                if flag_workspace_unmapped {
                    agent_issues.push(Issue {
                        severity: "info".into(),
                        code: "agents_mapping_skipped_flag_workspace".into(),
                        message: format!(
                            "-w path ({}) is not in the [[agents]] mapping — discovery was not persisted",
                            ws_path.display()
                        ),
                        suggestion:
                            "Run 'alf check' without -w (or from the mapped install) to update the mapping"
                                .into(),
                    });
                } else if ws_exists {
                    crate::discovery::persist(&mut config, &outcome)?;
                    agent_issues.extend(collect_unpersisted_id_issues(&outcome));
                }

                agent_issues.extend(collect_agent_issues(&outcome));
                agents_section = Some(build_agents_section(outcome));
            }
            Err(e) => {
                agent_issues.push(Issue {
                    severity: "warning".into(),
                    code: "agent_discovery_failed".into(),
                    message: format!("Agent discovery failed: {e:#}"),
                    suggestion: "Fix the reported file (e.g. a malformed .alf-agent-id) and re-run alf check".into(),
                });
            }
        }
    }

    // Vault scope (WP1): the agent whose per-agent vault applies. Lenient —
    // check is a diagnostic and never errors on ambiguity or an unknown
    // selector; it just falls back to the legacy install-scoped view.
    let vault_scope: Option<Uuid> = selector::vault_scope_agent_id_lenient(&config, runtime, agent)
        .ok()
        .flatten();

    // WP1: check is the natural upgrade touchpoint — perform the legacy-vault
    // migration when the target is unambiguous, report otherwise. Never a
    // hard failure.
    match vault_migrate::ensure_migrated(&config, runtime, None) {
        Ok(MigrationOutcome::NotNeeded) => {}
        Ok(MigrationOutcome::Migrated { vault, key, agent }) => {
            if let Some(p) = vault {
                output::progress(&format!(
                    "  Migrated legacy vault to {} (agent {agent})",
                    p.display()
                ));
            }
            if let Some(p) = key {
                output::progress(&format!(
                    "  Migrated legacy vault key to {} (agent {agent})",
                    p.display()
                ));
            }
        }
        Ok(MigrationOutcome::Blocked(err)) => {
            agent_issues.push(Issue {
                severity: "warning".into(),
                code: err.code.into(),
                message: err.cause,
                suggestion: err.remedy,
            });
        }
        Err(e) => {
            agent_issues.push(Issue {
                severity: "warning".into(),
                code: "vault_migration_failed".into(),
                message: format!("Legacy vault migration failed: {e:#}"),
                suggestion: "Fix the reported file problem and re-run alf check".into(),
            });
        }
    }

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

    // OpenClaw info (openclaw-only block in the JSON output).
    let openclaw = if runtime == "openclaw" {
        Some(OpenClawInfo {
            config_found: resolved.runtime_configured_path.is_some()
                || alf_core::home_dir()
                    .map(|h| h.join(".openclaw").join("openclaw.json").exists())
                    .unwrap_or(false),
            workspace_configured: resolved.runtime_configured_path.clone(),
        })
    } else {
        None
    };

    // ALF state
    let status = context::gather_status()?;
    let api_key_set = status.api_key_set;
    // The global --agent scopes the tracked/last-synced section to one agent
    // (information-only; unknown selectors just fall back to the default view).
    let scoped = agent
        .and_then(|sel| config.find_agent(runtime, sel))
        .map(|row| row.alf_agent_id)
        .and_then(|id| status.agents.iter().find(|a| a.agent_id == id).cloned());
    let (agent_tracked, last_synced_sequence, last_synced_at) = match (&scoped, agent) {
        (Some(a), _) => (true, Some(a.last_synced_sequence), a.last_synced_at.clone()),
        (None, Some(_)) => (false, None, None),
        (None, None) => (
            !status.agents.is_empty(),
            status.agents.first().map(|a| a.last_synced_sequence),
            status.agents.first().and_then(|a| a.last_synced_at.clone()),
        ),
    };

    // Fetch the server's view of the agent once: it confirms connectivity AND
    // yields the delta-folded credential count used for vault parity (WS-B).
    // Scoping (WP1): prefer the vault-scoped agent (--agent / sole enabled)
    // so parity compares the per-agent vault against ITS cloud counts, not
    // whichever state file happens to sort first.
    let server_agent: Option<AgentInfo> = if api_key_set && agent_tracked {
        ApiClient::from_config(&config).ok().and_then(|c| {
            vault_scope
                .and_then(|id| status.agents.iter().find(|a| a.agent_id == id))
                .or_else(|| status.agents.first())
                .and_then(|a| c.get_agent(a.agent_id).ok())
        })
    } else {
        None
    };
    let service_reachable = if api_key_set && agent_tracked {
        server_agent.is_some()
    } else if api_key_set {
        // No agents tracked yet, but key is set — validate config connectivity.
        ApiClient::from_config(&config).is_ok()
    } else {
        false
    };

    let alf_info = AlfInfo {
        config_exists: status.config_exists,
        api_key_set,
        agent_tracked,
        last_synced_sequence,
        last_synced_at,
        service_reachable,
    };

    // Vault parity (WS-B): compare the local vault count to the server's
    // delta-folded credential count. Divergence ⇒ the vault has not fully synced
    // (e.g. credentials added but not yet pushed, or a diverged local base).
    let mut vault = gather_vault(vault_scope);
    vault.server_credential_count = server_agent
        .as_ref()
        .and_then(|a| a.layer_counts.as_ref())
        .map(|lc| lc.credentials as usize);
    vault.parity_ok = match (vault.credential_count, vault.server_credential_count) {
        (Some(local), Some(server)) => Some(local == server),
        _ => None,
    };

    // Collect issues
    let mut issues = collect_issues(&workspace_info, &resources, &alf_info, &resolved, runtime);
    issues.extend(agent_issues);
    if vault.parity_ok == Some(false) {
        issues.push(Issue {
            severity: "warning".into(),
            code: "vault_not_synced".into(),
            message: format!(
                "Local vault has {} credential(s) but the cloud shows {} — the vault has not fully synced.",
                vault.credential_count.unwrap_or(0),
                vault.server_credential_count.unwrap_or(0)
            ),
            suggestion: format!(
                "Run `alf sync --recover -r {} -w {}` to re-derive the delta against cloud truth.",
                runtime,
                ws_path.display()
            ),
        });
    }

    let has_errors = issues.iter().any(|i| i.severity == "error");
    let ready_to_sync = !has_errors && ws_exists;

    let alfignore = AlfignoreInfo {
        present: ws_exists && ws_path.join(".alfignore").is_file(),
    };

    let mut result = CheckResult {
        version: env!("CARGO_PKG_VERSION").into(),
        ok: !has_errors,
        runtime: runtime.into(),
        ready_to_sync,
        workspace: workspace_info,
        resources,
        alfignore,
        openclaw,
        alf: alf_info,
        agents: agents_section,
        env: gather_env(),
        vault,
        issues,
        suggestions: Vec::new(),
    };
    result.suggestions = build_suggestions(&result);

    Ok(result)
}

// ---------------------------------------------------------------------------
// Agents section (WP0)
// ---------------------------------------------------------------------------

fn status_label(status: crate::discovery::RowStatus) -> &'static str {
    match status {
        crate::discovery::RowStatus::Existing => "existing",
        crate::discovery::RowStatus::New => "new",
        crate::discovery::RowStatus::Removed => "removed",
        crate::discovery::RowStatus::Drift => "drift",
    }
}

fn build_agents_section(outcome: crate::discovery::ReconcileOutcome) -> AgentsSection {
    let agents = outcome
        .rows
        .iter()
        .map(|r| AgentRow {
            runtime_agent: r.entry.runtime_agent.clone(),
            runtime_agent_id: r.entry.runtime_agent_id.clone(),
            alf_agent_id: r.entry.alf_agent_id.to_string(),
            workspace: r.entry.workspace.clone(),
            enabled: r.entry.enabled,
            status: status_label(r.status),
        })
        .collect();
    let aliases_with = |status: crate::discovery::RowStatus| -> Vec<String> {
        outcome
            .rows
            .iter()
            .filter(|r| r.status == status)
            .map(|r| r.entry.runtime_agent.clone())
            .collect()
    };
    AgentsSection {
        first_run: outcome.first_run,
        agents,
        new: aliases_with(crate::discovery::RowStatus::New),
        removed: aliases_with(crate::discovery::RowStatus::Removed),
        drift: outcome.drift,
    }
}

/// Issues derived from a reconcile outcome. Warnings only — none of these
/// flip `ready_to_sync` (discovery is information-only).
fn collect_agent_issues(outcome: &crate::discovery::ReconcileOutcome) -> Vec<Issue> {
    let mut issues = Vec::new();
    for row in &outcome.rows {
        match row.status {
            crate::discovery::RowStatus::New if !row.entry.enabled => {
                issues.push(Issue {
                    severity: "info".into(),
                    code: "agent_discovered_new".into(),
                    message: format!(
                        "New agent '{}' discovered — not enabled.",
                        row.entry.runtime_agent
                    ),
                    suggestion: format!("Run: alf agents enable {}", row.entry.runtime_agent),
                });
            }
            crate::discovery::RowStatus::Removed => {
                issues.push(Issue {
                    severity: "warning".into(),
                    code: "agent_removed".into(),
                    message: format!(
                        "Agent '{}' is mapped but no longer discovered in this install.",
                        row.entry.runtime_agent
                    ),
                    suggestion: "Mapping and cloud archive are preserved; edit ~/.alf/config.toml to drop the row if this is intentional".into(),
                });
            }
            _ => {}
        }
    }
    for d in &outcome.drift {
        issues.push(Issue {
            severity: "warning".into(),
            code: "agent_identity_drift".into(),
            message: d.message.clone(),
            suggestion: d.remedy.clone(),
        });
    }
    issues
}

/// After a persist: rows whose workspace exists but still lacks its
/// `.alf-agent-id` (persist writes it best-effort; failure is a warning,
/// never fatal).
fn collect_unpersisted_id_issues(outcome: &crate::discovery::ReconcileOutcome) -> Vec<Issue> {
    let mut issues = Vec::new();
    for row in &outcome.rows {
        if !matches!(
            row.status,
            crate::discovery::RowStatus::New | crate::discovery::RowStatus::Existing
        ) {
            continue;
        }
        let ws = Path::new(&row.entry.workspace);
        if ws.is_dir() && !ws.join(alf_core::AGENT_ID_FILE).is_file() {
            issues.push(Issue {
                severity: "warning".into(),
                code: "agent_id_not_persisted".into(),
                message: format!(
                    "Could not persist {} into {}",
                    alf_core::AGENT_ID_FILE,
                    ws.display()
                ),
                suggestion: "Check workspace permissions; export retries the write".into(),
            });
        }
    }
    issues
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

    println!("  alf:       {}", result.version);
    println!("  Runtime:   {}", result.runtime);
    println!(
        "  Workspace: {} (source: {})",
        result.workspace.path, result.workspace.source
    );
    println!(
        "  Exists:    {}  Writable: {}",
        if result.workspace.exists { "yes" } else { "no" },
        if result.workspace.writable {
            "yes"
        } else {
            "no"
        }
    );
    println!();

    println!("  Resources:");
    println!("    SOUL.md:     {}", yn(result.resources.soul_md));
    println!("    IDENTITY.md: {}", yn(result.resources.identity_md));
    println!("    AGENTS.md:   {}", yn(result.resources.agents_md));
    println!("    USER.md:     {}", yn(result.resources.user_md));
    println!("    MEMORY.md:   {}", yn(result.resources.memory_md));
    println!("    memory/:     {}", yn(result.resources.memory_dir));
    println!("    .alfignore:  {}", yn(result.alfignore.present));
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
    if let Some(seq) = result.alf.last_synced_sequence {
        println!(
            "    Last synced: seq {} ({})",
            seq,
            result
                .alf
                .last_synced_at
                .as_deref()
                .unwrap_or("time unknown")
        );
    }
    println!();

    println!("  Vault:");
    println!("    Path:        {}", result.vault.path);
    println!("    Exists:      {}", yn(result.vault.exists));
    if let Some(n) = result.vault.credential_count {
        println!("    Credentials: {n}");
    }
    if let Some(n) = result.vault.server_credential_count {
        println!("    Cloud:       {n}");
    }
    if let Some(ok) = result.vault.parity_ok {
        println!("    Synced:      {}", yn(ok));
    }
    println!();

    if let Some(ref agents) = result.agents {
        println!("  Agents:");
        for row in &agents.agents {
            println!(
                "    {}  {}  {}  ({})",
                row.runtime_agent,
                row.alf_agent_id,
                if row.enabled { "enabled" } else { "disabled" },
                row.status
            );
        }
        if !agents.drift.is_empty() {
            for d in &agents.drift {
                println!("    {} {}", "⚠".yellow().bold(), d.message);
            }
        }
        println!();
    }

    println!("  Environment:");
    println!(
        "    HOME:                 {}",
        result.env.home.as_deref().unwrap_or("(unset)")
    );
    if let Some(ref ah) = result.env.alf_home {
        println!("    ALF_HOME:             {ah}");
    }
    println!(
        "    ALF_API_KEY:          {}",
        if result.env.alf_api_key_set {
            "set"
        } else {
            "unset"
        }
    );
    println!(
        "    ALF_VAULT_KEY:        {}",
        if result.env.alf_vault_key_set {
            "set"
        } else {
            "unset"
        }
    );
    println!();

    if !result.issues.is_empty() {
        println!("  Issues:");
        for issue in &result.issues {
            let severity_label = match issue.severity.as_str() {
                "error" => "ERROR".red().bold().to_string(),
                "warning" => "WARN".yellow().bold().to_string(),
                _ => "INFO".dimmed().to_string(),
            };
            println!(
                "    [{}] {} ({})",
                severity_label, issue.message, issue.code
            );
            println!("      Suggestion: {}", issue.suggestion);
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
    if b {
        "yes"
    } else {
        "no"
    }
}

/// Snapshot alf-relevant env vars. Secret-bearing vars are reduced to presence
/// only — their values are never read into the result.
fn gather_env() -> EnvInfo {
    let val = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
    let set = |k: &str| std::env::var_os(k).map(|v| !v.is_empty()).unwrap_or(false);
    EnvInfo {
        home: val("HOME").or_else(|| val("USERPROFILE")),
        alf_home: val("ALF_HOME"),
        alf_human: val("ALF_HUMAN"),
        alf_api_key_set: set("ALF_API_KEY"),
        alf_vault_key_set: set("ALF_VAULT_KEY"),
    }
}

/// Locate the (per-agent, WP1) credential vault and, if present, count its
/// records. Never fails: a missing or malformed vault yields
/// `credential_count: None`.
fn gather_vault(scope: Option<Uuid>) -> VaultInfo {
    match crate::vault_key::default_vault_path(scope) {
        Ok(path) => {
            let exists = path.is_file();
            let credential_count = exists
                .then(|| fs::read_to_string(&path).ok())
                .flatten()
                .and_then(|raw| serde_json::from_str::<CredentialsDocument>(&raw).ok())
                .map(|doc| doc.credentials.len());
            // A leftover install-scoped vault next to a per-agent scope means
            // migration is pending (the issues list carries the remedy).
            let legacy_vault_present = scope.is_some()
                && crate::vault_key::default_vault_path(None)
                    .map(|p| p.is_file())
                    .unwrap_or(false);
            VaultInfo {
                path: path.to_string_lossy().into_owned(),
                exists,
                agent_id: scope,
                legacy_vault_present,
                credential_count,
                server_credential_count: None,
                parity_ok: None,
            }
        }
        Err(_) => VaultInfo {
            path: "(unknown)".into(),
            exists: false,
            agent_id: None,
            legacy_vault_present: false,
            credential_count: None,
            server_credential_count: None,
            parity_ok: None,
        },
    }
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
        let resolved = resolve_workspace(Some(&flag_path), &config, "openclaw");

        assert_eq!(resolved.path, PathBuf::from("/custom/workspace"));
        assert_eq!(resolved.source, "flag");
    }

    #[test]
    fn resolve_workspace_alf_config_second() {
        let mut config = Config::default();
        config.defaults.workspace = Some("/alf-configured/workspace".into());
        let resolved = resolve_workspace(None, &config, "openclaw");

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
        let resolved = resolve_workspace(None, &config, "openclaw");

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
        let resolved = resolve_workspace(None, &config, "openclaw");

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
    fn resolve_workspace_zeroclaw_reads_workspace_dir() {
        let _guard = crate::context::tests::HOME_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());
        std::env::remove_var("ALF_HOME");

        let zc_dir = tmp.path().join(".zeroclaw");
        fs::create_dir_all(&zc_dir).unwrap();
        fs::write(
            zc_dir.join("config.toml"),
            "schema_version = 3\nworkspace_dir = \"/custom/zc/workspace\"\n\n[memory]\nworkspace_dir = \"/should/not/win\"\n",
        )
        .unwrap();

        let mut config = Config::default();
        config.defaults.workspace = None;
        let resolved = resolve_workspace(None, &config, "zeroclaw");

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }

        assert_eq!(resolved.path, PathBuf::from("/custom/zc/workspace"));
        assert_eq!(resolved.source, "zeroclaw_config");
    }

    #[test]
    fn resolve_workspace_zeroclaw_default_fallback() {
        let _guard = crate::context::tests::HOME_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());
        std::env::remove_var("ALF_HOME");

        let mut config = Config::default();
        config.defaults.workspace = None;
        let resolved = resolve_workspace(None, &config, "zeroclaw");

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }

        assert_eq!(resolved.source, "default");
        assert!(resolved.path.ends_with(".zeroclaw"));
    }

    // --- Generic runtime workspace resolution (WP-M1) ------------------------

    /// Generic honors `-w` (priority 1) and `[defaults].workspace` (priority 2)
    /// exactly like every other runtime.
    #[test]
    fn resolve_workspace_generic_honors_explicit_workspace() {
        let flag = PathBuf::from("/gen/ws");
        let resolved = resolve_workspace(Some(&flag), &Config::default(), "generic");
        assert_eq!(resolved.path, PathBuf::from("/gen/ws"));
        assert_eq!(resolved.source, "flag");

        let mut config = Config::default();
        config.defaults.workspace = Some("/alf/gen/ws".into());
        let resolved = resolve_workspace(None, &config, "generic");
        assert_eq!(resolved.path, PathBuf::from("/alf/gen/ws"));
        assert_eq!(resolved.source, "alf_config");
    }

    /// Without an explicit workspace, generic must NOT fall through to OpenClaw
    /// discovery — even when a `~/.openclaw/openclaw.json` is present. It
    /// resolves to the "unresolved" sentinel (empty path) instead.
    #[test]
    fn resolve_workspace_generic_never_falls_through_to_openclaw() {
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

        let resolved = resolve_workspace(None, &Config::default(), "generic");

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }

        assert_eq!(resolved.source, "unresolved");
        assert_eq!(resolved.path, PathBuf::new());
        assert!(resolved.runtime_configured_path.is_none());
    }

    /// Goal (c) guard: adding the generic arm must not perturb openclaw /
    /// zeroclaw / hermes resolution. Pins each runtime's default-fallback
    /// (path, source) byte-for-byte against a clean HOME.
    #[test]
    fn resolve_workspace_existing_runtimes_unchanged_by_generic_arm() {
        let _guard = crate::context::tests::HOME_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let prev = std::env::var_os("HOME");
        let prev_alf = std::env::var_os("ALF_HOME");
        let prev_hermes = std::env::var_os("HERMES_HOME");
        std::env::set_var("HOME", tmp.path());
        std::env::remove_var("ALF_HOME");
        std::env::remove_var("HERMES_HOME");

        let config = Config::default();
        let openclaw = resolve_workspace(None, &config, "openclaw");
        let zeroclaw = resolve_workspace(None, &config, "zeroclaw");
        let hermes = resolve_workspace(None, &config, "hermes");

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_alf {
            Some(v) => std::env::set_var("ALF_HOME", v),
            None => std::env::remove_var("ALF_HOME"),
        }
        match prev_hermes {
            Some(v) => std::env::set_var("HERMES_HOME", v),
            None => std::env::remove_var("HERMES_HOME"),
        }

        assert_eq!(openclaw.source, "default");
        assert_eq!(
            openclaw.path,
            tmp.path().join(".openclaw").join("workspace")
        );
        assert_eq!(zeroclaw.source, "default");
        assert_eq!(zeroclaw.path, tmp.path().join(".zeroclaw"));
        assert_eq!(hermes.source, "default");
        assert_eq!(hermes.path, tmp.path().join(".hermes"));
    }

    /// R1: an unresolved generic workspace errors (no stray CWD write path).
    /// Non-generic runtimes always resolve, so the guard is a no-op for them.
    #[test]
    fn resolve_workspace_required_errors_for_unresolved_generic() {
        let err = resolve_workspace_required(None, &Config::default(), "generic").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("generic"), "must name the runtime: {msg}");
        assert!(msg.contains("-w"), "must suggest -w: {msg}");

        // With an explicit -w it resolves fine.
        let flag = PathBuf::from("/gen/ws");
        assert_eq!(
            resolve_workspace_required(Some(&flag), &Config::default(), "generic").unwrap(),
            flag
        );
    }

    /// C3: the workspace_not_found config hint names the correct config surface
    /// per runtime (and never points a non-openclaw runtime at openclaw.json).
    #[test]
    fn workspace_config_hint_is_runtime_aware() {
        assert!(workspace_config_hint("openclaw").contains("openclaw.json"));
        assert!(workspace_config_hint("zeroclaw").contains("zeroclaw/config.toml"));
        assert!(workspace_config_hint("hermes").contains("HERMES_HOME"));
        let generic = workspace_config_hint("generic");
        assert!(generic.contains("-w") && generic.contains("[defaults].workspace"));
        assert!(
            !generic.contains("openclaw"),
            "generic hint must not mention openclaw"
        );
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

    #[test]
    fn gather_env_redacts_secrets() {
        let _guard = crate::context::tests::HOME_LOCK.lock().unwrap();
        let prev_api = std::env::var_os("ALF_API_KEY");
        let prev_vk = std::env::var_os("ALF_VAULT_KEY");

        std::env::set_var("ALF_API_KEY", "super-secret-key");
        std::env::set_var("ALF_VAULT_KEY", "vault-secret-value");

        let env = gather_env();
        assert!(env.alf_api_key_set);
        assert!(env.alf_vault_key_set);

        // Contract: secret VALUES must never be serialized — only presence.
        let json = serde_json::to_string(&env).unwrap();
        assert!(!json.contains("super-secret-key"), "API key value leaked");
        assert!(
            !json.contains("vault-secret-value"),
            "vault key value leaked"
        );

        restore_var("ALF_API_KEY", prev_api);
        restore_var("ALF_VAULT_KEY", prev_vk);
    }

    #[test]
    fn gather_vault_reports_presence_and_count() {
        let _guard = crate::context::tests::HOME_LOCK.lock().unwrap();
        let prev_alf_home = std::env::var_os("ALF_HOME");

        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());

        // No vault file yet (mapping-less host: legacy install-scoped path).
        let v = gather_vault(None);
        assert!(!v.exists);
        assert_eq!(v.credential_count, None);
        assert_eq!(v.agent_id, None);
        assert!(!v.legacy_vault_present);

        // A well-formed (empty) vault parses and counts.
        let vault = tmp
            .path()
            .join(".alf")
            .join("vault")
            .join("credentials.json");
        std::fs::create_dir_all(vault.parent().unwrap()).unwrap();
        std::fs::write(&vault, r#"{"credentials":[]}"#).unwrap();
        let v = gather_vault(None);
        assert!(v.exists);
        assert_eq!(v.credential_count, Some(0));

        // Malformed JSON: present but uncounted, never panics.
        std::fs::write(&vault, "{ not json").unwrap();
        let v = gather_vault(None);
        assert!(v.exists);
        assert_eq!(v.credential_count, None);

        restore_var("ALF_HOME", prev_alf_home);
    }

    /// WP1: a scoped gather reads the per-agent vault path, reports the
    /// scope, and flags a pending legacy vault.
    #[test]
    fn gather_vault_per_agent_scope_and_legacy_flag() {
        let _guard = crate::context::tests::HOME_LOCK.lock().unwrap();
        let prev_alf_home = std::env::var_os("ALF_HOME");

        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());
        let id = Uuid::parse_str("cfef1150-0000-4000-8000-0000000000aa").unwrap();

        // Per-agent vault with one record; a legacy vault sits alongside.
        let agent_vault = alf_core::agent_vault_path(tmp.path(), id);
        std::fs::create_dir_all(agent_vault.parent().unwrap()).unwrap();
        std::fs::write(
            &agent_vault,
            r#"{"credentials":[{
                "id":"00000000-0000-0000-0000-000000000001",
                "agent_id":"00000000-0000-0000-0000-000000000001",
                "service":"x","credential_type":"api_key",
                "encrypted_payload":"AAAA",
                "encryption":{"algorithm":"xchacha20-poly1305","nonce":"AAAA"},
                "created_at":"2026-01-01T00:00:00Z"
            }]}"#,
        )
        .unwrap();
        let legacy = tmp
            .path()
            .join(".alf")
            .join("vault")
            .join("credentials.json");
        std::fs::write(&legacy, r#"{"credentials":[]}"#).unwrap();

        let v = gather_vault(Some(id));
        assert_eq!(v.path, agent_vault.to_string_lossy());
        assert!(v.exists);
        assert_eq!(v.credential_count, Some(1));
        assert_eq!(v.agent_id, Some(id));
        assert!(
            v.legacy_vault_present,
            "pending legacy vault must be flagged"
        );

        // Legacy gone ⇒ flag clears.
        std::fs::remove_file(&legacy).unwrap();
        let v = gather_vault(Some(id));
        assert!(!v.legacy_vault_present);

        restore_var("ALF_HOME", prev_alf_home);
    }

    fn restore_var(key: &str, prev: Option<std::ffi::OsString>) {
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
