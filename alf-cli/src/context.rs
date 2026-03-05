//! Runtime context for the CLI: config path, state directory, tracked agents.
//!
//! Used by the help system to report "where things are" and suggest next steps.

use anyhow::Result;
use serde::Serialize;
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

use crate::config::Config;
use crate::state::AgentState;

/// Summary of the current alf environment (config, state dir, tracked agents).
#[derive(Debug, Clone, Serialize)]
pub struct StatusSummary {
    /// Path to the config directory (~/.alf/).
    pub config_dir: PathBuf,

    /// Path to the config file (~/.alf/config.toml).
    pub config_path: PathBuf,

    /// Whether the config file exists.
    pub config_exists: bool,

    /// Whether an API key is set (never exposes the key).
    pub api_key_set: bool,

    /// Path to the state directory (~/.alf/state/).
    pub state_dir: PathBuf,

    /// Whether the state directory exists.
    pub state_dir_exists: bool,

    /// Tracked agents (from state *.toml files).
    pub agents: Vec<AgentSummary>,
}

/// Per-agent summary from state file and optional snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct AgentSummary {
    pub agent_id: Uuid,
    pub last_synced_sequence: u64,
    pub last_synced_at: Option<String>,
    pub snapshot_exists: bool,
}

/// Gather current config and state into a summary. Does not fail on missing
/// files; reports what exists.
pub fn gather_status() -> Result<StatusSummary> {
    let config_dir = Config::dir()?;
    let config_path = Config::path()?;
    let config = Config::load()?;
    let config_exists = config_path.is_file();
    let api_key_set = !config.service.api_key.is_empty();

    let state_dir = AgentState::state_dir()?;
    let state_dir_exists = state_dir.is_dir();

    let mut agents = Vec::new();
    if state_dir_exists {
        let entries = fs::read_dir(&state_dir);
        if let Ok(entries) = entries {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                    continue;
                }
                let stem = match path.file_stem().and_then(|s| s.to_str()) {
                    Some(s) => s,
                    None => continue,
                };
                let agent_id = match Uuid::parse_str(stem) {
                    Ok(id) => id,
                    Err(_) => continue,
                };

                let state = AgentState::load(agent_id).unwrap_or_else(|_| AgentState::new(agent_id));
                let snapshot_path = state_dir.join(format!("{agent_id}-snapshot.alf"));
                let snapshot_exists = snapshot_path.is_file();

                agents.push(AgentSummary {
                    agent_id,
                    last_synced_sequence: state.last_synced_sequence,
                    last_synced_at: state
                        .last_synced_at
                        .map(|dt| dt.to_rfc3339()),
                    snapshot_exists,
                });
            }
        }
    }

    Ok(StatusSummary {
        config_dir,
        config_path,
        config_exists,
        api_key_set,
        state_dir,
        state_dir_exists,
        agents,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::io::Write;
    use tempfile::TempDir;

    /// Restore HOME when dropped (for tests that set HOME to a temp dir).
    struct RestoreHome(Option<std::ffi::OsString>);

    impl Drop for RestoreHome {
        fn drop(&mut self) {
            if let Some(ref v) = self.0 {
                env::set_var("HOME", v);
            } else {
                env::remove_var("HOME");
            }
        }
    }

    #[test]
    fn gather_status_no_config_uses_defaults() {
        let tmp = TempDir::new().unwrap();
        let _restore = RestoreHome(env::var_os("HOME"));
        env::set_var("HOME", tmp.path());

        let status = gather_status().unwrap();

        assert!(!status.config_exists);
        assert!(!status.api_key_set);
        assert_eq!(status.agents.len(), 0);
        assert!(status.config_path.ends_with(".alf/config.toml") || status.config_path.to_string_lossy().contains(".alf"));
        assert!(status.state_dir.ends_with(".alf/state") || status.state_dir.to_string_lossy().contains(".alf"));
    }

    #[test]
    fn gather_status_with_config_and_state() {
        let tmp = TempDir::new().unwrap();
        let _restore = RestoreHome(env::var_os("HOME"));
        env::set_var("HOME", tmp.path());

        let alf = tmp.path().join(".alf");
        let state_dir = alf.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(alf.join("config.toml"), "[service]\napi_key = \"test-key\"\n").unwrap();

        // Use a distinct test UUID to avoid colliding with real ~/.alf/state when tests run in parallel.
        let agent_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let state_toml = state_dir.join(format!("{}.toml", agent_id));
        std::fs::write(
            &state_toml,
            "agent_id = \"00000000-0000-0000-0000-000000000001\"\nlast_synced_sequence = 2\nlast_synced_at = \"2026-01-15T12:00:00Z\"\n",
        ).unwrap();
        let snapshot_path = state_dir.join(format!("{}-snapshot.alf", agent_id));
        let mut f = std::fs::File::create(&snapshot_path).unwrap();
        f.write_all(b"PK").unwrap(); // minimal zip bytes
        drop(f);

        let status = gather_status().unwrap();

        assert!(status.config_exists);
        assert!(status.api_key_set);
        assert!(status.state_dir_exists);
        assert_eq!(status.agents.len(), 1);
        assert_eq!(status.agents[0].agent_id, agent_id);
        assert_eq!(status.agents[0].last_synced_sequence, 2);
        assert!(status.agents[0].last_synced_at.is_some());
        assert!(status.agents[0].snapshot_exists);
    }
}
