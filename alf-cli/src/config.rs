//! Configuration management for `~/.alf/config.toml`.

use crate::fs_private::write_private;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Top-level config file structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub service: ServiceConfig,

    #[serde(default)]
    pub defaults: DefaultsConfig,

    /// Discovered-agent mapping (`[[agents]]`). Must stay the last field so
    /// the TOML array-of-tables serializes after `[service]`/`[defaults]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<AgentEntry>,
}

/// One row of the discovered-agent mapping. Maintained by `alf check` /
/// `alf agents` / selector first-contact; users edit `enabled` only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentEntry {
    /// Reserved for multi-runtime hosts; None ⇒ `[defaults].runtime`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    pub runtime_agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_agent_id: Option<String>,
    pub alf_agent_id: Uuid,
    pub workspace: String,
    pub enabled: bool,
    /// Unknown keys preserved across rewrites (forward compat).
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, toml::Value>,
}

/// Service connection settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServiceConfig {
    #[serde(default = "default_api_url")]
    pub api_url: String,

    #[serde(default)]
    pub api_key: String,
}

/// Default values for CLI flags.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DefaultsConfig {
    #[serde(default = "default_runtime")]
    pub runtime: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

fn default_api_url() -> String {
    "https://api.agent-life.ai".into()
}

fn default_runtime() -> String {
    "openclaw".into()
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            api_url: default_api_url(),
            api_key: String::new(),
        }
    }
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            runtime: default_runtime(),
            workspace: None,
        }
    }
}

impl Config {
    /// Returns the path to the config directory (`~/.alf/`, or `$ALF_HOME/.alf`).
    pub fn dir() -> Result<PathBuf> {
        let home = alf_core::home_dir().context("Could not determine home directory")?;
        Ok(home.join(".alf"))
    }

    /// Returns the path to the config file (`~/.alf/config.toml`).
    pub fn path() -> Result<PathBuf> {
        Ok(Self::dir()?.join("config.toml"))
    }

    /// Resolve the runtime: the CLI flag if given, else `[defaults] runtime`
    /// (which itself defaults to `"openclaw"`). Never fails.
    pub fn resolve_runtime(&self, flag: Option<String>) -> String {
        flag.unwrap_or_else(|| self.defaults.runtime.clone())
    }

    /// Resolve the workspace: the CLI flag if given, else `[defaults] workspace`,
    /// else an actionable error naming the config file.
    pub fn resolve_workspace(&self, flag: Option<PathBuf>) -> Result<PathBuf> {
        if let Some(w) = flag {
            return Ok(w);
        }
        match self.defaults.workspace.as_deref().filter(|s| !s.is_empty()) {
            Some(w) => Ok(PathBuf::from(w)),
            None => {
                let cfg = Self::path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "~/.alf/config.toml".to_string());
                anyhow::bail!(
                    "No workspace specified. Pass -w <path> or set [defaults] workspace in {cfg}."
                )
            }
        }
    }

    /// Load config from disk, or return defaults if the file doesn't exist.
    ///
    /// Does not create the file if missing — call [`save`](Config::save)
    /// explicitly if you want to persist defaults.
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        Self::load_from(&path)
    }

    /// Load config from a specific path. Returns defaults if the file
    /// doesn't exist. Falls back to `ALF_API_KEY` env var if no key in file.
    pub fn load_from(path: &Path) -> Result<Self> {
        let mut config = if !path.exists() {
            Self::default()
        } else {
            let content = fs::read_to_string(path)
                .with_context(|| format!("Failed to read config from {}", path.display()))?;
            toml::from_str(&content)
                .with_context(|| format!("Failed to parse config at {}", path.display()))?
        };

        if config.service.api_key.is_empty() {
            // Unsynchronized env read: fine for a single-process CLI. Tests that
            // flip HOME / ALF_API_KEY use HOME_LOCK; do not use shared mutable env
            // from multiple threads in production.
            if let Ok(key) = std::env::var("ALF_API_KEY") {
                if !key.is_empty() {
                    config.service.api_key = key;
                }
            }
        }

        Ok(config)
    }

    /// Save the config to `~/.alf/config.toml`, creating the directory
    /// if needed.
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        self.save_to(&path)
    }

    /// Rows of the `[[agents]]` mapping that belong to `runtime` (row runtime
    /// matches, or is None — legacy/hand-written rows default to any runtime).
    pub fn agents_for_runtime(&self, runtime: &str) -> Vec<&AgentEntry> {
        self.agents
            .iter()
            .filter(|a| a.runtime.as_deref().map(|r| r == runtime).unwrap_or(true))
            .collect()
    }

    /// Find a mapping row by alias-or-id within `runtime`'s rows: a selector
    /// that parses as a UUID matches `alf_agent_id`, otherwise it must equal
    /// the `runtime_agent` alias exactly.
    pub fn find_agent(&self, runtime: &str, selector: &str) -> Option<&AgentEntry> {
        let rows = self.agents_for_runtime(runtime);
        if let Ok(id) = Uuid::parse_str(selector) {
            if let Some(row) = rows.iter().find(|a| a.alf_agent_id == id) {
                return Some(row);
            }
        }
        rows.into_iter().find(|a| a.runtime_agent == selector)
    }

    /// Insert or replace a mapping row, keyed by `alf_agent_id`.
    pub fn upsert_agent(&mut self, entry: AgentEntry) {
        match self
            .agents
            .iter_mut()
            .find(|a| a.alf_agent_id == entry.alf_agent_id)
        {
            Some(existing) => *existing = entry,
            None => self.agents.push(entry),
        }
    }

    /// Flip a row's `enabled` flag (idempotent) and return the updated row.
    /// Errors when no row matches the selector.
    pub fn set_agent_enabled(
        &mut self,
        runtime: &str,
        selector: &str,
        enabled: bool,
    ) -> Result<AgentEntry> {
        let id = match self.find_agent(runtime, selector) {
            Some(row) => row.alf_agent_id,
            None => anyhow::bail!("No agent matching '{selector}' for runtime '{runtime}'"),
        };
        let row = self
            .agents
            .iter_mut()
            .find(|a| a.alf_agent_id == id)
            .expect("row found by find_agent");
        row.enabled = enabled;
        Ok(row.clone())
    }

    /// Save the config to a specific path, creating parent directories
    /// if needed.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }
        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;
        write_private(path, &content)
            .with_context(|| format!("Failed to write config to {}", path.display()))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::tests::HOME_LOCK;
    use tempfile::TempDir;

    #[test]
    fn default_config() {
        let config = Config::default();
        assert_eq!(config.service.api_url, "https://api.agent-life.ai");
        assert_eq!(config.service.api_key, "");
        assert_eq!(config.defaults.runtime, "openclaw");
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        let config = Config {
            service: ServiceConfig {
                api_url: "https://custom.api.example.com".into(),
                api_key: "sk-test-12345".into(),
            },
            defaults: DefaultsConfig {
                runtime: "zeroclaw".into(),
                workspace: Some("/home/user/.openclaw/workspace".into()),
            },
            agents: Vec::new(),
        };

        config.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(config, loaded);
    }

    #[test]
    fn load_missing_file_returns_defaults() {
        let _lock = HOME_LOCK.lock().unwrap();
        std::env::remove_var("ALF_API_KEY");

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.toml");

        let config = Config::load_from(&path).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn load_partial_config_fills_defaults() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        // Only service section, no defaults section
        fs::write(&path, "[service]\napi_key = \"my-key\"\n").unwrap();

        let config = Config::load_from(&path).unwrap();
        assert_eq!(config.service.api_key, "my-key");
        assert_eq!(config.service.api_url, "https://api.agent-life.ai"); // default
        assert_eq!(config.defaults.runtime, "openclaw"); // default
    }

    #[test]
    fn save_creates_parent_directories() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join("dir").join("config.toml");

        let config = Config::default();
        config.save_to(&path).unwrap();

        assert!(path.exists());
    }

    #[test]
    fn config_toml_format() {
        let config = Config::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();

        // Should contain our sections
        assert!(toml_str.contains("[service]"));
        assert!(toml_str.contains("[defaults]"));
        assert!(toml_str.contains("api_url"));
        assert!(toml_str.contains("runtime"));
    }

    #[test]
    fn env_var_fallback_when_no_key_in_file() {
        let _lock = HOME_LOCK.lock().unwrap();

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        fs::write(
            &path,
            "[service]\napi_url = \"https://api.agent-life.ai\"\n",
        )
        .unwrap();

        std::env::set_var("ALF_API_KEY", "alf_sk_from_env");
        let config = Config::load_from(&path).unwrap();
        std::env::remove_var("ALF_API_KEY");

        assert_eq!(config.service.api_key, "alf_sk_from_env");
    }

    #[test]
    fn file_key_takes_precedence_over_env_var() {
        let _lock = HOME_LOCK.lock().unwrap();

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        fs::write(&path, "[service]\napi_key = \"from_file\"\n").unwrap();

        std::env::set_var("ALF_API_KEY", "from_env");
        let config = Config::load_from(&path).unwrap();
        std::env::remove_var("ALF_API_KEY");

        assert_eq!(config.service.api_key, "from_file");
    }

    #[test]
    #[cfg(unix)]
    fn save_to_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        Config::default().save_to(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn resolve_runtime_flag_wins() {
        assert_eq!(
            Config::default().resolve_runtime(Some("zeroclaw".into())),
            "zeroclaw"
        );
    }

    #[test]
    fn resolve_runtime_falls_back_to_default() {
        assert_eq!(Config::default().resolve_runtime(None), "openclaw");
        let mut custom = Config::default();
        custom.defaults.runtime = "zeroclaw".into();
        assert_eq!(custom.resolve_runtime(None), "zeroclaw");
    }

    #[test]
    fn resolve_workspace_flag_wins() {
        let ws = Config::default()
            .resolve_workspace(Some(PathBuf::from("/tmp/ws")))
            .unwrap();
        assert_eq!(ws, PathBuf::from("/tmp/ws"));
    }

    #[test]
    fn resolve_workspace_uses_default() {
        let mut config = Config::default();
        config.defaults.workspace = Some("/tmp/default-ws".into());
        assert_eq!(
            config.resolve_workspace(None).unwrap(),
            PathBuf::from("/tmp/default-ws")
        );
    }

    #[test]
    fn resolve_workspace_errors_when_unset() {
        let err = Config::default().resolve_workspace(None).unwrap_err();
        assert!(err.to_string().contains("workspace"));
    }

    // -----------------------------------------------------------------------
    // [[agents]] mapping (WP0)
    // -----------------------------------------------------------------------

    fn agent_entry(alias: &str, id: &str, workspace: &str, enabled: bool) -> AgentEntry {
        AgentEntry {
            runtime: Some("openclaw".into()),
            runtime_agent: alias.into(),
            runtime_agent_id: None,
            alf_agent_id: Uuid::parse_str(id).unwrap(),
            workspace: workspace.into(),
            enabled,
            extra: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn agents_mapping_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        let mut config = Config::default();
        let mut row_a = agent_entry(
            "main",
            "cfef1150-0000-4000-8000-0000000000aa",
            "/home/u/.openclaw/workspace",
            true,
        );
        // Unknown key must survive a save/load cycle via `extra`.
        row_a
            .extra
            .insert("future_key".into(), toml::Value::String("preserved".into()));
        let mut row_b = agent_entry(
            "helper",
            "cfef1150-0000-4000-8000-0000000000bb",
            "/home/u/.openclaw/workspace-helper",
            false,
        );
        row_b.runtime_agent_id = None; // explicit: optional field omitted
        config.agents = vec![row_a.clone(), row_b.clone()];

        config.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.agents, vec![row_a, row_b]);
        assert_eq!(
            loaded.agents[0].extra.get("future_key"),
            Some(&toml::Value::String("preserved".into()))
        );
    }

    #[test]
    fn legacy_config_without_agents_parses_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[service]\napi_key = \"my-key\"\n").unwrap();

        let config = Config::load_from(&path).unwrap();
        assert!(config.agents.is_empty());
    }

    #[test]
    fn agents_serialize_as_array_of_tables_after_defaults() {
        let config = Config {
            agents: vec![agent_entry(
                "main",
                "cfef1150-0000-4000-8000-0000000000cc",
                "/ws",
                true,
            )],
            ..Default::default()
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let agents_pos = toml_str.find("[[agents]]").expect("array of tables");
        let defaults_pos = toml_str.find("[defaults]").expect("defaults table");
        assert!(
            agents_pos > defaults_pos,
            "[[agents]] must serialize after [defaults]:\n{toml_str}"
        );
        // An empty mapping serializes no [[agents]] block at all.
        assert!(!toml::to_string_pretty(&Config::default())
            .unwrap()
            .contains("agents"));
    }

    #[test]
    fn find_agent_by_alias_and_by_id() {
        let id = "cfef1150-0000-4000-8000-0000000000dd";
        let config = Config {
            agents: vec![agent_entry("main", id, "/ws", true)],
            ..Default::default()
        };

        assert_eq!(
            config.find_agent("openclaw", "main").unwrap().alf_agent_id,
            Uuid::parse_str(id).unwrap()
        );
        assert_eq!(
            config.find_agent("openclaw", id).unwrap().runtime_agent,
            "main"
        );
        assert!(config.find_agent("openclaw", "nope").is_none());
        // Runtime scoping: the row is invisible to another runtime.
        assert!(config.find_agent("zeroclaw", "main").is_none());
    }

    #[test]
    fn upsert_agent_replaces_by_id_and_set_enabled_flips() {
        let mut config = Config::default();
        let id = "cfef1150-0000-4000-8000-0000000000ee";
        config.upsert_agent(agent_entry("main", id, "/ws", true));
        assert_eq!(config.agents.len(), 1);

        // Same id, refreshed workspace → replaced, not duplicated.
        config.upsert_agent(agent_entry("main", id, "/ws-moved", true));
        assert_eq!(config.agents.len(), 1);
        assert_eq!(config.agents[0].workspace, "/ws-moved");

        let row = config.set_agent_enabled("openclaw", "main", false).unwrap();
        assert!(!row.enabled);
        // Idempotent.
        let row = config.set_agent_enabled("openclaw", "main", false).unwrap();
        assert!(!row.enabled);
        assert!(config.set_agent_enabled("openclaw", "ghost", true).is_err());
    }
}
