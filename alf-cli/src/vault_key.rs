//! Vault-key resolution: figure out which 32-byte key to use, given
//! CLI flags, env vars, runtime default paths, and passphrase mode.
//!
//! Resolution order (first hit wins):
//!
//! 1. `--vault-key-file PATH`           (explicit file)
//! 2. `--vault-key-env VAR` / `ALF_VAULT_KEY` env (base64 key)
//! 3. `--vault-passphrase-file PATH` or `ALF_VAULT_PASSPHRASE` (Argon2id mode)
//! 4. Default file for the runtime (`~/.openclaw/state/.alf-vault-key`
//!    or `~/.zeroclaw/state/.alf-vault-key`)
//!
//! Modules outside this file must never read the key bytes directly —
//! they get a `VaultKey` (which is `Zeroize`-on-drop) and pass it
//! through to `alf-core::crypto`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use alf_core::{Argon2Params, VaultKey, RECOMMENDED_ARGON2};

/// Flags shared by `alf export`, `alf import`, and every `alf vault *`
/// subcommand that needs a key.
#[derive(Debug, Clone, Default)]
pub struct VaultKeyArgs {
    /// `--vault-key-file PATH`: explicit file holding base64 key.
    pub key_file: Option<PathBuf>,
    /// `--vault-key-env VAR`: name of env var holding base64 key.
    /// Defaults to `ALF_VAULT_KEY` when `--vault-key-env` is omitted.
    pub key_env: Option<String>,
    /// `--vault-passphrase-file PATH`: passphrase from file (Argon2id).
    pub passphrase_file: Option<PathBuf>,
    /// `--vault-passphrase-env VAR`: passphrase from env var.
    pub passphrase_env: Option<String>,
    /// `--vault-salt`: base64 salt for passphrase mode. If omitted, a
    /// fixed per-runtime salt is used so the same passphrase reproduces
    /// the same key across hosts. Use a record-stored salt for new
    /// records via `kdf_params.extra.salt`.
    pub salt_b64: Option<String>,
}

/// Source of the vault key, useful for status messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    File(PathBuf),
    Env(String),
    PassphraseFile(PathBuf),
    PassphraseEnv(String),
    DefaultFile(PathBuf),
}

impl KeySource {
    pub fn label(&self) -> String {
        match self {
            Self::File(p) => format!("file:{}", p.display()),
            Self::Env(v) => format!("env:{v}"),
            Self::PassphraseFile(p) => format!("passphrase-file:{}", p.display()),
            Self::PassphraseEnv(v) => format!("passphrase-env:{v}"),
            Self::DefaultFile(p) => format!("default-file:{}", p.display()),
        }
    }
}

/// Resolve a vault key for a given runtime.
///
/// Returns `Ok(None)` if no key source was supplied AND no default file
/// exists — callers may then fall back to metadata-only behavior. Other
/// failures (file exists but unreadable, bad base64, etc.) return Err.
pub fn resolve(args: &VaultKeyArgs, runtime: &str) -> Result<Option<(VaultKey, KeySource)>> {
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

    // 3. Passphrase mode.
    if let Some(path) = &args.passphrase_file {
        let pass = fs::read_to_string(path)
            .with_context(|| format!("Failed to read passphrase file {}", path.display()))?;
        let key = derive_from_passphrase(pass.trim(), args.salt_b64.as_deref(), runtime)?;
        return Ok(Some((key, KeySource::PassphraseFile(path.clone()))));
    }
    if let Some(env_name) = &args.passphrase_env {
        let pass = std::env::var(env_name)
            .with_context(|| format!("Passphrase env var {env_name} not set"))?;
        let key = derive_from_passphrase(pass.trim(), args.salt_b64.as_deref(), runtime)?;
        return Ok(Some((key, KeySource::PassphraseEnv(env_name.clone()))));
    }
    if let Ok(pass) = std::env::var("ALF_VAULT_PASSPHRASE") {
        if !pass.is_empty() {
            let key = derive_from_passphrase(pass.trim(), args.salt_b64.as_deref(), runtime)?;
            return Ok(Some((
                key,
                KeySource::PassphraseEnv("ALF_VAULT_PASSPHRASE".into()),
            )));
        }
    }

    // 4. Runtime default file.
    if let Some(path) = default_key_path(runtime)? {
        if path.is_file() {
            let key = load_from_file(&path)?;
            return Ok(Some((key, KeySource::DefaultFile(path))));
        }
    }

    Ok(None)
}

/// Resolve a vault key and bail if none is available.
pub fn resolve_required(args: &VaultKeyArgs, runtime: &str) -> Result<(VaultKey, KeySource)> {
    match resolve(args, runtime)? {
        Some(pair) => Ok(pair),
        None => bail!(
            "No vault key resolved. Provide one of: --vault-key-file, --vault-key-env, \
             --vault-passphrase-file, --vault-passphrase-env, ALF_VAULT_KEY env, \
             ALF_VAULT_PASSPHRASE env, or place a base64 key at the runtime default \
             path (~/.{runtime}/state/.alf-vault-key)."
        ),
    }
}

fn load_from_file(path: &Path) -> Result<VaultKey> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("Failed to read vault key file {}", path.display()))?;
    VaultKey::from_base64(&raw).map_err(|e| anyhow!("Invalid key in {}: {e}", path.display()))
}

fn derive_from_passphrase(
    passphrase: &str,
    salt_b64: Option<&str>,
    runtime: &str,
) -> Result<VaultKey> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    let salt: Vec<u8> = match salt_b64 {
        Some(s) => B64
            .decode(s.as_bytes())
            .map_err(|_| anyhow!("--vault-salt is not valid base64"))?,
        None => default_passphrase_salt(runtime).into(),
    };

    let params = Argon2Params {
        memory_cost: RECOMMENDED_ARGON2.memory_cost,
        time_cost: RECOMMENDED_ARGON2.time_cost,
        parallelism: RECOMMENDED_ARGON2.parallelism,
    };
    let (key, _kdf) = VaultKey::from_passphrase(passphrase, &salt, params)
        .map_err(|e| anyhow!("Argon2id derivation failed: {e}"))?;
    Ok(key)
}

/// Stable per-runtime salt used when the user provides a passphrase but
/// no explicit salt. Documented in vault-key-management.md so users
/// know what their passphrase derives to across machines.
fn default_passphrase_salt(runtime: &str) -> &'static [u8] {
    match runtime {
        "openclaw" => b"alf.v1.openclaw.passphrase.salt",
        "zeroclaw" => b"alf.v1.zeroclaw.passphrase.salt",
        _ => b"alf.v1.default.passphrase.salt",
    }
}

/// Path of the default vault key file for a runtime.
///
/// `~/.openclaw/state/.alf-vault-key` or `~/.zeroclaw/state/.alf-vault-key`.
/// Returns `Ok(None)` for unknown runtimes (no default file applies).
pub fn default_key_path(runtime: &str) -> Result<Option<PathBuf>> {
    let home = alf_core::home_dir().context("Could not determine home directory")?;
    let path = match runtime {
        "openclaw" => home.join(".openclaw").join("state").join(".alf-vault-key"),
        "zeroclaw" => home.join(".zeroclaw").join("state").join(".alf-vault-key"),
        _ => return Ok(None),
    };
    Ok(Some(path))
}

/// Path of the agent-managed ALF vault: `~/.alf/vault/credentials.json`.
///
/// Runtime-neutral — the vault is ALF's own store, deliberately separate from
/// any runtime keystore. `alf vault add` writes here, and the adapter merges
/// this file into the archive's Layer 4 on `alf sync`. Unlike the vault key,
/// this file holds only ciphertext, so it is safe to sync.
pub fn default_vault_path() -> Result<PathBuf> {
    let home = alf_core::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".alf").join("vault").join("credentials.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_key_file(key: &VaultKey, dir: &TempDir) -> PathBuf {
        let path = dir.path().join("key");
        fs::write(&path, key.to_base64()).unwrap();
        path
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
        let (resolved, src) = resolve(&args, "openclaw").unwrap().unwrap();
        assert_eq!(resolved.fingerprint(), key.fingerprint());
        assert_eq!(src, KeySource::File(path));
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
        let (resolved, src) = resolve(&args, "openclaw").unwrap().unwrap();
        assert_eq!(resolved.fingerprint(), key.fingerprint());
        assert_eq!(src, KeySource::Env(env_var.to_string()));

        std::env::remove_var(env_var);
    }

    #[test]
    fn passphrase_file_resolves() {
        let dir = TempDir::new().unwrap();
        let pass_path = dir.path().join("passphrase");
        fs::write(&pass_path, "correct horse battery staple").unwrap();

        let args = VaultKeyArgs {
            passphrase_file: Some(pass_path.clone()),
            ..Default::default()
        };
        let (k1, _) = resolve(&args, "openclaw").unwrap().unwrap();
        let (k2, _) = resolve(&args, "openclaw").unwrap().unwrap();
        assert_eq!(
            k1.fingerprint(),
            k2.fingerprint(),
            "passphrase should be stable"
        );

        // Different runtime salt -> different key.
        let (k_z, _) = resolve(&args, "zeroclaw").unwrap().unwrap();
        assert_ne!(k1.fingerprint(), k_z.fingerprint());
    }

    #[test]
    fn returns_none_when_no_source() {
        let args = VaultKeyArgs::default();
        // Use a fake runtime so default_key_path returns None.
        let result = resolve(&args, "fakeruntime").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn default_key_path_per_runtime() {
        let p = default_key_path("openclaw").unwrap().unwrap();
        assert!(p.to_string_lossy().contains(".openclaw"));
        assert!(p.to_string_lossy().ends_with(".alf-vault-key"));

        let z = default_key_path("zeroclaw").unwrap().unwrap();
        assert!(z.to_string_lossy().contains(".zeroclaw"));

        let unknown = default_key_path("unknownruntime").unwrap();
        assert!(unknown.is_none());
    }
}
