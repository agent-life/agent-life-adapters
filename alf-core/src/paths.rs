//! Filesystem path resolution shared across the CLI and adapters.
//!
//! The home directory is the base for both alf's own data (`~/.alf`) and the
//! runtime directories it reads (`~/.openclaw`, `~/.zeroclaw`). Setting
//! `ALF_HOME` overrides it, giving alf a stable anchor when an agent process
//! rewrites the OS `$HOME` — which would otherwise move `~/.alf` out from under
//! the CLI.

use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Resolve the home base directory.
///
/// Precedence: `ALF_HOME` (if set and non-empty) → `$HOME` (Unix) /
/// `%USERPROFILE%` (Windows). Callers append `.alf`, `.openclaw`, etc. as
/// before, so `ALF_HOME=/data` puts the config at `/data/.alf/config.toml`.
pub fn home_dir() -> Option<PathBuf> {
    if let Some(alf_home) = std::env::var_os("ALF_HOME") {
        if !alf_home.is_empty() {
            return Some(PathBuf::from(alf_home));
        }
    }
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

/// Canonical per-agent ALF vault file:
/// `<home>/.alf/vault/<alf_agent_id>/credentials.json`.
///
/// The directory name is the hyphenated lowercase UUID (the same S3-visible
/// layout convention the sync service uses). Shared by the CLI's vault
/// commands and every adapter's Layer-4 read/write so the composition can
/// never drift between them.
pub fn agent_vault_path(home: &Path, alf_agent_id: Uuid) -> PathBuf {
    home.join(".alf")
        .join("vault")
        .join(alf_agent_id.to_string())
        .join("credentials.json")
}

/// Legacy install-scoped vault file: `<home>/.alf/vault/credentials.json`.
///
/// Pre-WP1 installs kept a single vault here; the CLI migrates it to
/// [`agent_vault_path`] once an `[[agents]]` mapping exists. Adapters never
/// read this path — migration runs before any export/import.
pub fn legacy_vault_path(home: &Path) -> PathBuf {
    home.join(".alf").join("vault").join("credentials.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize env mutation within this module.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn alf_home_takes_precedence() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev_home = std::env::var_os("HOME");
        let prev_alf = std::env::var_os("ALF_HOME");

        std::env::set_var("HOME", "/real/home");
        std::env::set_var("ALF_HOME", "/override/home");
        assert_eq!(home_dir(), Some(PathBuf::from("/override/home")));

        restore("HOME", prev_home);
        restore("ALF_HOME", prev_alf);
    }

    #[test]
    fn empty_alf_home_falls_back_to_home() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev_home = std::env::var_os("HOME");
        let prev_alf = std::env::var_os("ALF_HOME");

        std::env::set_var("ALF_HOME", "");
        std::env::set_var("HOME", "/real/home");
        // On unix HOME is used; the empty ALF_HOME must be ignored.
        #[cfg(unix)]
        assert_eq!(home_dir(), Some(PathBuf::from("/real/home")));

        restore("HOME", prev_home);
        restore("ALF_HOME", prev_alf);
    }

    fn restore(key: &str, prev: Option<std::ffi::OsString>) {
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    /// Path-shape pin: the per-agent vault dir is the hyphenated lowercase
    /// UUID — an S3-visible layout convention, not just an implementation
    /// detail.
    #[test]
    fn agent_vault_path_shape_pinned() {
        let id = Uuid::parse_str("CFEF1150-0000-4000-8000-0000000000AA").unwrap();
        let p = agent_vault_path(Path::new("/home/u"), id);
        assert_eq!(
            p,
            PathBuf::from(
                "/home/u/.alf/vault/cfef1150-0000-4000-8000-0000000000aa/credentials.json"
            )
        );
    }

    #[test]
    fn legacy_vault_path_shape_pinned() {
        let p = legacy_vault_path(Path::new("/home/u"));
        assert_eq!(p, PathBuf::from("/home/u/.alf/vault/credentials.json"));
    }
}
