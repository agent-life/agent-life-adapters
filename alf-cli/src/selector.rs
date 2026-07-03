//! Current-agent selector (WP0).
//!
//! Precedence: the global `--agent` flag → a non-empty `ALF_AGENT` env var →
//! None ⇒ the sole enabled `[[agents]]` row. The env var is read manually
//! (`std::env::var`, never clap's `env` attr) so precedence is
//! pure-unit-testable and the winning source is reportable.
//!
//! Enabled-gate policy (design §8): selection ≠ sync-eligibility. `alf sync`
//! refuses a disabled selection; export/import/add/restore/purge/vault accept
//! an explicitly selected disabled agent. None-resolution and `--all` consider
//! only enabled rows.

use crate::config::{AgentEntry, Config};
use crate::discovery;
use crate::errors::{codes, CliError};
use crate::output;
use crate::state;

use alf_core::adapter::{Adapter, AgentBinding, MemorySource};

use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Which input won the selector precedence.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectorSource {
    Flag,
    Env,
    SoleEnabled,
    /// Cloud ops only: a UUID argument that is not in the mapping, used
    /// verbatim (restore-by-UUID onto a fresh host). Never constructed yet —
    /// `resolve_for_cloud_op` returns a bare id; the variant reserves the
    /// wire value for when cloud ops report their selection.
    #[allow(dead_code)]
    LegacyUuid,
}

/// The resolved current agent.
#[derive(Debug)]
pub struct SelectedAgent {
    pub alf_agent_id: Uuid,
    pub alias: String,
    pub workspace: PathBuf,
    /// Not read by WP0 command wiring yet — WP1 threads it into per-agent
    /// vault/key paths.
    #[allow(dead_code)]
    pub runtime: String,
    /// Fresh-discovery match when available, else synthesized
    /// (`InWorkspaceFiles`) from the mapping row.
    pub binding: AgentBinding,
    pub enabled: bool,
    pub source: SelectorSource,
}

/// Resolve the current agent for a workspace-scoped command.
///
/// Empty `[[agents]]` for the runtime ⇒ LAZY INIT: run the same
/// discover→reconcile→persist pipeline `alf check` uses, then resolve —
/// one code path (`config` is `&mut` for exactly that).
pub fn select_current_agent(
    config: &mut Config,
    adapter: &dyn Adapter,
    runtime: &str,
    install: &Path,
    agent_flag: Option<&str>,
) -> Result<SelectedAgent> {
    lazy_init(config, adapter, runtime, install)?;

    let (selector, source) = match agent_flag {
        Some(s) => (Some(s.to_string()), SelectorSource::Flag),
        None => match env_agent() {
            Some(s) => (Some(s), SelectorSource::Env),
            None => (None, SelectorSource::SoleEnabled),
        },
    };

    let row = match &selector {
        Some(sel) => match config.find_agent(runtime, sel) {
            Some(row) => row.clone(),
            None => return Err(agent_not_found(runtime, sel, config).into()),
        },
        None => sole_enabled_row(config, runtime)?,
    };

    Ok(to_selected(&row, adapter, runtime, install, source))
}

/// All enabled rows for the runtime (for `alf sync --all`). Same lazy init.
pub fn select_all_enabled(
    config: &mut Config,
    adapter: &dyn Adapter,
    runtime: &str,
    install: &Path,
) -> Result<Vec<SelectedAgent>> {
    lazy_init(config, adapter, runtime, install)?;

    let rows: Vec<AgentEntry> = config
        .agents_for_runtime(runtime)
        .into_iter()
        .filter(|r| r.enabled)
        .cloned()
        .collect();
    if rows.is_empty() {
        return Err(no_agents(runtime, config).into());
    }
    Ok(rows
        .iter()
        .map(|r| to_selected(r, adapter, runtime, install, SelectorSource::SoleEnabled))
        .collect())
}

/// Agent resolution for cloud ops (`alf restore` / `alf purge`).
///
/// A UUID argument that is not in the mapping passes through verbatim
/// (source `LegacyUuid` — preserves restore-by-UUID onto a fresh host); an
/// alias resolves via the mapping; no argument falls back `ALF_AGENT` → sole
/// enabled row → legacy [`state::resolve_agent_id`]`(None)` when the mapping
/// is empty. No lazy init — cloud ops need no workspace context (`_install`
/// is the seam WP1 threads per-agent paths through).
pub fn resolve_for_cloud_op(
    config: &mut Config,
    _adapter: &dyn Adapter,
    runtime: &str,
    _install: Option<&Path>,
    agent_arg: Option<&str>,
) -> Result<Uuid> {
    let selector = agent_arg.map(str::to_string).or_else(env_agent);
    if let Some(sel) = selector {
        if let Some(row) = config.find_agent(runtime, &sel) {
            return Ok(row.alf_agent_id);
        }
        if let Ok(id) = Uuid::parse_str(&sel) {
            return Ok(id); // SelectorSource::LegacyUuid
        }
        return Err(agent_not_found(runtime, &sel, config).into());
    }

    if config.agents_for_runtime(runtime).is_empty() {
        return state::resolve_agent_id(None);
    }
    Ok(sole_enabled_row(config, runtime)?.alf_agent_id)
}

/// Lenient selection for `alf vault add`/`encrypt` (no lazy init — no
/// workspace context): an explicit `--agent`/`ALF_AGENT` selector must
/// resolve (alias via mapping, unknown UUID passes through); with no selector
/// the sole enabled row applies, else `None` (the caller keeps its nil-UUID
/// default).
pub fn vault_default_agent_id(
    config: &Config,
    runtime: &str,
    agent_flag: Option<&str>,
) -> Result<Option<Uuid>> {
    let selector = agent_flag.map(str::to_string).or_else(env_agent);
    if let Some(sel) = selector {
        if let Some(row) = config.find_agent(runtime, &sel) {
            return Ok(Some(row.alf_agent_id));
        }
        if let Ok(id) = Uuid::parse_str(&sel) {
            return Ok(Some(id));
        }
        return Err(agent_not_found(runtime, &sel, config).into());
    }
    let enabled: Vec<&AgentEntry> = config
        .agents_for_runtime(runtime)
        .into_iter()
        .filter(|r| r.enabled)
        .collect();
    Ok((enabled.len() == 1).then(|| enabled[0].alf_agent_id))
}

/// The sync enabled-gate: refuse a disabled selection.
pub fn require_enabled_for_sync(selected: &SelectedAgent) -> Result<()> {
    if !selected.enabled {
        return Err(CliError {
            code: codes::AGENT_DISABLED,
            cause: format!(
                "Agent '{}' ({}) is disabled for sync.",
                selected.alias, selected.alf_agent_id
            ),
            remedy: format!("Run 'alf agents enable {}' to enable it.", selected.alias),
        }
        .into());
    }
    Ok(())
}

/// Resolve the workspace a command should operate on: an explicit `-w` wins
/// (with a mismatch warning when it differs from the selected binding's
/// workspace), else the selection's workspace. Returns the path plus whether
/// the flag overrode the selection (an ad-hoc target).
pub fn effective_workspace(
    selected: &SelectedAgent,
    workspace_flag: Option<&Path>,
) -> (PathBuf, bool) {
    match workspace_flag {
        Some(w) if w != selected.workspace.as_path() => {
            output::progress(&format!(
                "  ! -w path ({}) differs from agent '{}' mapped workspace ({}) — \
                 proceeding with the explicit -w path",
                w.display(),
                selected.alias,
                selected.workspace.display()
            ));
            (w.to_path_buf(), true)
        }
        Some(w) => (w.to_path_buf(), false),
        None => (selected.workspace.clone(), false),
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Non-empty `ALF_AGENT`, read manually (never via clap's `env` attr).
fn env_agent() -> Option<String> {
    std::env::var("ALF_AGENT").ok().filter(|s| !s.is_empty())
}

/// First-contact lazy init (user decision #2): an empty mapping for the
/// runtime runs the check pipeline. Persist only when the install exists
/// (same guard as `alf check`); for a nonexistent install the rows are kept
/// in memory so this invocation can still resolve, but nothing is saved.
fn lazy_init(
    config: &mut Config,
    adapter: &dyn Adapter,
    runtime: &str,
    install: &Path,
) -> Result<()> {
    if !config.agents_for_runtime(runtime).is_empty() {
        return Ok(());
    }
    let outcome = discovery::discover_and_reconcile(config, adapter, runtime, install)?;
    if install.is_dir() {
        discovery::persist(config, &outcome)?;
    } else {
        for row in &outcome.rows {
            if row.status == discovery::RowStatus::New {
                config.upsert_agent(row.entry.clone());
            }
        }
    }
    Ok(())
}

/// None-selector resolution: exactly one enabled row, or a coded error.
fn sole_enabled_row(config: &Config, runtime: &str) -> Result<AgentEntry> {
    let rows = config.agents_for_runtime(runtime);
    let enabled: Vec<&&AgentEntry> = rows.iter().filter(|r| r.enabled).collect();
    match enabled.len() {
        1 => Ok((*enabled[0]).clone()),
        0 => Err(no_agents(runtime, config).into()),
        n => {
            let aliases = enabled
                .iter()
                .map(|r| r.runtime_agent.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(CliError {
                code: codes::AGENT_SELECTION_AMBIGUOUS,
                cause: format!(
                    "{n} agents are enabled ({aliases}). Pass --agent <alias-or-id> \
                     or set the ALF_AGENT environment variable."
                ),
                remedy: "Run 'alf agents' to list agents, or 'alf sync --all' to sync \
                         every enabled agent."
                    .into(),
            }
            .into())
        }
    }
}

/// Build a [`SelectedAgent`] from a mapping row. The binding comes from a
/// fresh discovery pass when one matches the row (by runtime id or
/// workspace); otherwise it is synthesized from the row.
fn to_selected(
    row: &AgentEntry,
    adapter: &dyn Adapter,
    runtime: &str,
    install: &Path,
    source: SelectorSource,
) -> SelectedAgent {
    let binding = adapter
        .discover_agents(install)
        .ok()
        .and_then(|bindings| {
            bindings.into_iter().find(|b| {
                (b.runtime_agent_id.is_some() && b.runtime_agent_id == row.runtime_agent_id)
                    || b.workspace == Path::new(&row.workspace)
            })
        })
        .unwrap_or_else(|| AgentBinding {
            runtime_agent: row.runtime_agent.clone(),
            runtime_agent_id: row.runtime_agent_id.clone(),
            workspace: PathBuf::from(&row.workspace),
            memory_source: MemorySource::InWorkspaceFiles,
            default_enabled: row.enabled,
        });

    SelectedAgent {
        alf_agent_id: row.alf_agent_id,
        alias: row.runtime_agent.clone(),
        workspace: binding.workspace.clone(),
        runtime: runtime.to_string(),
        enabled: row.enabled,
        binding,
        source,
    }
}

/// Coded not-found error, listing known aliases with their enabled state.
pub(crate) fn agent_not_found(runtime: &str, selector: &str, config: &Config) -> CliError {
    let rows = config.agents_for_runtime(runtime);
    let known = if rows.is_empty() {
        "(none)".to_string()
    } else {
        rows.iter()
            .map(|r| {
                format!(
                    "{} ({})",
                    r.runtime_agent,
                    if r.enabled { "enabled" } else { "disabled" }
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    CliError {
        code: codes::AGENT_NOT_FOUND,
        cause: format!(
            "No agent named '{selector}' for runtime '{runtime}'. Known agents: {known}."
        ),
        remedy: "Run 'alf agents' to list agents, or 'alf check' to re-discover this install."
            .into(),
    }
}

/// Coded no-agents error: rows-but-none-enabled points at `alf agents
/// enable`; an empty mapping points at `alf check`.
fn no_agents(runtime: &str, config: &Config) -> CliError {
    let rows = config.agents_for_runtime(runtime);
    if rows.is_empty() {
        return CliError {
            code: codes::NO_AGENTS,
            cause: format!("No agents are mapped for runtime '{runtime}'."),
            remedy: "Run 'alf check' to discover agents.".into(),
        };
    }
    let aliases = rows
        .iter()
        .map(|r| r.runtime_agent.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    CliError {
        code: codes::NO_AGENTS,
        cause: format!("No agent is enabled for runtime '{runtime}' (mapped: {aliases})."),
        remedy: format!(
            "Run 'alf agents enable <alias>' to enable one (e.g. 'alf agents enable {}').",
            rows[0].runtime_agent
        ),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::tests::{RestoreEnv, HOME_LOCK};
    use adapter_openclaw::OpenClawAdapter;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::TempDir;

    fn uuid(n: u8) -> Uuid {
        let mut bytes = [0u8; 16];
        bytes[15] = n;
        Uuid::from_bytes(bytes)
    }

    fn entry(alias: &str, id: Uuid, workspace: &str, enabled: bool) -> AgentEntry {
        AgentEntry {
            runtime: Some("openclaw".into()),
            runtime_agent: alias.into(),
            runtime_agent_id: None,
            alf_agent_id: id,
            workspace: workspace.into(),
            enabled,
            extra: BTreeMap::new(),
        }
    }

    fn config_with(rows: Vec<AgentEntry>) -> Config {
        Config {
            agents: rows,
            ..Default::default()
        }
    }

    fn code_of(err: &anyhow::Error) -> &'static str {
        err.downcast_ref::<CliError>().expect("coded error").code
    }

    #[test]
    fn flag_wins_over_env() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        std::env::set_var("ALF_AGENT", "helper");

        let tmp = TempDir::new().unwrap();
        let mut config = config_with(vec![
            entry("main", uuid(1), "/ws-main", true),
            entry("helper", uuid(2), "/ws-helper", true),
        ]);
        let selected = select_current_agent(
            &mut config,
            &OpenClawAdapter,
            "openclaw",
            tmp.path(),
            Some("main"),
        )
        .unwrap();
        assert_eq!(selected.alf_agent_id, uuid(1));
        assert_eq!(selected.source, SelectorSource::Flag);
    }

    #[test]
    fn env_used_when_no_flag() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        std::env::set_var("ALF_AGENT", "helper");

        let tmp = TempDir::new().unwrap();
        let mut config = config_with(vec![
            entry("main", uuid(1), "/ws-main", true),
            entry("helper", uuid(2), "/ws-helper", true),
        ]);
        let selected =
            select_current_agent(&mut config, &OpenClawAdapter, "openclaw", tmp.path(), None)
                .unwrap();
        assert_eq!(selected.alf_agent_id, uuid(2));
        assert_eq!(selected.source, SelectorSource::Env);
    }

    #[test]
    fn none_resolves_sole_enabled() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        std::env::remove_var("ALF_AGENT");

        let tmp = TempDir::new().unwrap();
        let mut config = config_with(vec![
            entry("main", uuid(1), "/ws-main", true),
            entry("helper", uuid(2), "/ws-helper", false),
        ]);
        let selected =
            select_current_agent(&mut config, &OpenClawAdapter, "openclaw", tmp.path(), None)
                .unwrap();
        assert_eq!(selected.alf_agent_id, uuid(1));
        assert_eq!(selected.source, SelectorSource::SoleEnabled);
        assert_eq!(selected.alias, "main");
        assert_eq!(selected.workspace, PathBuf::from("/ws-main"));
    }

    /// The DoD None-with->1 case: the message must name --agent AND ALF_AGENT
    /// and carry the agent_selection_ambiguous code.
    #[test]
    fn none_with_multiple_enabled_errors_with_guidance() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        std::env::remove_var("ALF_AGENT");

        let tmp = TempDir::new().unwrap();
        let mut config = config_with(vec![
            entry("main", uuid(1), "/ws-main", true),
            entry("helper", uuid(2), "/ws-helper", true),
        ]);
        let err = select_current_agent(&mut config, &OpenClawAdapter, "openclaw", tmp.path(), None)
            .unwrap_err();
        assert_eq!(code_of(&err), codes::AGENT_SELECTION_AMBIGUOUS);
        let cli_err = err.downcast_ref::<CliError>().unwrap();
        assert!(cli_err.cause.contains("2 agents are enabled"));
        assert!(cli_err.cause.contains("main"));
        assert!(cli_err.cause.contains("helper"));
        assert!(cli_err.cause.contains("--agent"), "must name the flag");
        assert!(cli_err.cause.contains("ALF_AGENT"), "must name the env var");
        assert!(cli_err.remedy.contains("alf sync --all"));
    }

    #[test]
    fn none_with_rows_but_none_enabled_errors() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        std::env::remove_var("ALF_AGENT");

        let tmp = TempDir::new().unwrap();
        let mut config = config_with(vec![entry("main", uuid(1), "/ws-main", false)]);
        let err = select_current_agent(&mut config, &OpenClawAdapter, "openclaw", tmp.path(), None)
            .unwrap_err();
        assert_eq!(code_of(&err), codes::NO_AGENTS);
        let cli_err = err.downcast_ref::<CliError>().unwrap();
        assert!(cli_err.remedy.contains("alf agents enable"));
    }

    #[test]
    fn explicit_alias_resolves() {
        let tmp = TempDir::new().unwrap();
        let mut config = config_with(vec![
            entry("main", uuid(1), "/ws-main", true),
            entry("helper", uuid(2), "/ws-helper", false),
        ]);
        let selected = select_current_agent(
            &mut config,
            &OpenClawAdapter,
            "openclaw",
            tmp.path(),
            Some("helper"),
        )
        .unwrap();
        assert_eq!(selected.alf_agent_id, uuid(2));
    }

    #[test]
    fn explicit_uuid_resolves() {
        let tmp = TempDir::new().unwrap();
        let mut config = config_with(vec![
            entry("main", uuid(1), "/ws-main", true),
            entry("helper", uuid(2), "/ws-helper", true),
        ]);
        let selected = select_current_agent(
            &mut config,
            &OpenClawAdapter,
            "openclaw",
            tmp.path(),
            Some(&uuid(2).to_string()),
        )
        .unwrap();
        assert_eq!(selected.alf_agent_id, uuid(2));
        assert_eq!(selected.alias, "helper");
    }

    /// Selection ≠ sync-eligibility: a disabled agent is selectable (export,
    /// vault, …); only the sync gate refuses it.
    #[test]
    fn explicit_disabled_agent_selectable_for_export() {
        let tmp = TempDir::new().unwrap();
        let mut config = config_with(vec![entry("main", uuid(1), "/ws-main", false)]);
        let selected = select_current_agent(
            &mut config,
            &OpenClawAdapter,
            "openclaw",
            tmp.path(),
            Some("main"),
        )
        .unwrap();
        assert!(!selected.enabled);
    }

    #[test]
    fn sync_refuses_disabled_selection() {
        let tmp = TempDir::new().unwrap();
        let mut config = config_with(vec![entry("main", uuid(1), "/ws-main", false)]);
        let selected = select_current_agent(
            &mut config,
            &OpenClawAdapter,
            "openclaw",
            tmp.path(),
            Some("main"),
        )
        .unwrap();
        let err = require_enabled_for_sync(&selected).unwrap_err();
        assert_eq!(code_of(&err), codes::AGENT_DISABLED);
        let cli_err = err.downcast_ref::<CliError>().unwrap();
        assert!(cli_err.remedy.contains("alf agents enable main"));
    }

    #[test]
    fn unknown_name_error_lists_known_aliases() {
        let tmp = TempDir::new().unwrap();
        let mut config = config_with(vec![
            entry("main", uuid(1), "/ws-main", true),
            entry("helper", uuid(2), "/ws-helper", false),
        ]);
        let err = select_current_agent(
            &mut config,
            &OpenClawAdapter,
            "openclaw",
            tmp.path(),
            Some("ghost"),
        )
        .unwrap_err();
        assert_eq!(code_of(&err), codes::AGENT_NOT_FOUND);
        let cli_err = err.downcast_ref::<CliError>().unwrap();
        assert!(cli_err.cause.contains("main (enabled)"));
        assert!(cli_err.cause.contains("helper (disabled)"));
        assert!(cli_err.remedy.contains("alf agents"));
        assert!(cli_err.remedy.contains("alf check"));
    }

    /// A UUID that is not in the mapping passes through verbatim for cloud
    /// ops — restore-by-UUID onto a fresh host must keep working.
    #[test]
    fn cloud_op_uuid_passthrough_preserved() {
        let mut config = config_with(vec![entry("main", uuid(1), "/ws-main", true)]);
        let stranger = uuid(9);
        let resolved = resolve_for_cloud_op(
            &mut config,
            &OpenClawAdapter,
            "openclaw",
            None,
            Some(&stranger.to_string()),
        )
        .unwrap();
        assert_eq!(resolved, stranger);

        // An alias resolves through the mapping.
        let resolved = resolve_for_cloud_op(
            &mut config,
            &OpenClawAdapter,
            "openclaw",
            None,
            Some("main"),
        )
        .unwrap();
        assert_eq!(resolved, uuid(1));
    }

    /// Empty mapping + no selector falls back to the legacy sole-state-file
    /// resolution (pre-WP0 hosts keep working).
    #[test]
    fn cloud_op_legacy_fallback_sole_state_file() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        std::env::remove_var("ALF_AGENT");
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());

        let state_dir = tmp.path().join(".alf").join("state");
        fs::create_dir_all(&state_dir).unwrap();
        let id = uuid(7);
        fs::write(
            state_dir.join(format!("{id}.toml")),
            format!("agent_id = \"{id}\"\nlast_synced_sequence = 3\n"),
        )
        .unwrap();

        let mut config = Config::default();
        let resolved =
            resolve_for_cloud_op(&mut config, &OpenClawAdapter, "openclaw", None, None).unwrap();
        assert_eq!(resolved, id);
    }

    /// First contact writes the mapping once; a second selection resolves from
    /// the persisted rows without rewriting the config.
    #[test]
    fn empty_mapping_lazy_init_writes_rows_once() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        std::env::remove_var("ALF_AGENT");
        let home = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", home.path());

        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join(".alf-agent-id"), uuid(4).to_string()).unwrap();

        let mut config = Config::default();
        let selected =
            select_current_agent(&mut config, &OpenClawAdapter, "openclaw", &ws, None).unwrap();
        assert_eq!(selected.alf_agent_id, uuid(4), "adopts the workspace id");
        assert_eq!(config.agents.len(), 1);

        let config_path = home.path().join(".alf").join("config.toml");
        let first_write = fs::read_to_string(&config_path).unwrap();
        assert!(first_write.contains("[[agents]]"));

        // Second selection: rows already present — no rewrite.
        let mut config2 = Config::load_from(&config_path).unwrap();
        let selected2 =
            select_current_agent(&mut config2, &OpenClawAdapter, "openclaw", &ws, None).unwrap();
        assert_eq!(selected2.alf_agent_id, uuid(4));
        let second_write = fs::read_to_string(&config_path).unwrap();
        assert_eq!(first_write, second_write, "no rewrite on re-selection");
    }

    #[test]
    fn effective_workspace_flag_override_reports_adhoc() {
        let tmp = TempDir::new().unwrap();
        let mut config = config_with(vec![entry("main", uuid(1), "/ws-main", true)]);
        let selected = select_current_agent(
            &mut config,
            &OpenClawAdapter,
            "openclaw",
            tmp.path(),
            Some("main"),
        )
        .unwrap();

        let (ws, adhoc) = effective_workspace(&selected, None);
        assert_eq!(ws, PathBuf::from("/ws-main"));
        assert!(!adhoc);

        let (ws, adhoc) = effective_workspace(&selected, Some(Path::new("/ws-main")));
        assert_eq!(ws, PathBuf::from("/ws-main"));
        assert!(!adhoc);

        let (ws, adhoc) = effective_workspace(&selected, Some(Path::new("/elsewhere")));
        assert_eq!(ws, PathBuf::from("/elsewhere"));
        assert!(adhoc);
    }

    #[test]
    fn vault_default_agent_id_is_lenient_without_selector() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        std::env::remove_var("ALF_AGENT");

        // No mapping ⇒ None (caller keeps the nil-UUID default), never an error.
        let config = Config::default();
        assert_eq!(
            vault_default_agent_id(&config, "openclaw", None).unwrap(),
            None
        );

        // Sole enabled row ⇒ its id.
        let config = config_with(vec![entry("main", uuid(1), "/ws", true)]);
        assert_eq!(
            vault_default_agent_id(&config, "openclaw", None).unwrap(),
            Some(uuid(1))
        );

        // Explicit unknown alias must still error.
        let err = vault_default_agent_id(&config, "openclaw", Some("ghost")).unwrap_err();
        assert_eq!(code_of(&err), codes::AGENT_NOT_FOUND);
    }
}
