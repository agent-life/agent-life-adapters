//! Per-agent sync state management.
//!
//! Each agent's sync cursor is stored in `~/.alf/state/{agent_id}.toml`.
//! See [`docs/how_alf_syncs.md`] for the full data model and branch table.
//!
//! # Sync-control invariant
//!
//! `last_synced_sequence` is **the** sync-control variable: the only field any
//! control flow in the CLI is allowed to read. `last_synced_at` is purely
//! informational metadata — written on every save, displayed in `alf help status`,
//! and propagated into delta manifests as `base_timestamp` — but no `if`/`match`/`while`
//! is allowed to gate behaviour on it. A CI grep guard enforces this.

use crate::config::Config;
use crate::fs_private::write_private_atomic;

use anyhow::bail;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Sync state for a single agent.
///
/// Persisted to `~/.alf/state/{agent_id}.toml`. The state file and the
/// local base snapshot (`{agent_id}-snapshot.alf`) are independent artifacts;
/// see [`local_base_exists`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentState {
    /// Agent identifier.
    pub agent_id: Uuid,

    /// **The** sync-control variable. The only field any control flow reads.
    ///
    /// `None` ⇒ the agent has never completed a sync.
    /// `Some(N)` ⇒ the cloud assigned us sequence `N` at the last successful sync
    /// (`Some(0)` after the first snapshot upload; `Some(K)` after K-many delta pushes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_sequence: Option<u64>,

    /// Informational metadata. Stamped on every save, displayed in `alf help status`,
    /// propagated into delta manifests as `base_timestamp`. **Never branched on.**
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<DateTime<Utc>>,
}

impl AgentState {
    /// Create a new state for an agent that has never synced.
    pub fn new(agent_id: Uuid) -> Self {
        Self {
            agent_id,
            last_synced_sequence: None,
            last_synced_at: None,
        }
    }

    /// Returns the state directory (`~/.alf/state/`).
    pub fn state_dir() -> Result<PathBuf> {
        Ok(Config::dir()?.join("state"))
    }

    /// Load state for an agent, or return a fresh state if no file exists.
    pub fn load(agent_id: Uuid) -> Result<Self> {
        let path = state_file_path(agent_id)?;
        Self::load_from(&path, agent_id)
    }

    /// Load state from a specific path. Forward-compatible: unknown TOML keys
    /// (e.g. the legacy `snapshot_path` from pre-0.1.4 files) are silently ignored.
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
        let path = state_file_path(self.agent_id)?;
        self.save_to(&path)
    }

    /// Save state to a specific path, creating parent directories if needed.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }
        let content = toml::to_string_pretty(self).context("Failed to serialize state")?;
        // Atomic temp+rename (WP-M3 review B1): the watch loop makes autonomous
        // syncs reachable, and the host may SIGKILL `alf mcp serve` at any moment
        // (design §5.3 treats this as normal). A truncating write could leave a
        // torn `state.toml`; the atomic rename guarantees the reader sees either
        // the old or the new file, never a partial mix.
        write_private_atomic(path, &content)
            .with_context(|| format!("Failed to write state to {}", path.display()))?;
        Ok(())
    }

    /// Delete this agent's state file.
    pub fn delete(agent_id: Uuid) -> Result<()> {
        let path = state_file_path(agent_id)?;
        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("Failed to delete state at {}", path.display()))?;
        }
        Ok(())
    }
}

/// Path to the state file for an agent (`~/.alf/state/{agent_id}.toml`).
pub fn state_file_path(agent_id: Uuid) -> Result<PathBuf> {
    Ok(AgentState::state_dir()?.join(format!("{agent_id}.toml")))
}

/// Path to the local base snapshot for an agent (`~/.alf/state/{agent_id}-snapshot.alf`).
///
/// This file is the frozen base used to compute the next delta. It is written
/// by `alf sync` and `alf restore` (and `alf sync --recover`); it is read only
/// as the previous-snapshot input to delta computation.
pub fn local_base_path(agent_id: Uuid) -> Result<PathBuf> {
    Ok(AgentState::state_dir()?.join(format!("{agent_id}-snapshot.alf")))
}

/// Whether the local base snapshot exists for this agent.
///
/// Cheap, side-effect-free disk check. Used as the secondary control-flow input
/// in `alf sync` (see [`docs/how_alf_syncs.md`]).
pub fn local_base_exists(agent_id: Uuid) -> Result<bool> {
    Ok(local_base_path(agent_id)?.is_file())
}

/// IDs of every agent tracked in `~/.alf/state/*.toml`.
///
/// Used by discovery's allocation context (adopting a pre-WP0 synced agent's
/// cloud identity on first contact) and by [`resolve_agent_id`].
pub fn tracked_agent_ids() -> Result<Vec<Uuid>> {
    let state_dir = AgentState::state_dir()?;
    let mut ids = Vec::new();

    if state_dir.is_dir() {
        let entries = fs::read_dir(&state_dir)
            .with_context(|| format!("Failed to read state directory {}", state_dir.display()))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if let Ok(id) = Uuid::parse_str(stem) {
                    ids.push(id);
                }
            }
        }
    }

    Ok(ids)
}

/// Resolve an agent ID from an optional CLI argument or from the state directory.
///
/// If `agent_arg` is `Some`, this validates and parses it as a UUID.
/// If `None`, this looks at `~/.alf/state/*.toml`:
/// - If exactly one agent is tracked, its ID is returned.
/// - If zero or multiple agents are tracked, an error is returned asking for `--agent`.
///
/// This is the mapping-empty cloud-op fallback used by
/// `selector::resolve_for_cloud_op` — it keeps restore-by-state working on
/// hosts that synced before the `[[agents]]` mapping existed.
pub fn resolve_agent_id(agent_arg: Option<&str>) -> Result<Uuid> {
    if let Some(id_str) = agent_arg {
        return Uuid::parse_str(id_str)
            .with_context(|| format!("Invalid agent ID: '{id_str}'. Expected a UUID."));
    }

    let state_dir = AgentState::state_dir()?;
    let ids = tracked_agent_ids()?;

    match ids.len() {
        0 => bail!(
            "No agent ID specified and no agents are tracked in {}. \
             Run `alf sync` first or pass --agent <agent-id>.",
            state_dir.display()
        ),
        1 => Ok(ids[0]),
        _ => bail!(
            "No agent ID specified and multiple agents are tracked in {}. \
             Pass --agent <agent-id> to disambiguate.",
            state_dir.display()
        ),
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
        assert!(state.last_synced_sequence.is_none());
        assert!(state.last_synced_at.is_none());
    }

    #[test]
    fn save_and_load_round_trip_with_sequence_and_timestamp() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent.toml");

        let state = AgentState {
            agent_id: Uuid::new_v4(),
            last_synced_sequence: Some(42),
            last_synced_at: Some(Utc::now()),
        };

        state.save_to(&path).unwrap();
        let loaded = AgentState::load_from(&path, state.agent_id).unwrap();
        assert_eq!(state.agent_id, loaded.agent_id);
        assert_eq!(state.last_synced_sequence, loaded.last_synced_sequence);
        assert_eq!(state.last_synced_at, loaded.last_synced_at);
    }

    #[test]
    fn round_trip_without_sequence_stays_none() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent.toml");
        let id = Uuid::new_v4();

        let state = AgentState::new(id);
        state.save_to(&path).unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("last_synced_sequence"),
            "unsynced state must omit last_synced_sequence; got: {raw}"
        );

        let loaded = AgentState::load_from(&path, id).unwrap();
        assert!(loaded.last_synced_sequence.is_none());
        assert!(loaded.last_synced_at.is_none());
    }

    #[test]
    fn load_missing_returns_fresh() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.toml");
        let id = Uuid::new_v4();

        let state = AgentState::load_from(&path, id).unwrap();
        assert_eq!(state, AgentState::new(id));
        assert!(state.last_synced_sequence.is_none());
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
            last_synced_sequence: Some(5),
            last_synced_at: None,
        };
        let toml_str = toml::to_string_pretty(&state).unwrap();
        assert!(toml_str.contains("last_synced_sequence = 5"));
        assert!(toml_str.contains("agent_id"));
        assert!(!toml_str.contains("last_synced_at"));
        assert!(!toml_str.contains("snapshot_path"));
    }

    #[test]
    fn loads_legacy_state_file_with_snapshot_path() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent.toml");
        let id = Uuid::parse_str("00000000-0000-0000-0000-000000000042").unwrap();

        let legacy = format!(
            r#"agent_id = "{id}"
last_synced_sequence = 7
last_synced_at = "2026-01-15T12:00:00Z"
snapshot_path = "/tmp/legacy-snapshot.alf"
"#
        );
        fs::write(&path, legacy).unwrap();

        let loaded = AgentState::load_from(&path, id).unwrap();
        assert_eq!(loaded.agent_id, id);
        assert_eq!(loaded.last_synced_sequence, Some(7));
        assert!(loaded.last_synced_at.is_some());
    }

    #[test]
    fn loads_legacy_first_sync_state_file() {
        // A legacy state.toml from the pre-0.1.4 CLI right after a first sync:
        // last_synced_sequence = 0 (not None — pre-0.1.4 didn't have the Option).
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent.toml");
        let id = Uuid::parse_str("00000000-0000-0000-0000-000000000043").unwrap();

        let legacy = format!(
            r#"agent_id = "{id}"
last_synced_sequence = 0
last_synced_at = "2026-01-15T12:00:00Z"
snapshot_path = "/tmp/legacy-snapshot.alf"
"#
        );
        fs::write(&path, legacy).unwrap();

        let loaded = AgentState::load_from(&path, id).unwrap();
        assert_eq!(
            loaded.last_synced_sequence,
            Some(0),
            "post-first-sync legacy file must load as Some(0), not None"
        );
    }

    #[test]
    fn local_base_path_format() {
        // HOME_LOCK: the path derives from ALF_HOME/HOME, which other tests
        // mutate under this lock — read-side must serialize too.
        let _guard = crate::context::tests::HOME_LOCK.lock().unwrap();
        let id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let base = local_base_path(id).unwrap();
        assert!(base.ends_with(format!("{id}-snapshot.alf")));
    }

    #[test]
    fn local_base_exists_reports_disk_state() {
        let _guard = crate::context::tests::HOME_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());

        let id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let result_absent = local_base_exists(id).unwrap();
        let base = local_base_path(id).unwrap();
        fs::create_dir_all(base.parent().unwrap()).unwrap();
        fs::write(&base, b"PK").unwrap();
        let result_present = local_base_exists(id).unwrap();

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }

        assert!(!result_absent);
        assert!(result_present);
    }

    #[test]
    fn state_file_and_local_base_paths_share_state_dir() {
        // HOME_LOCK: both calls re-read ALF_HOME/HOME; without the lock a
        // concurrent env-mutating test can flip the home between them.
        let _guard = crate::context::tests::HOME_LOCK.lock().unwrap();
        let id = Uuid::new_v4();
        let s = state_file_path(id).unwrap();
        let b = local_base_path(id).unwrap();
        assert_eq!(s.parent(), b.parent());
    }

    /// CI guard: codifies the "last_synced_at is informational metadata, never
    /// read by control flow" invariant. Scans every `.rs` file in `alf-cli/src/`
    /// and asserts that the substring `last_synced_at` does not appear inside an
    /// `if`/`match`/`while` condition.
    ///
    /// Allowed uses (none of which match the regex):
    /// - field declarations and writes (`last_synced_at: Some(...)`)
    /// - display formatting (`a.last_synced_at.as_deref().unwrap_or("(never)")`)
    /// - propagation into delta manifests (`base_timestamp: state.last_synced_at`)
    ///
    /// Disallowed uses (the regex catches these):
    /// - `if state.last_synced_at.is_some() { ... }`
    /// - `match state.last_synced_at { ... }`
    /// - `while state.last_synced_at.is_some() { ... }`
    #[test]
    fn no_control_flow_branches_on_last_synced_at() {
        // Scan every .rs file under alf-cli/src/ for forbidden patterns.
        // CARGO_MANIFEST_DIR points at alf-cli/ when this test runs.
        let crate_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let src_dir = crate_root.join("src");

        let forbidden = [
            "if state.last_synced_at",
            "if !state.last_synced_at",
            "if last_synced_at",
            "match state.last_synced_at",
            "match last_synced_at",
            "while state.last_synced_at",
        ];

        let mut offenders = Vec::new();
        visit_rs_files(&src_dir, &mut |path, contents| {
            for (lineno, line) in contents.lines().enumerate() {
                let trimmed = line.trim_start();
                // Skip comments and doc strings — those describe the rule itself.
                if trimmed.starts_with("//") || trimmed.starts_with("///") {
                    continue;
                }
                for needle in &forbidden {
                    if !line.contains(needle) {
                        continue;
                    }
                    // Skip the pattern when it appears as a string literal on
                    // the same line (e.g. the `forbidden` array in this very
                    // test). Real control-flow uses are never quoted.
                    let quoted = format!("\"{needle}\"");
                    if line.contains(&quoted) {
                        continue;
                    }
                    offenders.push(format!(
                        "{}:{}: forbidden pattern `{}`: {}",
                        path.display(),
                        lineno + 1,
                        needle,
                        line.trim()
                    ));
                }
            }
        });

        assert!(
            offenders.is_empty(),
            "control flow must not branch on last_synced_at:\n  {}",
            offenders.join("\n  ")
        );
    }

    fn visit_rs_files(dir: &std::path::Path, cb: &mut dyn FnMut(&std::path::Path, &str)) {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit_rs_files(&path, cb);
            } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                if let Ok(contents) = fs::read_to_string(&path) {
                    cb(&path, &contents);
                }
            }
        }
    }

    #[test]
    #[cfg(unix)]
    fn save_to_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent.toml");
        let state = AgentState::new(Uuid::new_v4());
        state.save_to(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
