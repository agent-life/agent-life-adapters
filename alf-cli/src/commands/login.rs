//! `alf login` — authenticate with the agent-life service.

use crate::config::Config;
use crate::output;
use anyhow::{bail, Result};
use colored::Colorize;
use serde::Serialize;

#[derive(Serialize)]
struct LoginResult {
    ok: bool,
    key_masked: String,
    config_path: String,
}

pub fn run(key: Option<&str>) -> Result<()> {
    match key {
        Some(api_key) => login_with_key(api_key),
        None => login_interactive(),
    }
}

/// Store a provided API key directly.
fn login_with_key(api_key: &str) -> Result<()> {
    let human = output::human_mode();

    let trimmed = api_key.trim();
    if trimmed.is_empty() {
        bail!("API key must not be empty");
    }

    let config_path = Config::path()?;
    let mut config = Config::load_from(&config_path)?;
    config.service.api_key = trimmed.to_string();
    config.save_to(&config_path)?;

    let masked = mask_key(trimmed);

    if human {
        println!("{} API key saved to {}", "✓".green().bold(), config_path.display());
        println!("  Key: {masked}");
        println!();
        println!("  You can now use `alf sync` and `alf restore`.");
    } else {
        output::progress(&format!("API key saved to {}", config_path.display()));
        output::json(&LoginResult {
            ok: true,
            key_masked: masked,
            config_path: config_path.to_string_lossy().into(),
        });
    }

    Ok(())
}

/// Interactive login via device flow (Phase 2 stub).
fn login_interactive() -> Result<()> {
    let human = output::human_mode();

    if human {
        println!(
            "{} Interactive login (device flow)",
            "▸".blue().bold()
        );
        println!();
        println!("  You can authenticate with an API key:");
        println!();
        println!("    alf login --key <your-api-key>");
        println!();
        println!("  To get an API key, visit: https://agent-life.ai/settings/api-keys");
    } else {
        output::json(&serde_json::json!({
            "ok": false,
            "error": "Interactive login not yet implemented. Use: alf login --key <your-api-key>",
            "hint": "Get an API key at https://agent-life.ai/settings/api-keys"
        }));
    }

    Ok(())
}

/// Mask an API key for display: show first 8 and last 4 characters.
fn mask_key(key: &str) -> String {
    if key.len() <= 12 {
        return "*".repeat(key.len());
    }
    let prefix = &key[..8];
    let suffix = &key[key.len() - 4..];
    let masked_len = key.len() - 12;
    format!("{prefix}{}...{suffix}", "*".repeat(masked_len.min(8)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use tempfile::TempDir;

    #[test]
    fn mask_key_long() {
        let masked = mask_key("alf_sk_1234567890abcdef");
        assert!(masked.starts_with("alf_sk_1"));
        assert!(masked.ends_with("...cdef"));
        assert!(masked.contains('*'));
    }

    #[test]
    fn mask_key_short() {
        let masked = mask_key("short");
        assert_eq!(masked, "*****");
    }

    #[test]
    fn save_key_to_config() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        let config = Config::default();
        config.save_to(&path).unwrap();

        let mut loaded = Config::load_from(&path).unwrap();
        loaded.service.api_key = "alf_test_key_12345".into();
        loaded.save_to(&path).unwrap();

        let reloaded = Config::load_from(&path).unwrap();
        assert_eq!(reloaded.service.api_key, "alf_test_key_12345");
        assert_eq!(reloaded.service.api_url, "https://api.agent-life.ai");
        assert_eq!(reloaded.defaults.runtime, "openclaw");
    }
}
