//! Filesystem path resolution shared across the CLI and adapters.
//!
//! The home directory is the base for both alf's own data (`~/.alf`) and the
//! runtime directories it reads (`~/.openclaw`, `~/.zeroclaw`). Setting
//! `ALF_HOME` overrides it, giving alf a stable anchor when an agent process
//! rewrites the OS `$HOME` — which would otherwise move `~/.alf` out from under
//! the CLI.

use std::path::PathBuf;

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
}
