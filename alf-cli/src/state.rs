//! Per-agent sync state management.
//!
//! Each agent's sync cursor is stored in `~/.alf/state/{agent_id}.toml`.
//! This allows the CLI to track what has been synced and compute deltas
//! from the correct base.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::config::Config;

/// Sync state for a single agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentState {
    /// Agent identifier.
    pub agent_id: Uuid,

    /// Sequence number of the last successful sync.
    pub last_synced_sequence: u64,

    /// When the last sync completed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<DateTime<Utc>>,

    /// Path to the last exported snapshot (used as delta base).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_path: Option<String>,
}

impl AgentState {
    /// Create a new state for an agent that has never synced.
    pub fn new(agent_id: Uuid) -> Self {
        Self {
            agent_id,
            last_synced_sequence: 0,
            last_synced_at: None,
            snapshot_path: None,
        }
    }

    /// Returns the state directory (`~/.alf/state/`).
    pub fn state_dir() -> Result<PathBuf> {
        Ok(Config::dir()?.join("state"))
    }

    /// Returns the path to this agent's state file.
    pub fn path_for(agent_id: Uuid) -> Result<PathBuf> {
        Ok(Self::state_dir()?.join(format!("{agent_id}.toml")))
    }

    /// Load state for an agent, or return a fresh state if no file exists.
    pub fn load(agent_id: Uuid) -> Result<Self> {
        let path = Self::path_for(agent_id)?;
        Self::load_from(&path, agent_id)
    }

    /// Load state from a specific path.
    pub fn load_from(path: &Path, agent_id: Uuid) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new(agent_id));
        }
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read state from {}", path.display()))?;
        let state: AgentState = toml::from_str(&content)
            .with_context(|| format!("Failed to parse state at {}", path.display()))?;
        Ok(state)
    }

    /// Save state to `~/.alf/state/{agent_id}.toml`.
    pub fn save(&self) -> Result<()> {
        let path = Self::path_for(self.agent_id)?;
        self.save_to(&path)
    }

    /// Save state to a specific path, creating parent directories if needed.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }
        let content = toml::to_string_pretty(self).context("Failed to serialize state")?;
        fs::write(path, content)
            .with_context(|| format!("Failed to write state to {}", path.display()))?;
        Ok(())
    }

    /// Delete this agent's state file.
    #[allow(dead_code)]
    pub fn delete(agent_id: Uuid) -> Result<()> {
        let path = Self::path_for(agent_id)?;
        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("Failed to delete state at {}", path.display()))?;
        }
        Ok(())
    }

    /// Whether this agent has been synced before.
    pub fn has_synced(&self) -> bool {
        self.last_synced_sequence > 0
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
    fn new_state_defaults() {
        let id = Uuid::new_v4();
        let state = AgentState::new(id);
        assert_eq!(state.agent_id, id);
        assert_eq!(state.last_synced_sequence, 0);
        assert!(state.last_synced_at.is_none());
        assert!(state.snapshot_path.is_none());
        assert!(!state.has_synced());
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent.toml");

        let state = AgentState {
            agent_id: Uuid::new_v4(),
            last_synced_sequence: 42,
            last_synced_at: Some(Utc::now()),
            snapshot_path: Some("/tmp/test.alf".into()),
        };

        state.save_to(&path).unwrap();
        let loaded = AgentState::load_from(&path, state.agent_id).unwrap();
        assert_eq!(state.agent_id, loaded.agent_id);
        assert_eq!(state.last_synced_sequence, loaded.last_synced_sequence);
        assert_eq!(state.snapshot_path, loaded.snapshot_path);
        assert!(loaded.has_synced());
    }

    #[test]
    fn load_missing_returns_fresh() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.toml");
        let id = Uuid::new_v4();

        let state = AgentState::load_from(&path, id).unwrap();
        assert_eq!(state, AgentState::new(id));
    }

    #[test]
    fn save_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join("dir").join("agent.toml");

        let state = AgentState::new(Uuid::new_v4());
        state.save_to(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn state_toml_format() {
        let state = AgentState {
            agent_id: Uuid::new_v4(),
            last_synced_sequence: 5,
            last_synced_at: None,
            snapshot_path: None,
        };
        let toml_str = toml::to_string_pretty(&state).unwrap();
        assert!(toml_str.contains("last_synced_sequence = 5"));
        assert!(toml_str.contains("agent_id"));
        // Optional None fields should be absent
        assert!(!toml_str.contains("last_synced_at"));
        assert!(!toml_str.contains("snapshot_path"));
    }
}