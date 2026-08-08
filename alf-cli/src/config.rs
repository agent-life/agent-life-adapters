//! Configuration management for `~/.alf/config.toml`.

use crate::fs_private::write_private_atomic;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

/// Bounded wait for the cross-process config lock, matching the per-agent lock
/// timeout so a contended writer fails uniformly rather than blocking forever.
const CONFIG_LOCK_TIMEOUT: Duration = Duration::from_secs(10);
const CONFIG_LOCK_POLL: Duration = Duration::from_millis(250);

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

    /// Path to the single cross-process config lock (`~/.alf/config.lock`) that
    /// serializes every `config.toml` read-modify-write (RF-013).
    pub fn lock_path() -> Result<PathBuf> {
        Ok(Self::dir()?.join("config.lock"))
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
    /// Does not create the file if missing — call
    /// [`update_locked`](Config::update_locked) (or [`save_to`](Config::save_to))
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

        // Symmetric fallback for the service URL: a file that doesn't set
        // `[service] api_url` (it deserializes to the default) can be pointed
        // elsewhere via ALF_API_URL — used by containers/runtimes so no config
        // pre-write is required. A URL explicitly set in the file wins.
        if config.service.api_url == default_api_url() {
            if let Ok(url) = std::env::var("ALF_API_URL") {
                if !url.is_empty() {
                    config.service.api_url = url;
                }
            }
        }

        Ok(config)
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
        // Atomic temp+rename (WP-M5 review A1): the long-running MCP server is now
        // a `config.toml` writer (`run_rediscovery`), and the tools read it on
        // parallel threads with no shared lock. A non-atomic truncate+write would
        // let a reader land on an empty file — and since every field is
        // `#[serde(default)]`, that parses as an empty `[[agents]]` mapping, so
        // `alf_agents_list` would momentarily report zero agents. Rename makes
        // every reader see either the whole old or the whole new config.
        write_private_atomic(path, &content)
            .with_context(|| format!("Failed to write config to {}", path.display()))?;
        Ok(())
    }

    /// Serialized cross-process read-modify-write of `config.toml` (RF-013).
    ///
    /// [`save_to`](Config::save_to) makes an individual write crash-safe, but it
    /// does not make a load→mutate→save span atomic: two `alf` processes (e.g. a
    /// CLI command and a running `mcp serve` watch loop) can each read version A,
    /// make disjoint edits, and save B then C — silently dropping B. This is the
    /// only correct way to mutate the shared document when more than one process
    /// can run.
    ///
    /// It acquires the config flock (`~/.alf/config.lock`), **reloads** the
    /// latest config from disk into `self` — so `f` rebases onto whatever a
    /// concurrent writer committed, rather than clobbering it — runs `f`,
    /// atomically saves, and releases. Any edits made to `self` *before* calling
    /// this are discarded by the reload; express every mutation inside `f`.
    ///
    /// LOCK ORDER: the config lock is acquired ABOVE the per-agent lock. Never
    /// call this while holding a per-agent lock (see
    /// `commands::mcp::watch::lock` hierarchy).
    pub fn update_locked<T>(&mut self, f: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
        let path = Self::path()?;
        let lock_path = Self::lock_path()?;
        Self::update_locked_at(self, &path, &lock_path, f)
    }

    /// [`update_locked`](Config::update_locked) against explicit paths — for
    /// tests that point `HOME`/`ALF_HOME` at a temp directory or drive two
    /// processes against the same config.
    pub fn update_locked_at<T>(
        config: &mut Config,
        path: &Path,
        lock_path: &Path,
        f: impl FnOnce(&mut Config) -> Result<T>,
    ) -> Result<T> {
        Self::update_locked_with(
            config,
            path,
            lock_path,
            CONFIG_LOCK_TIMEOUT,
            CONFIG_LOCK_POLL,
            f,
        )
    }

    /// The lock-acquire → reload → mutate → save core, with an explicit wait
    /// budget so tests can force the contended `config_busy` path without the
    /// 10 s production timeout.
    fn update_locked_with<T>(
        config: &mut Config,
        path: &Path,
        lock_path: &Path,
        timeout: Duration,
        poll: Duration,
        f: impl FnOnce(&mut Config) -> Result<T>,
    ) -> Result<T> {
        // The lock file (and thus the config dir) must exist before we can flock
        // it. Creating the lock file is not itself the critical section.
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }
        let _guard = crate::commands::mcp::watch::lock::acquire_timeout(lock_path, timeout, poll)
            .with_context(|| format!("config lock unusable at {}", lock_path.display()))?
            .ok_or_else(config_busy)?;

        *config = Self::load_from(path)?;
        let out = f(config)?;
        config.save_to(path)?;
        Ok(out)
    }
}

/// The `config_busy` error for a config-lock acquisition that timed out — the
/// `config.toml` RMW was not attempted, so the file is unchanged.
fn config_busy() -> anyhow::Error {
    crate::errors::CliError {
        code: crate::errors::codes::CONFIG_BUSY,
        cause: "another alf process is updating the configuration".to_string(),
        remedy: "retry shortly".to_string(),
    }
    .into()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::mcp::watch::lock;
    use crate::context::tests::HOME_LOCK;
    use crate::errors::{codes, CliError};
    use std::sync::{Arc, Barrier};
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
        std::env::remove_var("ALF_API_URL");

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.toml");

        let config = Config::load_from(&path).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn load_partial_config_fills_defaults() {
        let _lock = HOME_LOCK.lock().unwrap();
        std::env::remove_var("ALF_API_URL");

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
    fn api_url_env_fallback_and_file_precedence() {
        let _lock = HOME_LOCK.lock().unwrap();

        let dir = TempDir::new().unwrap();

        // File without api_url → ALF_API_URL fills it in.
        let path = dir.path().join("config.toml");
        fs::write(&path, "[service]\napi_key = \"my-key\"\n").unwrap();
        std::env::set_var("ALF_API_URL", "http://localhost:9099");
        let config = Config::load_from(&path).unwrap();
        assert_eq!(config.service.api_url, "http://localhost:9099");

        // File with an explicit non-default api_url wins over the env var.
        let path = dir.path().join("config-explicit.toml");
        fs::write(&path, "[service]\napi_url = \"https://custom.example\"\n").unwrap();
        let config = Config::load_from(&path).unwrap();
        std::env::remove_var("ALF_API_URL");
        assert_eq!(config.service.api_url, "https://custom.example");
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
    fn save_to_is_atomic_and_leaves_no_temp() {
        // WP-M5 review A1: an overwriting save must not leave a torn/partial file
        // or a `.tmp` sibling — a concurrent reader (an MCP tool's `Config::load`)
        // always sees a whole config.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        let mut first = Config::default();
        first.agents.push(AgentEntry {
            runtime: Some("hermes".into()),
            runtime_agent: "scout".into(),
            runtime_agent_id: None,
            alf_agent_id: uuid::Uuid::nil(),
            workspace: "/ws".into(),
            enabled: true,
            extra: Default::default(),
        });
        first.save_to(&path).unwrap();
        // Overwrite; the file must remain fully parseable throughout.
        Config::default().save_to(&path).unwrap();

        let reloaded = Config::load_from(&path).unwrap();
        assert!(reloaded.agents.is_empty(), "second whole config won");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no torn .tmp sibling survives a save");
    }

    #[test]
    fn concurrent_saves_never_expose_an_empty_agent_mapping() {
        // WP-M5 review A1 DoD guard: with the MCP server (or a CLI) repeatedly
        // saving config.toml, a concurrent reader (`alf_agents_list` →
        // `Config::load`) must always see a whole file — never the truncate window
        // that would parse as an empty `[[agents]]` mapping. Atomic temp+rename
        // guarantees it; the pre-A1 in-place truncate+write did not.
        use std::sync::Arc;
        let dir = Arc::new(TempDir::new().unwrap());
        let path = dir.path().join("config.toml");

        let mut seeded = Config::default();
        seeded.agents.push(AgentEntry {
            runtime: Some("hermes".into()),
            runtime_agent: "scout".into(),
            runtime_agent_id: None,
            alf_agent_id: uuid::Uuid::nil(),
            workspace: "/ws".into(),
            enabled: true,
            extra: Default::default(),
        });
        seeded.save_to(&path).unwrap();

        let mut handles = Vec::new();
        for _ in 0..3 {
            let (p, cfg) = (path.clone(), seeded.clone());
            handles.push(std::thread::spawn(move || {
                for _ in 0..40 {
                    cfg.save_to(&p).unwrap();
                }
            }));
        }
        for _ in 0..3 {
            let p = path.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..80 {
                    let c = Config::load_from(&p).expect("config always parseable");
                    assert_eq!(c.agents.len(), 1, "reader never sees a torn/empty config");
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
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

    // -----------------------------------------------------------------------
    // RF-013 — cross-process config read-modify-write serialization.
    //
    // These use `update_locked_at`/`update_locked_with` against explicit paths
    // so they need no `HOME`/`ALF_HOME` and stay deterministic. `flock` is
    // per-open-file-description, so two separate opens of the same lock path in
    // the same process contend exactly as two processes would — the same
    // property the `lock.rs` unit tests rely on.
    // -----------------------------------------------------------------------

    fn uuid_n(n: u8) -> String {
        format!("00000000-0000-0000-0000-0000000000{n:02}")
    }

    /// Two writers that both start from the same (empty) config each add a
    /// DIFFERENT agent. Under the lock, the second writer reloads and sees the
    /// first's row, so both survive. This is the fixed behavior.
    #[test]
    fn concurrent_config_update_preserves_disjoint_agent_adds() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let lock_path = dir.path().join("config.lock");
        Config::default().save_to(&path).unwrap();

        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (1u8..=2)
            .map(|i| {
                let (path, lock_path, barrier) = (path.clone(), lock_path.clone(), barrier.clone());
                std::thread::spawn(move || {
                    // Both read version A before either writes — the classic
                    // lost-update window.
                    let mut cfg = Config::load_from(&path).unwrap();
                    barrier.wait();
                    Config::update_locked_at(&mut cfg, &path, &lock_path, |c| {
                        c.upsert_agent(agent_entry(
                            &format!("agent-{i}"),
                            &uuid_n(i),
                            &format!("/ws-{i}"),
                            true,
                        ));
                        Ok(())
                    })
                    .unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let final_cfg = Config::load_from(&path).unwrap();
        assert_eq!(
            final_cfg.agents.len(),
            2,
            "both disjoint agent adds must survive the serialized RMW"
        );
    }

    /// The bug the lock fixes: the pre-RF-013 pattern (load → mutate in memory →
    /// `save_to`, no lock, no reload) drops one update when both writers start
    /// from the same version. Proves the harness actually reproduces the race
    /// the fixed test above defeats.
    #[test]
    fn concurrent_config_update_naive_save_loses_one() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        Config::default().save_to(&path).unwrap();

        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (1u8..=2)
            .map(|i| {
                let (path, barrier) = (path.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let mut cfg = Config::load_from(&path).unwrap();
                    barrier.wait();
                    cfg.upsert_agent(agent_entry(
                        &format!("agent-{i}"),
                        &uuid_n(i),
                        &format!("/ws-{i}"),
                        true,
                    ));
                    cfg.save_to(&path).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let final_cfg = Config::load_from(&path).unwrap();
        assert_eq!(
            final_cfg.agents.len(),
            1,
            "the unserialized load-mutate-save loses one of the two adds"
        );
    }

    /// Disjoint fields (an api-key write racing an agent add) must both survive
    /// — the corrected form of RF-013's "change an interval and add an agent"
    /// case (watch cadence is in-memory only, so a persisted field stands in).
    #[test]
    fn concurrent_config_update_disjoint_fields_survive() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let lock_path = dir.path().join("config.lock");
        Config::default().save_to(&path).unwrap();

        let barrier = Arc::new(Barrier::new(2));
        let key_thread = {
            let (path, lock_path, barrier) = (path.clone(), lock_path.clone(), barrier.clone());
            std::thread::spawn(move || {
                let mut cfg = Config::load_from(&path).unwrap();
                barrier.wait();
                Config::update_locked_at(&mut cfg, &path, &lock_path, |c| {
                    c.service.api_key = "sk-live-key".into();
                    Ok(())
                })
                .unwrap();
            })
        };
        let agent_thread = {
            let (path, lock_path, barrier) = (path.clone(), lock_path.clone(), barrier.clone());
            std::thread::spawn(move || {
                let mut cfg = Config::load_from(&path).unwrap();
                barrier.wait();
                Config::update_locked_at(&mut cfg, &path, &lock_path, |c| {
                    c.upsert_agent(agent_entry("agent-1", &uuid_n(1), "/ws-1", true));
                    Ok(())
                })
                .unwrap();
            })
        };
        key_thread.join().unwrap();
        agent_thread.join().unwrap();

        let final_cfg = Config::load_from(&path).unwrap();
        assert_eq!(
            final_cfg.service.api_key, "sk-live-key",
            "api-key write kept"
        );
        assert_eq!(final_cfg.agents.len(), 1, "agent add kept");
    }

    /// A held config lock makes an update time out with `config_busy` and leaves
    /// `config.toml` byte-for-byte unchanged (the RMW is never attempted). Uses
    /// a short wait budget so the test does not sit on the 10 s production
    /// timeout.
    #[test]
    fn config_lock_held_times_out_leaving_bytes_unchanged() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let lock_path = dir.path().join("config.lock");

        let mut seeded = Config::default();
        seeded.service.api_key = "seed-key".into();
        seeded.save_to(&path).unwrap();
        let before = fs::read(&path).unwrap();

        // Hold the config lock exclusively from a separate fd.
        let held = lock::acquire_blocking(&lock_path).unwrap();

        let mut cfg = Config::load_from(&path).unwrap();
        let err = Config::update_locked_with(
            &mut cfg,
            &path,
            &lock_path,
            Duration::from_millis(200),
            Duration::from_millis(25),
            |c| {
                c.service.api_key = "should-not-persist".into();
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(
            err.downcast_ref::<CliError>().map(|e| e.code),
            Some(codes::CONFIG_BUSY),
            "a contended config lock surfaces config_busy"
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            before,
            "a timed-out RMW leaves config.toml byte-identical"
        );
        drop(held);
    }

    /// Lock-order guard (RF-013 §3): config lock is always taken before the
    /// per-agent lock. Two threads take them in that order and both complete —
    /// a future inversion would deadlock and hang this test.
    #[test]
    fn config_lock_ordering_config_then_agent_no_deadlock() {
        let dir = TempDir::new().unwrap();
        let config_lock = dir.path().join("config.lock");
        let agent_lock = dir.path().join("agent.lock");

        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let (config_lock, agent_lock, barrier) =
                    (config_lock.clone(), agent_lock.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    let cfg_guard = lock::acquire_timeout(
                        &config_lock,
                        Duration::from_secs(5),
                        Duration::from_millis(10),
                    )
                    .unwrap()
                    .expect("config lock acquired within budget");
                    let agent_guard = lock::acquire_timeout(
                        &agent_lock,
                        Duration::from_secs(5),
                        Duration::from_millis(10),
                    )
                    .unwrap()
                    .expect("agent lock acquired within budget");
                    // Trivial critical section; drop in reverse order.
                    drop(agent_guard);
                    drop(cfg_guard);
                })
            })
            .collect();
        for h in handles {
            h.join()
                .expect("no deadlock — consistent config→agent order");
        }
    }
}
