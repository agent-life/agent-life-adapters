//! Vault-key resolution: figure out which 32-byte key to use, given
//! CLI flags, env vars, and per-agent runtime default paths.
//!
//! Resolution order (first hit wins):
//!
//! 1. `--vault-key-file PATH`           (explicit file)
//! 2. `--vault-key-env VAR` / `ALF_VAULT_KEY` env (base64 key)
//! 3. Default file for the runtime + agent scope
//!    (`~/.{openclaw|zeroclaw|hermes}/state/{alf_agent_id}/.alf-vault-key`, or the
//!    legacy `~/.{rt}/state/.alf-vault-key` when no agent scope applies)
//!
//! Modules outside this file must never read the key bytes directly —
//! they get a `VaultKey` (which is `Zeroize`-on-drop) and pass it
//! through to `alf-core::crypto`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use uuid::Uuid;

use alf_core::VaultKey;

use crate::errors::{codes, CliError};

/// Flags shared by `alf export`, `alf import`, and every `alf vault *`
/// subcommand that needs a key.
#[derive(Debug, Clone, Default)]
pub struct VaultKeyArgs {
    /// `--vault-key-file PATH`: explicit file holding base64 key.
    pub key_file: Option<PathBuf>,
    /// `--vault-key-env VAR`: name of env var holding base64 key.
    /// Defaults to `ALF_VAULT_KEY` when `--vault-key-env` is omitted.
    pub key_env: Option<String>,
}

/// Source of the vault key, useful for status messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    File(PathBuf),
    Env(String),
    DefaultFile(PathBuf),
}

impl KeySource {
    pub fn label(&self) -> String {
        match self {
            Self::File(p) => format!("file:{}", p.display()),
            Self::Env(v) => format!("env:{v}"),
            Self::DefaultFile(p) => format!("default-file:{}", p.display()),
        }
    }
}

/// Resolve a vault key for the given runtime + agent scope.
///
/// Returns `Ok(None)` if no key source was supplied AND no default file
/// exists — callers may then fall back to metadata-only behavior. Other
/// failures (file exists but unreadable, bad base64, etc.) return Err.
pub fn resolve(
    args: &VaultKeyArgs,
    runtime: &str,
    agent: Option<Uuid>,
) -> Result<Option<(VaultKey, KeySource)>> {
    // 1. Explicit --vault-key-file.
    if let Some(path) = &args.key_file {
        let key = load_from_file(path)?;
        return Ok(Some((key, KeySource::File(path.clone()))));
    }

    // 2. Explicit --vault-key-env (or default ALF_VAULT_KEY if value is
    //    present in env). Only attempt env lookup if the user opted in
    //    (passed --vault-key-env) OR the default env var is set.
    let env_name = args.key_env.as_deref().unwrap_or("ALF_VAULT_KEY");
    if let Ok(value) = std::env::var(env_name) {
        if !value.is_empty() {
            let key = VaultKey::from_base64(&value)
                .map_err(|e| anyhow!("Invalid key in env var {env_name}: {e}"))?;
            return Ok(Some((key, KeySource::Env(env_name.to_string()))));
        }
    }

    // 3. Default key file for (runtime, agent scope).
    if let Some(path) = default_key_path(runtime, agent)? {
        if path.is_file() {
            let key = load_from_file(&path)?;
            return Ok(Some((key, KeySource::DefaultFile(path))));
        }
    }

    Ok(None)
}

/// Resolve a vault key and fail with a coded error if none is available.
pub fn resolve_required(
    args: &VaultKeyArgs,
    runtime: &str,
    agent: Option<Uuid>,
) -> Result<(VaultKey, KeySource)> {
    match resolve(args, runtime, agent)? {
        Some(resolved) => Ok(resolved),
        None => {
            let flags = "--vault-key-file, --vault-key-env, or the ALF_VAULT_KEY env var";
            let remedy = match default_key_path(runtime, agent)? {
                Some(p) => format!(
                    "Run 'alf vault keygen --out {}' to create a default key, or pass \
                     one of: {flags}. Pass --agent <alias-or-id> if the wrong agent \
                     was selected.",
                    p.display()
                ),
                None => format!("Pass one of: {flags}."),
            };
            Err(CliError {
                code: codes::VAULT_KEY_UNRESOLVED,
                cause: format!("No vault key resolved for runtime '{runtime}'."),
                remedy,
            }
            .into())
        }
    }
}

fn load_from_file(path: &Path) -> Result<VaultKey> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("Failed to read vault key file {}", path.display()))?;
    VaultKey::from_base64(&raw).map_err(|e| anyhow!("Invalid key in {}: {e}", path.display()))
}

/// Path of the default vault key file for a runtime + agent scope.
///
/// `(openclaw|zeroclaw|hermes, Some(id))` → `~/.{rt}/state/{id}/.alf-vault-key`;
/// `(.., None)` → the legacy `~/.{rt}/state/.alf-vault-key`. Returns
/// `Ok(None)` for unknown runtimes (no default file applies).
pub fn default_key_path(runtime: &str, agent: Option<Uuid>) -> Result<Option<PathBuf>> {
    let home = alf_core::home_dir().context("Could not determine home directory")?;
    let state_dir = match runtime {
        "openclaw" => home.join(".openclaw").join("state"),
        "zeroclaw" => home.join(".zeroclaw").join("state"),
        // WP5: `~/.hermes/state/<id>/.alf-vault-key`. `state/` is outside every
        // profile's synced unit (profiles hold a `state.db` *file*, never a
        // `state/` dir), so the key never travels in an archive.
        "hermes" => home.join(".hermes").join("state"),
        _ => return Ok(None),
    };
    let path = match agent {
        Some(id) => state_dir.join(id.to_string()).join(".alf-vault-key"),
        None => state_dir.join(".alf-vault-key"),
    };
    Ok(Some(path))
}

/// The pre-WP1 install-scoped key path — migration source only.
pub(crate) fn legacy_default_key_path(runtime: &str) -> Result<Option<PathBuf>> {
    default_key_path(runtime, None)
}

/// Path of the agent-managed ALF vault.
///
/// `Some(id)` → `~/.alf/vault/{id}/credentials.json`; `None` → the legacy
/// install-scoped `~/.alf/vault/credentials.json`. Runtime-neutral — the
/// vault is ALF's own store, deliberately separate from any runtime keystore.
/// `alf vault add` writes here, and the adapters merge this file into the
/// archive's Layer 4 on `alf sync`. Unlike the vault key, this file holds
/// only ciphertext, so it is safe to sync.
pub fn default_vault_path(agent: Option<Uuid>) -> Result<PathBuf> {
    let home = alf_core::home_dir().context("Could not determine home directory")?;
    Ok(match agent {
        Some(id) => alf_core::agent_vault_path(&home, id),
        None => alf_core::legacy_vault_path(&home),
    })
}

/// The pre-WP1 install-scoped vault path — migration source only.
pub(crate) fn legacy_default_vault_path() -> Result<PathBuf> {
    default_vault_path(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::tests::{RestoreEnv, HOME_LOCK};
    use tempfile::TempDir;

    fn temp_key_file(key: &VaultKey, dir: &TempDir) -> PathBuf {
        let path = dir.path().join("key");
        fs::write(&path, key.to_base64()).unwrap();
        path
    }

    fn uuid(n: u8) -> Uuid {
        let mut bytes = [0u8; 16];
        bytes[15] = n;
        Uuid::from_bytes(bytes)
    }

    #[test]
    fn resolves_from_explicit_file() {
        let dir = TempDir::new().unwrap();
        let key = VaultKey::generate();
        let path = temp_key_file(&key, &dir);

        let args = VaultKeyArgs {
            key_file: Some(path.clone()),
            ..Default::default()
        };
        let (resolved, source) = resolve(&args, "openclaw", None).unwrap().unwrap();
        assert_eq!(resolved.fingerprint(), key.fingerprint());
        assert_eq!(source, KeySource::File(path));
    }

    #[test]
    fn resolves_from_env() {
        let key = VaultKey::generate();
        let env_var = "ALF_TEST_VAULT_ENV_KEY";
        std::env::set_var(env_var, key.to_base64());

        let args = VaultKeyArgs {
            key_env: Some(env_var.to_string()),
            ..Default::default()
        };
        let (resolved, source) = resolve(&args, "openclaw", None).unwrap().unwrap();
        assert_eq!(resolved.fingerprint(), key.fingerprint());
        assert_eq!(source, KeySource::Env(env_var.to_string()));

        std::env::remove_var(env_var);
    }

    #[test]
    fn returns_none_when_no_source() {
        let args = VaultKeyArgs::default();
        // Use a fake runtime so default_key_path returns None.
        let result = resolve(&args, "fakeruntime", None).unwrap();
        assert!(result.is_none());
    }

    /// U-1: legacy (agent-less) key paths keep the exact pre-WP1 strings.
    #[test]
    fn default_key_path_legacy_when_no_agent() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());

        let p = default_key_path("openclaw", None).unwrap().unwrap();
        assert_eq!(
            p,
            tmp.path()
                .join(".openclaw")
                .join("state")
                .join(".alf-vault-key")
        );
        let z = default_key_path("zeroclaw", None).unwrap().unwrap();
        assert_eq!(
            z,
            tmp.path()
                .join(".zeroclaw")
                .join("state")
                .join(".alf-vault-key")
        );
        assert!(default_key_path("unknownruntime", None).unwrap().is_none());
        // The legacy aliases resolve identically (migration reads them).
        assert_eq!(legacy_default_key_path("openclaw").unwrap().unwrap(), p);
    }

    /// U-2: per-agent key paths are distinct per agent; hermes resolves under
    /// `~/.hermes/state/` (WP5); unknown runtimes have no default file.
    #[test]
    fn default_key_path_per_agent_distinct() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());

        let a = default_key_path("openclaw", Some(uuid(1)))
            .unwrap()
            .unwrap();
        let b = default_key_path("openclaw", Some(uuid(2)))
            .unwrap()
            .unwrap();
        assert_ne!(a, b, "two agents must get distinct default key paths");
        assert_eq!(
            a,
            tmp.path()
                .join(".openclaw")
                .join("state")
                .join(uuid(1).to_string())
                .join(".alf-vault-key")
        );

        // WP5: hermes resolves under `~/.hermes/state/<id>/.alf-vault-key`,
        // mirroring the openclaw/zeroclaw arms.
        assert_eq!(
            default_key_path("hermes", Some(uuid(1))).unwrap().unwrap(),
            tmp.path()
                .join(".hermes")
                .join("state")
                .join(uuid(1).to_string())
                .join(".alf-vault-key")
        );
        assert_eq!(
            default_key_path("hermes", None).unwrap().unwrap(),
            tmp.path()
                .join(".hermes")
                .join("state")
                .join(".alf-vault-key")
        );
        assert!(default_key_path("unknownruntime", Some(uuid(1)))
            .unwrap()
            .is_none());
    }

    /// U-4: explicit file/env always beat the per-agent default file (DoD).
    #[test]
    fn explicit_file_and_env_win_over_per_agent_default() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());

        // Seed a per-agent default key.
        let default_key = VaultKey::generate();
        let default_path = default_key_path("openclaw", Some(uuid(1)))
            .unwrap()
            .unwrap();
        fs::create_dir_all(default_path.parent().unwrap()).unwrap();
        fs::write(&default_path, default_key.to_base64()).unwrap();

        // Explicit file wins.
        let dir = TempDir::new().unwrap();
        let explicit_key = VaultKey::generate();
        let explicit_path = temp_key_file(&explicit_key, &dir);
        let args = VaultKeyArgs {
            key_file: Some(explicit_path),
            ..Default::default()
        };
        let (resolved, _) = resolve(&args, "openclaw", Some(uuid(1))).unwrap().unwrap();
        assert_eq!(resolved.fingerprint(), explicit_key.fingerprint());

        // Explicit env wins.
        let env_key = VaultKey::generate();
        let env_var = "ALF_TEST_WP1_ENV_KEY";
        std::env::set_var(env_var, env_key.to_base64());
        let args = VaultKeyArgs {
            key_env: Some(env_var.to_string()),
            ..Default::default()
        };
        let (resolved, _) = resolve(&args, "openclaw", Some(uuid(1))).unwrap().unwrap();
        assert_eq!(resolved.fingerprint(), env_key.fingerprint());
        std::env::remove_var(env_var);
    }

    /// U-5: with no flags, the per-agent default file resolves for the scoped
    /// agent, and the legacy file resolves for a scope-less context.
    #[test]
    fn default_file_resolves_per_agent() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());

        let agent_key = VaultKey::generate();
        let agent_path = default_key_path("openclaw", Some(uuid(7)))
            .unwrap()
            .unwrap();
        fs::create_dir_all(agent_path.parent().unwrap()).unwrap();
        fs::write(&agent_path, agent_key.to_base64()).unwrap();

        let legacy_key = VaultKey::generate();
        let legacy_path = default_key_path("openclaw", None).unwrap().unwrap();
        fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        fs::write(&legacy_path, legacy_key.to_base64()).unwrap();

        let args = VaultKeyArgs::default();
        let (resolved, source) = resolve(&args, "openclaw", Some(uuid(7))).unwrap().unwrap();
        assert_eq!(resolved.fingerprint(), agent_key.fingerprint());
        assert_eq!(source, KeySource::DefaultFile(agent_path));

        let (resolved, source) = resolve(&args, "openclaw", None).unwrap().unwrap();
        assert_eq!(resolved.fingerprint(), legacy_key.fingerprint());
        assert_eq!(source, KeySource::DefaultFile(legacy_path));
    }

    /// Pin the full 3-step resolution order: explicit file → env → per-agent
    /// default file. All three sources present ⇒ file wins; drop the file ⇒
    /// env wins; drop the env ⇒ the default file resolves.
    #[test]
    fn resolve_order_is_file_env_default() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());

        let default_key = VaultKey::generate();
        let default_path = default_key_path("openclaw", Some(uuid(9)))
            .unwrap()
            .unwrap();
        fs::create_dir_all(default_path.parent().unwrap()).unwrap();
        fs::write(&default_path, default_key.to_base64()).unwrap();

        let env_key = VaultKey::generate();
        let env_var = "ALF_TEST_ORDER_ENV_KEY";
        std::env::set_var(env_var, env_key.to_base64());

        let dir = TempDir::new().unwrap();
        let file_key = VaultKey::generate();
        let file_path = temp_key_file(&file_key, &dir);

        // 1. File beats env and default.
        let args = VaultKeyArgs {
            key_file: Some(file_path),
            key_env: Some(env_var.to_string()),
        };
        let (resolved, source) = resolve(&args, "openclaw", Some(uuid(9))).unwrap().unwrap();
        assert_eq!(resolved.fingerprint(), file_key.fingerprint());
        assert!(matches!(source, KeySource::File(_)));

        // 2. Env beats default.
        let args = VaultKeyArgs {
            key_file: None,
            key_env: Some(env_var.to_string()),
        };
        let (resolved, source) = resolve(&args, "openclaw", Some(uuid(9))).unwrap().unwrap();
        assert_eq!(resolved.fingerprint(), env_key.fingerprint());
        assert!(matches!(source, KeySource::Env(_)));

        // 3. Default file resolves when nothing explicit is passed.
        std::env::remove_var(env_var);
        std::env::remove_var("ALF_VAULT_KEY");
        let args = VaultKeyArgs::default();
        let (resolved, source) = resolve(&args, "openclaw", Some(uuid(9))).unwrap().unwrap();
        assert_eq!(resolved.fingerprint(), default_key.fingerprint());
        assert!(matches!(source, KeySource::DefaultFile(_)));
    }

    /// U-6: the unresolved-key error is coded and its remedy names the exact
    /// per-agent keygen command.
    #[test]
    fn resolve_required_remedy_names_per_agent_keygen_command() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());
        std::env::remove_var("ALF_VAULT_KEY");

        // VaultKey has no Debug (deliberately unprintable), so unwrap_err()
        // is unavailable — match instead.
        let expect_err = |r: Result<(VaultKey, KeySource)>| match r {
            Ok(_) => panic!("resolution must fail with no key source"),
            Err(e) => e,
        };

        let args = VaultKeyArgs::default();
        let err = expect_err(resolve_required(&args, "openclaw", Some(uuid(3))));
        let cli_err = err.downcast_ref::<CliError>().expect("coded error");
        assert_eq!(cli_err.code, codes::VAULT_KEY_UNRESOLVED);
        assert!(cli_err.remedy.contains("alf vault keygen --out"));
        assert!(
            cli_err
                .remedy
                .contains(&format!("{}/.alf-vault-key", uuid(3))),
            "remedy must name the per-agent key path: {}",
            cli_err.remedy
        );
        assert!(cli_err.remedy.contains("--agent"));

        // Hermes (WP5): has a per-agent default key path, so the remedy names
        // `keygen` + the per-agent path, exactly like openclaw.
        let err = expect_err(resolve_required(&args, "hermes", Some(uuid(3))));
        let cli_err = err.downcast_ref::<CliError>().expect("coded error");
        assert_eq!(cli_err.code, codes::VAULT_KEY_UNRESOLVED);
        assert!(cli_err.remedy.contains("alf vault keygen --out"));
        assert!(
            cli_err
                .remedy
                .contains(&format!("{}/.alf-vault-key", uuid(3))),
            "remedy must name the per-agent key path: {}",
            cli_err.remedy
        );
        assert!(cli_err.remedy.contains("--agent"));
    }
}
