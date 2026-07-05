//! Legacy → per-agent vault migration (WP1).
//!
//! Pre-WP1 installs keep one vault (`~/.alf/vault/credentials.json`) and one
//! per-runtime key (`~/.{rt}/state/.alf-vault-key`). WP1 scopes both by
//! `alf_agent_id`; this module moves the legacy files once, per file,
//! rename-first and key-less (D4): ciphertext bytes move verbatim, the vault
//! is never decrypted or rewritten, and the absence of the legacy file IS the
//! done-state — idempotence is structural, no marker file. A move either
//! happened or it didn't; there is no post-move rewrite.
//!
//! The migration target is the sole *enabled* mapping row for the runtime, or
//! an explicit human choice via `alf vault migrate --agent` (D5) — never the
//! incidental `--agent` of the triggering command. Anything ambiguous blocks
//! with `vault_migration_blocked` and an exact remedy.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use uuid::Uuid;

use alf_core::agent_vault_path;

use crate::config::Config;
use crate::errors::{codes, CliError};
use crate::output;
use crate::vault_key;

/// Result of [`ensure_migrated`].
pub enum MigrationOutcome {
    /// No legacy files exist — nothing to do (the idempotent steady state).
    NotNeeded,
    /// Legacy file(s) moved to their per-agent locations.
    Migrated {
        vault: Option<PathBuf>,
        key: Option<PathBuf>,
        agent: Uuid,
    },
    /// A legacy file exists but the target is ambiguous or diverged. Coded
    /// `vault_migration_blocked` with the exact remedy. Every trigger except
    /// `alf check` turns this into a hard error.
    Blocked(CliError),
}

/// The decision [`plan_migration`] reaches, separated from execution so
/// `alf vault migrate --dry-run` can report without writing.
pub(crate) enum MigrationPlan {
    NotNeeded,
    Blocked(CliError),
    Move {
        agent: Uuid,
        /// `(from, to)` for the vault leg, when a legacy vault exists.
        vault: Option<(PathBuf, PathBuf)>,
        /// `(from, to)` for the key leg, when a legacy key exists and the
        /// runtime has a per-agent key path. All three runtimes now have one;
        /// the leg is `None` only when no legacy key file is present to move.
        key: Option<(PathBuf, PathBuf)>,
    },
}

/// Idempotent legacy-vault migration. Call after selector resolution, before
/// any per-agent vault/key path use. `explicit_target` is set only by
/// `alf vault migrate --agent` (bypasses the ambiguity blocks, never the
/// diverged-pair block).
pub fn ensure_migrated(
    config: &Config,
    runtime: &str,
    explicit_target: Option<Uuid>,
) -> Result<MigrationOutcome> {
    match plan_migration(config, runtime, explicit_target)? {
        MigrationPlan::NotNeeded => Ok(MigrationOutcome::NotNeeded),
        MigrationPlan::Blocked(err) => Ok(MigrationOutcome::Blocked(err)),
        MigrationPlan::Move { agent, vault, key } => {
            let migrated_vault = match &vault {
                Some((from, to)) => {
                    move_vault(from, to)?;
                    Some(to.clone())
                }
                None => None,
            };
            let migrated_key = match &key {
                Some((from, to)) => {
                    move_key(from, to)?;
                    Some(to.clone())
                }
                None => None,
            };
            Ok(MigrationOutcome::Migrated {
                vault: migrated_vault,
                key: migrated_key,
                agent,
            })
        }
    }
}

/// Hard-error wrapper used by every trigger except `alf check` and
/// `alf vault migrate`: `Blocked` becomes the coded error, `Migrated` prints
/// progress lines.
pub fn require_migrated(config: &Config, runtime: &str) -> Result<()> {
    match ensure_migrated(config, runtime, None)? {
        MigrationOutcome::NotNeeded => Ok(()),
        MigrationOutcome::Blocked(err) => Err(err.into()),
        MigrationOutcome::Migrated { vault, key, agent } => {
            if let Some(p) = &vault {
                output::progress(&format!(
                    "  Migrated legacy vault to {} (agent {agent})",
                    p.display()
                ));
            }
            if let Some(p) = &key {
                output::progress(&format!(
                    "  Migrated legacy vault key to {} (agent {agent})",
                    p.display()
                ));
            }
            Ok(())
        }
    }
}

/// Decide what a migration run would do, without writing anything.
pub(crate) fn plan_migration(
    config: &Config,
    runtime: &str,
    explicit_target: Option<Uuid>,
) -> Result<MigrationPlan> {
    let legacy_vault = vault_key::legacy_default_vault_path()?;
    let legacy_vault_present = legacy_vault.is_file();
    let legacy_key = vault_key::legacy_default_key_path(runtime)?.filter(|p| p.is_file());

    // Both absent → structural idempotence: zero reads, zero writes.
    if !legacy_vault_present && legacy_key.is_none() {
        return Ok(MigrationPlan::NotNeeded);
    }

    // Target: explicit human choice (bypasses V5/V6), else the decision table.
    let agent = match explicit_target {
        Some(id) => id,
        None => match decide_target(config, runtime, legacy_vault_present)? {
            Ok(id) => id,
            Err(blocked) => return Ok(MigrationPlan::Blocked(blocked)),
        },
    };

    // Vault leg (V7 guard: a populated target next to the legacy file is a
    // genuine divergence — unless the pair is byte-identical, which is the
    // resumable EXDEV crash window).
    let vault_leg = if legacy_vault_present {
        let dest = agent_vault_path(&home_dir()?, agent);
        if dest.is_file() && !files_identical(&legacy_vault, &dest)? {
            return Ok(MigrationPlan::Blocked(CliError {
                code: codes::VAULT_MIGRATION_BLOCKED,
                cause: format!(
                    "Both the legacy vault ({}) and the per-agent vault ({}) exist \
                     and their contents differ.",
                    legacy_vault.display(),
                    dest.display()
                ),
                remedy: format!(
                    "Inspect both files with 'alf vault list --in <path>' and move the \
                     one you want to keep to {} manually, then delete the other.",
                    dest.display()
                ),
            }));
        }
        Some((legacy_vault, dest))
    } else {
        None
    };

    // Key leg. A fresh WP5 hermes install has no legacy no-agent key file
    // (`~/.hermes/state/.alf-vault-key` was never written under the old None
    // arm), so `legacy_key` is `None` and this leg is a no-op — the pre-WP5
    // behavior is preserved without a special case.
    let key_leg = match (
        &legacy_key,
        vault_key::default_key_path(runtime, Some(agent))?,
    ) {
        (Some(from), Some(to)) => {
            if to.is_file() && !files_identical(from, &to)? {
                return Ok(MigrationPlan::Blocked(CliError {
                    code: codes::VAULT_MIGRATION_BLOCKED,
                    cause: format!(
                        "Both the legacy vault key ({}) and the per-agent key ({}) exist \
                         and differ.",
                        from.display(),
                        to.display()
                    ),
                    remedy: format!(
                        "Keep the key that opens the vault at {} and remove the other; \
                         verify with 'alf vault decrypt'.",
                        to.display()
                    ),
                }));
            }
            Some((from.clone(), to))
        }
        _ => None,
    };

    if vault_leg.is_none() && key_leg.is_none() {
        // A legacy vault or key file was seen but neither leg is movable
        // (e.g. the legacy key path resolves to a file that isn't present).
        return Ok(MigrationPlan::NotNeeded);
    }

    Ok(MigrationPlan::Move {
        agent,
        vault: vault_leg,
        key: key_leg,
    })
}

/// The V2–V6 decision table. `Ok(Err(blocked))` is a blocked decision;
/// `Ok(Ok(id))` is the migration target.
#[allow(clippy::type_complexity)]
fn decide_target(
    config: &Config,
    runtime: &str,
    legacy_vault_present: bool,
) -> Result<std::result::Result<Uuid, CliError>> {
    let rows = config.agents_for_runtime(runtime);

    // V3: mapping empty for the runtime.
    if rows.is_empty() {
        return Ok(Err(CliError {
            code: codes::VAULT_MIGRATION_BLOCKED,
            cause: format!(
                "A legacy vault or key exists but no agents are mapped for runtime \
                 '{runtime}', so there is no migration target."
            ),
            remedy: format!("Run 'alf check -r {runtime}' to discover agents, then re-run."),
        }));
    }

    let enabled: Vec<_> = rows.iter().filter(|r| r.enabled).collect();

    // V4: rows exist, all disabled.
    if enabled.is_empty() {
        let first = rows[0].runtime_agent.clone();
        return Ok(Err(CliError {
            code: codes::VAULT_MIGRATION_BLOCKED,
            cause: format!(
                "A legacy vault or key exists but no agent is enabled for runtime \
                 '{runtime}', so the migration target is ambiguous."
            ),
            remedy: format!(
                "Run 'alf agents enable {first}' to enable the owning agent, or \
                  'alf vault migrate -r {runtime} --agent <alias-or-id>' to choose \
                 the target explicitly."
            ),
        }));
    }

    // V6: cross-runtime evidence — the legacy vault is runtime-neutral, so
    // another runtime's legacy key or mapped rows make the target ambiguous
    // even with a sole enabled row here.
    if legacy_vault_present {
        if let Some(evidence) = cross_runtime_evidence(config, runtime)? {
            return Ok(Err(CliError {
                code: codes::VAULT_MIGRATION_BLOCKED,
                cause: format!(
                    "The legacy vault is install-scoped (runtime-neutral) and this \
                     install shows other-runtime evidence ({evidence}), so the \
                     migration target is ambiguous."
                ),
                remedy: format!(
                    "Run 'alf vault migrate -r {runtime} --agent <alias-or-id>' to \
                     choose the owning agent explicitly."
                ),
            }));
        }
    }

    // V5: more than one enabled row.
    if enabled.len() > 1 {
        let aliases = enabled
            .iter()
            .map(|r| r.runtime_agent.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Ok(Err(CliError {
            code: codes::VAULT_MIGRATION_BLOCKED,
            cause: format!(
                "A legacy vault or key exists and {} agents are enabled for runtime \
                 '{runtime}' ({aliases}), so the migration target is ambiguous.",
                enabled.len()
            ),
            remedy: format!(
                "Run 'alf vault migrate -r {runtime} --agent <alias-or-id>' to choose \
                 the owning agent explicitly."
            ),
        }));
    }

    // V2: the sole enabled row (extra mapped-but-disabled rows don't block —
    // sole-enabled is exactly the None-selector's answer).
    Ok(Ok(enabled[0].alf_agent_id))
}

/// X: another runtime's legacy key file exists, or rows tagged with another
/// runtime exist. Returns a human-readable description of the evidence.
fn cross_runtime_evidence(config: &Config, runtime: &str) -> Result<Option<String>> {
    for other in ["openclaw", "zeroclaw"] {
        if other == runtime {
            continue;
        }
        if let Some(p) = vault_key::legacy_default_key_path(other)? {
            if p.is_file() {
                return Ok(Some(format!(
                    "a legacy {other} vault key at {}",
                    p.display()
                )));
            }
        }
    }
    if let Some(row) = config
        .agents
        .iter()
        .find(|a| a.runtime.as_deref().is_some_and(|r| r != runtime))
    {
        return Ok(Some(format!(
            "mapped agents for runtime '{}' (e.g. '{}')",
            row.runtime.as_deref().unwrap_or("?"),
            row.runtime_agent
        )));
    }
    Ok(None)
}

/// Vault leg: `create_dir_all` → same-tree `rename` (atomic; EXDEV fallback:
/// 0600 copy + byte-compare read-back + delete legacy). A pure move — the
/// vault is never parsed or rewritten.
fn move_vault(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }

    if to.is_file() {
        // plan_migration verified byte-identity: this is the resumable EXDEV
        // pair — finish by deleting the legacy copy.
        fs::remove_file(from)
            .with_context(|| format!("Failed to remove legacy vault {}", from.display()))?;
        return Ok(());
    }
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc_exdev()) => {
            copy_private_verified(from, to)?;
            fs::remove_file(from)
                .with_context(|| format!("Failed to remove legacy vault {}", from.display()))
        }
        Err(e) => {
            Err(e).with_context(|| format!("Failed to move {} to {}", from.display(), to.display()))
        }
    }
}

/// Key leg: `create_dir_all` + rename, content unchanged.
fn move_key(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    if to.is_file() {
        // plan_migration verified byte-identity — finish the resumed move.
        fs::remove_file(from)
            .with_context(|| format!("Failed to remove legacy key {}", from.display()))?;
        return Ok(());
    }
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc_exdev()) => {
            copy_private_verified(from, to)?;
            fs::remove_file(from)
                .with_context(|| format!("Failed to remove legacy key {}", from.display()))
        }
        Err(e) => {
            Err(e).with_context(|| format!("Failed to move {} to {}", from.display(), to.display()))
        }
    }
}

/// EXDEV fallback: 0600 copy, then read back and byte-compare before the
/// caller deletes the source.
fn copy_private_verified(from: &Path, to: &Path) -> Result<()> {
    let content =
        fs::read_to_string(from).with_context(|| format!("Failed to read {}", from.display()))?;
    crate::fs_private::write_private(to, &content)
        .with_context(|| format!("Failed to write {}", to.display()))?;
    let written =
        fs::read_to_string(to).with_context(|| format!("Failed to read back {}", to.display()))?;
    if written != content {
        anyhow::bail!(
            "Copy verification failed: {} does not match {} — legacy file left in place",
            to.display(),
            from.display()
        );
    }
    Ok(())
}

fn files_identical(a: &Path, b: &Path) -> Result<bool> {
    let ba = fs::read(a).with_context(|| format!("Failed to read {}", a.display()))?;
    let bb = fs::read(b).with_context(|| format!("Failed to read {}", b.display()))?;
    Ok(ba == bb)
}

fn home_dir() -> Result<PathBuf> {
    alf_core::home_dir().context("Could not determine home directory")
}

/// `EXDEV` ("cross-device link") without pulling in libc.
fn libc_exdev() -> i32 {
    18
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentEntry;
    use crate::context::tests::{RestoreEnv, HOME_LOCK};
    use alf_core::CredentialsDocument;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn uuid(n: u8) -> Uuid {
        let mut bytes = [0u8; 16];
        bytes[15] = n;
        Uuid::from_bytes(bytes)
    }

    fn entry(runtime: &str, alias: &str, id: Uuid, enabled: bool) -> AgentEntry {
        AgentEntry {
            runtime: Some(runtime.into()),
            runtime_agent: alias.into(),
            runtime_agent_id: None,
            alf_agent_id: id,
            workspace: "/ws".into(),
            enabled,
            extra: BTreeMap::new(),
        }
    }

    fn config_with(rows: Vec<AgentEntry>) -> Config {
        Config {
            agents: rows,
            ..Default::default()
        }
    }

    /// Isolated ALF_HOME with an optional legacy vault and/or legacy key.
    fn seed_home(tmp: &TempDir, vault: bool, key: bool) -> (PathBuf, PathBuf) {
        let legacy_vault = tmp
            .path()
            .join(".alf")
            .join("vault")
            .join("credentials.json");
        let legacy_key = tmp
            .path()
            .join(".openclaw")
            .join("state")
            .join(".alf-vault-key");
        if vault {
            fs::create_dir_all(legacy_vault.parent().unwrap()).unwrap();
            fs::write(
                &legacy_vault,
                r#"{"credentials":[{
                    "id":"00000000-0000-0000-0000-000000000009",
                    "agent_id":"00000000-0000-0000-0000-000000000009",
                    "service":"email","credential_type":"account",
                    "encrypted_payload":"Q0lQSEVS",
                    "encryption":{"algorithm":"xchacha20-poly1305","nonce":"Tk9OQ0U="},
                    "created_at":"2026-01-01T00:00:00Z",
                    "tags":["alf-vault"],
                    "future_field":"preserved"
                }],"future_doc_field":true}"#,
            )
            .unwrap();
        }
        if key {
            fs::create_dir_all(legacy_key.parent().unwrap()).unwrap();
            fs::write(&legacy_key, "a2V5LWJ5dGVz").unwrap();
        }
        (legacy_vault, legacy_key)
    }

    fn code_of(err: &CliError) -> &'static str {
        err.code
    }

    /// V1 / K1: nothing legacy ⇒ NotNeeded, zero writes.
    #[test]
    fn neither_legacy_file_is_not_needed() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());

        let config = config_with(vec![entry("openclaw", "main", uuid(1), true)]);
        assert!(matches!(
            ensure_migrated(&config, "openclaw", None).unwrap(),
            MigrationOutcome::NotNeeded
        ));
        assert!(!tmp.path().join(".alf").exists(), "zero writes");
    }

    /// V2 + K2: sole enabled row migrates both legs; records travel verbatim
    /// (unknown fields preserved); second run is NotNeeded (idempotence).
    #[test]
    fn sole_enabled_migrates_vault_and_key_idempotently() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());
        let (legacy_vault, legacy_key) = seed_home(&tmp, true, true);

        let config = config_with(vec![
            entry("openclaw", "main", uuid(1), true),
            // A vestigial disabled row must not block (V2 nuance).
            entry("openclaw", "old-default", uuid(2), false),
        ]);
        let outcome = ensure_migrated(&config, "openclaw", None).unwrap();
        let (vault, key, agent) = match outcome {
            MigrationOutcome::Migrated { vault, key, agent } => (vault, key, agent),
            _ => panic!("expected Migrated"),
        };
        assert_eq!(agent, uuid(1));

        // Files moved.
        assert!(!legacy_vault.exists(), "legacy vault must be gone");
        assert!(!legacy_key.exists(), "legacy key must be gone");
        let vault = vault.unwrap();
        let key = key.unwrap();
        assert_eq!(vault, agent_vault_path(tmp.path(), uuid(1)));
        assert!(key.to_string_lossy().contains(&uuid(1).to_string()));
        assert_eq!(fs::read_to_string(&key).unwrap(), "a2V5LWJ5dGVz");

        // Records travel verbatim — the move never rewrites the file.
        let doc: CredentialsDocument =
            serde_json::from_str(&fs::read_to_string(&vault).unwrap()).unwrap();
        assert_eq!(doc.credentials.len(), 1);
        assert_eq!(doc.credentials[0].encrypted_payload, "Q0lQSEVS");
        assert_eq!(
            doc.credentials[0].extra.get("future_field"),
            Some(&serde_json::json!("preserved"))
        );
        assert_eq!(
            doc.extra.get("future_doc_field"),
            Some(&serde_json::json!(true))
        );

        // Idempotence: second run does nothing.
        assert!(matches!(
            ensure_migrated(&config, "openclaw", None).unwrap(),
            MigrationOutcome::NotNeeded
        ));
    }

    /// V3: legacy vault + empty mapping blocks with the check remedy.
    #[test]
    fn empty_mapping_blocks_with_check_remedy() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());
        seed_home(&tmp, true, false);

        let outcome = ensure_migrated(&Config::default(), "openclaw", None).unwrap();
        match outcome {
            MigrationOutcome::Blocked(err) => {
                assert_eq!(code_of(&err), codes::VAULT_MIGRATION_BLOCKED);
                assert!(err.remedy.contains("alf check"));
            }
            _ => panic!("expected Blocked"),
        }
    }

    /// V4: rows exist but none enabled — remedy names enable AND the explicit
    /// migrate escape hatch.
    #[test]
    fn all_disabled_blocks_with_enable_remedy() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());
        seed_home(&tmp, true, false);

        let config = config_with(vec![entry("openclaw", "main", uuid(1), false)]);
        match ensure_migrated(&config, "openclaw", None).unwrap() {
            MigrationOutcome::Blocked(err) => {
                assert!(err.remedy.contains("alf agents enable main"));
                assert!(err.remedy.contains("alf vault migrate"));
            }
            _ => panic!("expected Blocked"),
        }
    }

    /// V5: two enabled rows block, naming the aliases; the explicit target
    /// bypasses the block (K3 companion: both legs then move).
    #[test]
    fn two_enabled_blocks_until_explicit_target() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());
        let (legacy_vault, legacy_key) = seed_home(&tmp, true, true);

        let config = config_with(vec![
            entry("openclaw", "main", uuid(1), true),
            entry("openclaw", "helper", uuid(2), true),
        ]);
        match ensure_migrated(&config, "openclaw", None).unwrap() {
            MigrationOutcome::Blocked(err) => {
                assert!(err.cause.contains("main") && err.cause.contains("helper"));
                assert!(err.remedy.contains("alf vault migrate"));
                assert!(err.remedy.contains("--agent"));
            }
            _ => panic!("expected Blocked"),
        }
        // K3: the key leg is blocked with the vault leg — nothing moved.
        assert!(legacy_vault.exists() && legacy_key.exists());

        // The human decision resolves it.
        match ensure_migrated(&config, "openclaw", Some(uuid(2))).unwrap() {
            MigrationOutcome::Migrated { agent, .. } => assert_eq!(agent, uuid(2)),
            _ => panic!("expected Migrated"),
        }
        assert!(!legacy_vault.exists() && !legacy_key.exists());
        assert!(agent_vault_path(tmp.path(), uuid(2)).is_file());
    }

    /// V6: cross-runtime evidence blocks even a sole enabled row (the legacy
    /// vault is runtime-neutral).
    #[test]
    fn cross_runtime_evidence_blocks() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());
        seed_home(&tmp, true, false);
        // Another runtime's legacy key.
        let zc_key = tmp
            .path()
            .join(".zeroclaw")
            .join("state")
            .join(".alf-vault-key");
        fs::create_dir_all(zc_key.parent().unwrap()).unwrap();
        fs::write(&zc_key, "emNrZXk=").unwrap();

        let config = config_with(vec![entry("openclaw", "main", uuid(1), true)]);
        match ensure_migrated(&config, "openclaw", None).unwrap() {
            MigrationOutcome::Blocked(err) => {
                assert!(err.cause.contains("zeroclaw"), "{}", err.cause);
                assert!(err.remedy.contains("alf vault migrate -r openclaw --agent"));
            }
            _ => panic!("expected Blocked"),
        }

        // Rows for another runtime are evidence too.
        fs::remove_file(&zc_key).unwrap();
        let config = config_with(vec![
            entry("openclaw", "main", uuid(1), true),
            entry("zeroclaw", "default", uuid(3), false),
        ]);
        match ensure_migrated(&config, "openclaw", None).unwrap() {
            MigrationOutcome::Blocked(err) => {
                assert!(err.cause.contains("zeroclaw"), "{}", err.cause);
            }
            _ => panic!("expected Blocked"),
        }
    }

    /// V7: a diverged legacy/per-agent pair blocks even with an explicit
    /// target; a byte-identical pair resumes (EXDEV crash window).
    #[test]
    fn diverged_pair_blocks_identical_pair_resumes() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());
        let (legacy_vault, _) = seed_home(&tmp, true, false);

        let dest = agent_vault_path(tmp.path(), uuid(1));
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(&dest, r#"{"credentials":[]}"#).unwrap();

        let config = config_with(vec![entry("openclaw", "main", uuid(1), true)]);
        match ensure_migrated(&config, "openclaw", Some(uuid(1))).unwrap() {
            MigrationOutcome::Blocked(err) => {
                assert!(err.cause.contains(&legacy_vault.display().to_string()));
                assert!(err.cause.contains(&dest.display().to_string()));
            }
            _ => panic!("expected Blocked"),
        }

        // Byte-identical pair (crashed EXDEV copy) resumes: legacy removed,
        // destination intact.
        fs::copy(&legacy_vault, &dest).unwrap();
        let dest_bytes = fs::read(&dest).unwrap();
        match ensure_migrated(&config, "openclaw", None).unwrap() {
            MigrationOutcome::Migrated { agent, .. } => assert_eq!(agent, uuid(1)),
            _ => panic!("expected Migrated"),
        }
        assert!(!legacy_vault.exists());
        assert_eq!(
            fs::read(&dest).unwrap(),
            dest_bytes,
            "the resumed move must not rewrite the destination"
        );
    }

    /// Key-only migration: no legacy vault, a legacy key moves to the
    /// per-agent path with content unchanged.
    #[test]
    fn key_only_migration_moves_key() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());
        let (_, legacy_key) = seed_home(&tmp, false, true);

        let config = config_with(vec![entry("openclaw", "main", uuid(1), true)]);
        match ensure_migrated(&config, "openclaw", None).unwrap() {
            MigrationOutcome::Migrated { vault, key, agent } => {
                assert_eq!(agent, uuid(1));
                assert!(vault.is_none());
                let key = key.unwrap();
                assert_eq!(fs::read_to_string(&key).unwrap(), "a2V5LWJ5dGVz");
            }
            _ => panic!("expected Migrated"),
        }
        assert!(!legacy_key.exists());
    }

    /// Vault-only migration: no legacy key ⇒ the key leg is a no-op (K1).
    #[test]
    fn vault_only_migration_skips_key_leg() {
        let _guard = HOME_LOCK.lock().unwrap();
        let _restore = RestoreEnv::snapshot();
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());
        seed_home(&tmp, true, false);

        let config = config_with(vec![entry("openclaw", "main", uuid(1), true)]);
        match ensure_migrated(&config, "openclaw", None).unwrap() {
            MigrationOutcome::Migrated { vault, key, .. } => {
                assert!(vault.is_some());
                assert!(key.is_none());
            }
            _ => panic!("expected Migrated"),
        }
    }
}
