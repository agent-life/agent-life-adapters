//! `alf login` — authenticate with the agent-life service.

use crate::config::Config;
use anyhow::{bail, Result};
use colored::Colorize;

pub fn run(key: Option<&str>) -> Result<()> {
    match key {
        Some(api_key) => login_with_key(api_key),
        None => login_interactive(),
    }
}

/// Store a provided API key directly.
fn login_with_key(api_key: &str) -> Result<()> {
    // Basic format validation
    let trimmed = api_key.trim();
    if trimmed.is_empty() {
        bail!("API key must not be empty");
    }

    // Load existing config, update key, save
    let config_path = Config::path()?;
    let mut config = Config::load_from(&config_path)?;
    config.service.api_key = trimmed.to_string();
    config.save_to(&config_path)?;

    let masked = mask_key(trimmed);
    println!("{} API key saved to {}", "✓".green().bold(), config_path.display());
    println!("  Key: {masked}");
    println!();
    println!("  You can now use `alf sync` and `alf restore`.");

    Ok(())
}

/// Interactive login via device flow (Phase 2 stub).
fn login_interactive() -> Result<()> {
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

        // Create initial config
        let config = Config::default();
        config.save_to(&path).unwrap();

        // Simulate login: load, update key, save
        let mut loaded = Config::load_from(&path).unwrap();
        loaded.service.api_key = "alf_test_key_12345".into();
        loaded.save_to(&path).unwrap();

        // Verify
        let reloaded = Config::load_from(&path).unwrap();
        assert_eq!(reloaded.service.api_key, "alf_test_key_12345");
        // Other fields preserved
        assert_eq!(reloaded.service.api_url, "https://api.agent-life.ai");
        assert_eq!(reloaded.defaults.runtime, "openclaw");
    }
}