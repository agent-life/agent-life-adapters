//! Configuration management for `~/.alf/config.toml`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Top-level config file structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub service: ServiceConfig,

    #[serde(default)]
    pub defaults: DefaultsConfig,
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

impl Default for Config {
    fn default() -> Self {
        Self {
            service: ServiceConfig::default(),
            defaults: DefaultsConfig::default(),
        }
    }
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
    /// Returns the path to the config directory (`~/.alf/`).
    pub fn dir() -> Result<PathBuf> {
        let home = home_dir().context("Could not determine home directory")?;
        Ok(home.join(".alf"))
    }

    /// Returns the path to the config file (`~/.alf/config.toml`).
    pub fn path() -> Result<PathBuf> {
        Ok(Self::dir()?.join("config.toml"))
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
    #[allow(dead_code)]
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        self.save_to(&path)
    }

    /// Save the config to a specific path, creating parent directories
    /// if needed.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }
        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;
        fs::write(path, content)
            .with_context(|| format!("Failed to write config to {}", path.display()))?;
        Ok(())
    }
}

/// Cross-platform home directory lookup.
fn home_dir() -> Option<PathBuf> {
    // std::env::home_dir is deprecated but dirs crate is heavy.
    // Use HOME on Unix, USERPROFILE on Windows.
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

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
        };

        config.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(config, loaded);
    }

    #[test]
    fn load_missing_file_returns_defaults() {
        let _lock = ENV_MUTEX.lock().unwrap();
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
        let _lock = ENV_MUTEX.lock().unwrap();

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        fs::write(&path, "[service]\napi_url = \"https://api.agent-life.ai\"\n").unwrap();

        std::env::set_var("ALF_API_KEY", "alf_sk_from_env");
        let config = Config::load_from(&path).unwrap();
        std::env::remove_var("ALF_API_KEY");

        assert_eq!(config.service.api_key, "alf_sk_from_env");
    }

    #[test]
    fn file_key_takes_precedence_over_env_var() {
        let _lock = ENV_MUTEX.lock().unwrap();

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        fs::write(&path, "[service]\napi_key = \"from_file\"\n").unwrap();

        std::env::set_var("ALF_API_KEY", "from_env");
        let config = Config::load_from(&path).unwrap();
        std::env::remove_var("ALF_API_KEY");

        assert_eq!(config.service.api_key, "from_file");
    }
}