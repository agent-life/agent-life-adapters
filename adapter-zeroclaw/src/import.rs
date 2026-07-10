//! Import an `.alf` archive into a ZeroClaw workspace.
//!
//! Restore has two complementary halves:
//! 1. **Files** — raw-source restore (preferred, lossless for ZeroClaw→ZeroClaw)
//!    or cross-runtime reconstruction from ALF structured data (identity,
//!    principals, memory-as-Markdown).
//! 2. **Memory** — for the SQLite backend, the agent's slice is restored into
//!    the shared `data/memory/brain.db` (per `agent_id`, total or merge),
//!    bootstrapping the store from the captured DDL when it is lazily absent and
//!    leaving every other agent's rows untouched. This runs independently of the
//!    file half because `brain.db` is not a raw source.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;

use alf_core::{AlfReader, ArchiveEnumeration, FileEntry, RestoreMode, VaultKey};

use crate::brain_db::{self, ArchivedAgent, BrainDbSchema, NativeRow};
use crate::ImportReport;

// ---------------------------------------------------------------------------
// Import entry point
// ---------------------------------------------------------------------------

/// Import an `.alf` archive into a ZeroClaw workspace.
///
/// Creates workspace directories if they don't exist. Prefers raw source files
/// when available. Falls back to reconstructing workspace from ALF data.
///
/// `vault_key`, when supplied, decrypts credential records and writes a
/// fresh `auth_profiles.json` to the workspace.
pub fn import(
    alf_file: &Path,
    workspace: &Path,
    vault_key: Option<&VaultKey>,
    mode: RestoreMode,
) -> Result<ImportReport> {
    let file = std::fs::File::open(alf_file)
        .with_context(|| format!("Failed to open ALF file: {}", alf_file.display()))?;
    let reader = std::io::BufReader::new(file);
    let mut alf = AlfReader::new(reader)?;

    let manifest = alf.manifest().clone();
    let agent_name = manifest.agent.name.clone();
    let agent_id = manifest.agent.id;

    let mut warnings = Vec::new();

    // Ensure workspace directory structure
    fs::create_dir_all(workspace)?;
    fs::create_dir_all(workspace.join("memory"))?;

    // Check for raw/zeroclaw/ sources
    let file_names = alf.file_names();
    let raw_prefix = "raw/zeroclaw/";
    let has_raw = file_names.iter().any(|f| f.starts_with(raw_prefix));

    if has_raw {
        restore_raw_sources(&mut alf, workspace, raw_prefix, &file_names)?;
    } else {
        warnings.push(
            "No raw/zeroclaw/ sources in archive — reconstructing from structured data."
                .to_string(),
        );
        reconstruct_from_structured(&mut alf, workspace, &mut warnings)?;
    }

    // Write agent ID file
    let id_file = workspace.join(".alf-agent-id");
    let _ = fs::write(&id_file, agent_id.to_string());

    // Inert-on-restore (D3): the raw tree carries `.alf-include.json`, so
    // external include entries come back unverified — a hostile/compromised
    // archive's external entries are not packed on the next sync until the
    // local user re-confirms them with `alf add --external`.
    match alf_core::include::mark_external_inert(workspace) {
        Ok(0) => {}
        Ok(n) => warnings.push(format!(
            "{n} external file entry(ies) imported as inert; re-add with \
             `alf add --external` to include them in sync."
        )),
        Err(e) => warnings.push(format!("could not mark external entries inert: {e}")),
    }

    // Count what we imported
    let identity = alf.read_identity()?;
    let principals = alf.read_principals()?;
    let credentials = alf.read_credentials()?;
    let all_memory = alf.read_all_memory()?;

    let credentials_count = credentials
        .as_ref()
        .map(|c| c.credentials.len() as u32)
        .unwrap_or(0);

    // Restore credentials. Records the agent added with `alf vault add` carry
    // the `alf-vault` tag — they go back to the agent's ALF vault as-is (still
    // encrypted, no key needed). Any other records came from a legacy archive's
    // runtime keystore and need the vault key to decrypt.
    if let Some(doc) = credentials {
        let doc_extra = doc.extra.clone();
        let (vault_records, auth_records): (Vec<_>, Vec<_>) = doc
            .credentials
            .into_iter()
            .partition(|c| c.tags.iter().any(|t| t == "alf-vault"));

        let vaulted = restore_agent_vault(vault_records, doc_extra, agent_id, workspace)?;
        if vaulted > 0 {
            warnings.push(format!(
                "Restored {vaulted} vaulted account(s) to the agent vault \
                 (inspect with `alf vault list`, decrypt with `alf vault decrypt`)."
            ));
        }

        if !auth_records.is_empty() {
            let auth_doc = alf_core::CredentialsDocument {
                credentials: auth_records,
                extra: std::collections::HashMap::new(),
            };
            match vault_key {
                Some(key) => match restore_credentials(&auth_doc, key, workspace) {
                    Ok(n) => {
                        if n > 0 {
                            warnings.push(format!(
                                "Restored {n} credential(s) into auth_profiles.json. \
                                 Verify with ZeroClaw."
                            ));
                        }
                    }
                    Err(e) => warnings.push(format!("Credential restore failed: {e}")),
                },
                None => warnings.push(format!(
                    "{} credential(s) found in archive (metadata only). \
                     Pass --vault-key-file or set ALF_VAULT_KEY to restore secret material.",
                    auth_doc.credentials.len()
                )),
            }
        }
    }

    // Restore the agent's brain.db slice (SQLite backend). Independent of the
    // file restore above — brain.db is not a raw source. Only for archives that
    // carry ZeroClaw provenance (manifest.agent.extra.zeroclaw_alias); cross-
    // runtime archives fall through to the Markdown reconstruction path.
    if let Some(archived) = archived_agent(&manifest) {
        let install_root = crate::export::zeroclaw_home(workspace);
        let db_path = crate::export::brain_db_path(&install_root);
        let rows: Vec<NativeRow> = all_memory
            .iter()
            .filter_map(NativeRow::from_record)
            .collect();
        let schema = read_schema_sidecar(&mut alf)?;
        let can_bootstrap = schema.as_ref().is_some_and(|s| !s.ddl.is_empty());

        if !db_path.is_file() && !can_bootstrap {
            warnings.push(format!(
                "no brain.db at {} and the archive carries no schema — {} memory row(s) not \
                 restored to SQLite; run `zeroclaw memory reindex` to create the store, then \
                 re-run restore",
                db_path.display(),
                rows.len()
            ));
        } else if !rows.is_empty() || db_path.is_file() {
            let schema = schema.unwrap_or_default();
            let now = Utc::now().to_rfc3339();
            let outcome =
                brain_db::restore_agent_slice(&db_path, &schema, &archived, &rows, mode, &now)
                    .context("restoring the agent's memory slice into brain.db")?;
            let mode_label = match mode {
                RestoreMode::Total => "total",
                RestoreMode::Merge => "merge",
            };
            warnings.push(format!(
                "Restored {} memory row(s) to agent '{}' in brain.db ({mode_label} mode).",
                outcome.rows_written, archived.alias
            ));
            if outcome.bootstrapped {
                warnings.push(format!(
                    "Created brain.db at {} from the captured schema.",
                    db_path.display()
                ));
            }
            if let Some(from) = outcome.remapped_from {
                warnings.push(format!(
                    "Alias '{}' already existed under id {} — restored under that id \
                     (archive id {from} kept as provenance).",
                    archived.alias, outcome.resolved_agent_id
                ));
            }
        }
    }

    Ok(ImportReport {
        agent_name,
        memory_records: all_memory.len() as u64,
        identity_imported: identity.is_some(),
        principals_count: principals
            .as_ref()
            .map(|p| p.principals.len() as u32)
            .unwrap_or(0),
        credentials_count,
        warnings,
    })
}

/// Extract the ZeroClaw agent provenance an export stamped into
/// `manifest.agent.extra`. `None` for archives without it (cross-runtime, or a
/// pre-WP3 ZeroClaw archive) — those skip the brain.db slice restore.
fn archived_agent(manifest: &alf_core::Manifest) -> Option<ArchivedAgent> {
    let extra = &manifest.agent.extra;
    let alias = extra
        .get("zeroclaw_alias")
        .and_then(|v| v.as_str())?
        .to_string();
    let id = extra
        .get("zeroclaw_agent_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    Some(ArchivedAgent {
        id,
        alias,
        created_at: None,
    })
}

/// Read the `brain.db` schema sidecar (`raw/zeroclaw/.alf-brain-db-schema.json`)
/// when present. Consumed by the slice restore; never written to the workspace.
fn read_schema_sidecar<R: std::io::Read + std::io::Seek>(
    alf: &mut AlfReader<R>,
) -> Result<Option<BrainDbSchema>> {
    let path = format!("raw/zeroclaw/{}", brain_db::SCHEMA_SIDECAR);
    if !alf.file_names().iter().any(|f| f == &path) {
        return Ok(None);
    }
    let bytes = alf.read_raw_entry(&path)?;
    let schema: BrainDbSchema =
        serde_json::from_slice(&bytes).context("parsing brain.db schema sidecar")?;
    Ok(Some(schema))
}

// ---------------------------------------------------------------------------
// Dry-run archive enumeration
// ---------------------------------------------------------------------------

/// Enumerate the workspace files an [`import`] would write, without touching
/// the filesystem. Backs `alf restore --dry-run`.
///
/// Mirrors the path decision in [`import`]: when the archive carries
/// `raw/zeroclaw/` entries the list is exact (paths and sizes from the
/// archive); otherwise restore reconstructs files from structured layers, and
/// the preview is a coarse, `size: 0` approximation with a warning.
pub fn enumerate_archive(alf_file: &Path) -> Result<ArchiveEnumeration> {
    let file = std::fs::File::open(alf_file)
        .with_context(|| format!("Failed to open ALF file: {}", alf_file.display()))?;
    let reader = std::io::BufReader::new(file);
    let mut alf = AlfReader::new(reader)?;

    let file_names = alf.file_names();
    let raw_prefix = "raw/zeroclaw/";
    let mut files = Vec::new();
    let mut warnings = Vec::new();

    let has_raw = file_names.iter().any(|f| f.starts_with(raw_prefix));
    if has_raw {
        let sidecar = format!("{raw_prefix}{}", brain_db::SCHEMA_SIDECAR);
        let mut raw: Vec<String> = file_names
            .iter()
            .filter(|f| f.starts_with(raw_prefix) && f.len() > raw_prefix.len() && **f != sidecar)
            .cloned()
            .collect();
        raw.sort();
        for name in &raw {
            let size = alf.entry_size(name)?;
            files.push(FileEntry {
                path: name[raw_prefix.len()..].to_string(),
                size,
            });
        }
    } else {
        warnings.push(
            "Archive has no raw/zeroclaw/ sources — restore would reconstruct files \
             from structured data; the list below is approximate and sizes are unknown."
                .to_string(),
        );
        let manifest = alf.manifest();
        if manifest.layers.identity.is_some() {
            for name in ["SOUL.md", "IDENTITY.md", "AGENTS.md"] {
                files.push(FileEntry {
                    path: name.to_string(),
                    size: 0,
                });
            }
        }
        if manifest.layers.principals.is_some() {
            files.push(FileEntry {
                path: "USER.md".to_string(),
                size: 0,
            });
        }
        let memory_records = manifest
            .layers
            .memory
            .as_ref()
            .map(|m| m.record_count)
            .unwrap_or(0);
        if memory_records > 0 {
            files.push(FileEntry {
                path: "MEMORY.md".to_string(),
                size: 0,
            });
        }
    }

    Ok(ArchiveEnumeration { files, warnings })
}

fn restore_credentials(
    doc: &alf_core::CredentialsDocument,
    key: &VaultKey,
    workspace: &Path,
) -> Result<usize> {
    use serde_json::{Map, Value};

    let mut profiles = Map::new();
    let mut restored = 0;

    for cred in &doc.credentials {
        if cred.encryption.algorithm == "none" || cred.encrypted_payload == "<not-exported>" {
            continue;
        }
        let plaintext = match alf_core::decrypt_record(cred, key) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let payload = match alf_core::VaultPayload::from_json_bytes(&plaintext) {
            Ok(p) => p,
            Err(_) => continue,
        };

        let key_name = cred
            .label
            .clone()
            .unwrap_or_else(|| format!("{}:{}", cred.service, cred.id));
        let mut entry = Map::new();
        entry.insert("service".into(), Value::String(cred.service.clone()));
        entry.insert("kind".into(), Value::String(payload.kind.clone()));
        entry.insert("secret".into(), Value::String(payload.secret.clone()));
        if let Some(u) = payload.username.clone() {
            entry.insert("username".into(), Value::String(u));
        }
        if let Some(section) = payload.extra.get("zeroclaw_section").cloned() {
            entry.insert("section".into(), section);
        }
        if let Some(field) = payload.extra.get("zeroclaw_field").cloned() {
            entry.insert("field".into(), field);
        }
        profiles.insert(key_name, Value::Object(entry));
        restored += 1;
    }

    if restored == 0 {
        return Ok(0);
    }

    let target = workspace.join("auth_profiles.json");
    let serialized = serde_json::to_string_pretty(&Value::Object(profiles))?;
    fs::write(&target, serialized)
        .with_context(|| format!("Failed to write {}", target.display()))?;

    Ok(restored)
}

/// Restore `alf-vault`-tagged records — accounts the agent added with
/// `alf vault add` — into the archive agent's own ALF vault
/// (`~/.alf/vault/{agent_id}/credentials.json`).
///
/// Records stay AEAD-encrypted exactly as the archive carried them: no vault
/// key is required, and the agent decrypts on demand with `alf vault decrypt`.
/// Full overwrite (D6): the archive is the truth for THIS agent — per-agent
/// scoping is what makes that safe. The archive doc's `extra` is preserved
/// verbatim (unknown doc-level fields survive every restore). This is the
/// write-twin of `export::load_agent_vault`.
fn restore_agent_vault(
    records: Vec<alf_core::CredentialRecord>,
    doc_extra: std::collections::HashMap<String, serde_json::Value>,
    agent_id: uuid::Uuid,
    workspace: &Path,
) -> Result<usize> {
    if records.is_empty() {
        return Ok(0);
    }
    let count = records.len();
    let doc = alf_core::CredentialsDocument {
        credentials: records,
        extra: doc_extra,
    };

    // The ALF vault lives under ALF's own home (`~/.alf/vault/`), runtime-
    // neutral and deliberately separate from any runtime keystore. Falls back
    // to a workspace-local copy the user can move when HOME is unset.
    let target = alf_core::home_dir()
        .map(|h| alf_core::agent_vault_path(&h, agent_id))
        .unwrap_or_else(|| workspace.join(".alf-restored-credentials.json"));

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let serialized = serde_json::to_string_pretty(&doc)?;
    fs::write(&target, serialized)
        .with_context(|| format!("Failed to write {}", target.display()))?;

    Ok(count)
}

// ---------------------------------------------------------------------------
// Path 1: Raw source restore (lossless)
// ---------------------------------------------------------------------------

fn restore_raw_sources<R: std::io::Read + std::io::Seek>(
    alf: &mut AlfReader<R>,
    workspace: &Path,
    prefix: &str,
    file_names: &[String],
) -> Result<()> {
    // Everything an export captured (config.toml, SOUL.md, memory/*, …) was
    // taken relative to the install root; restore it back there. The install
    // root is resolved from `workspace`, which may be a per-agent binding
    // (`<root>/agents/<alias>/workspace`) or the root itself.
    let install_root = crate::export::zeroclaw_home(workspace);

    let mut total_bytes: u64 = 0;
    for name in file_names {
        if !name.starts_with(prefix) {
            continue;
        }
        let relative = &name[prefix.len()..];
        if relative.is_empty() || relative == brain_db::SCHEMA_SIDECAR {
            // The schema sidecar is internal — consumed by the brain.db restore,
            // never written into the install (mirrors Hermes).
            continue;
        }

        // Reject path-traversal / absolute entry names relative to the install
        // root — a hostile or compromised-server archive must not escape it
        // (Zip Slip; see threat model A4.1/A1.1).
        let target = alf_core::safe_extract_path(&install_root, relative)
            .with_context(|| format!("refusing to extract archive entry {name:?}"))?;

        // The raw files (config.toml, SOUL.md, memory/*) live at the SHARED
        // install root, not per agent. When restoring one agent into a populated
        // install, do NOT clobber files that already exist — they belong to the
        // live install / other agents, and the archived config.toml is redacted
        // (overwriting it would drop the running secrets). A fresh restore (new
        // machine) has none of these, so it still gets the full workspace.
        if target.exists() {
            continue;
        }

        // Bound decompression to defend against zip bombs.
        let data = alf.read_raw_entry_capped(name, alf_core::MAX_RAW_ENTRY_BYTES)?;
        total_bytes = total_bytes.saturating_add(data.len() as u64);
        if total_bytes > alf_core::MAX_RAW_TOTAL_BYTES {
            anyhow::bail!(
                "raw source restore exceeds {} bytes (possible zip bomb)",
                alf_core::MAX_RAW_TOTAL_BYTES
            );
        }

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, &data)
            .with_context(|| format!("Failed to write {}", target.display()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Path 2: Cross-runtime reconstruction
// ---------------------------------------------------------------------------

fn reconstruct_from_structured<R: std::io::Read + std::io::Seek>(
    alf: &mut AlfReader<R>,
    workspace: &Path,
    warnings: &mut Vec<String>,
) -> Result<()> {
    // Identity → SOUL.md (+ IDENTITY.md, AGENTS.md)
    if let Some(identity) = alf.read_identity()? {
        if let Some(ref prose) = identity.prose {
            if let Some(ref soul) = prose.soul {
                fs::write(workspace.join("SOUL.md"), soul)?;
            }
            if let Some(ref profile) = prose.identity_profile {
                fs::write(workspace.join("IDENTITY.md"), profile)?;
            }
            if let Some(ref instructions) = prose.operating_instructions {
                fs::write(workspace.join("AGENTS.md"), instructions)?;
            }
        } else if let Some(ref structured) = identity.structured {
            // Synthesize a minimal SOUL.md
            let name = structured
                .names
                .as_ref()
                .map(|n| n.primary.as_str())
                .unwrap_or("Agent");
            let role = structured.role.as_deref().unwrap_or("AI Assistant");
            let soul = format!("# {name}\n\n{role}\n");
            fs::write(workspace.join("SOUL.md"), soul)?;
        }

        // If AIEOS raw source is present, write identity.json
        if identity.source_format.as_deref() == Some("aieos") {
            if let Some(ref raw) = identity.raw_source {
                let json = serde_json::to_string_pretty(raw)?;
                fs::write(workspace.join("identity.json"), json)?;
            }
        }
    }

    // Principals → USER.md
    if let Some(principals) = alf.read_principals()? {
        if let Some(principal) = principals.principals.first() {
            if let Some(ref prose) = principal.profile.prose {
                if let Some(ref user_profile) = prose.user_profile {
                    fs::write(workspace.join("USER.md"), user_profile)?;
                }
            } else if let Some(ref structured) = principal.profile.structured {
                let name = structured.name.as_deref().unwrap_or("User");
                let mut content = format!("# {name}\n");
                if let Some(ref tz) = structured.timezone {
                    content.push_str(&format!("\n## Timezone\n\n{tz}\n"));
                }
                fs::write(workspace.join("USER.md"), content)?;
            }
        }
    }

    // Memory records → memory/YYYY-MM-DD.md files
    let all_records = alf.read_all_memory()?;
    if all_records.is_empty() {
        return Ok(());
    }

    // Group by namespace/origin for reconstruction
    let mut core_sections: Vec<String> = Vec::new();
    let mut daily_groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut other_files: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for record in &all_records {
        // Skip tombstones/replaced records (WP4.1 §8.1) — ZeroClaw's brain.db
        // extractor is the one adapter that actually emits `Superseded` rows
        // (from `superseded_by`), so without this filter a cross-runtime
        // restore resurrects them next to their replacements.
        if !record.status.is_materialized() {
            continue;
        }
        let origin_file = record.source.origin_file.as_deref().unwrap_or("");

        match record.namespace.as_str() {
            "core" => {
                core_sections.push(record.content.clone());
            }
            "daily" => {
                let key = if !origin_file.is_empty() {
                    origin_file.to_string()
                } else if let Some(observed) = record.temporal.observed_at {
                    format!("memory/{}.md", observed.format("%Y-%m-%d"))
                } else {
                    format!(
                        "memory/{}.md",
                        record.temporal.created_at.format("%Y-%m-%d")
                    )
                };
                daily_groups
                    .entry(key)
                    .or_default()
                    .push(record.content.clone());
            }
            "conversation" => {
                // Conversation records grouped by date
                let key = format!(
                    "memory/{}.md",
                    record.temporal.created_at.format("%Y-%m-%d")
                );
                daily_groups
                    .entry(key)
                    .or_default()
                    .push(record.content.clone());
            }
            "session" => {
                let key = if !origin_file.is_empty() {
                    origin_file.to_string()
                } else {
                    let short_id = &record.id.to_string()[..8];
                    format!("memory/session_{short_id}.md")
                };
                other_files
                    .entry(key)
                    .or_default()
                    .push(record.content.clone());
            }
            _ => {
                let key = if !origin_file.is_empty() {
                    origin_file.to_string()
                } else {
                    format!("memory/{}.md", record.namespace)
                };
                other_files
                    .entry(key)
                    .or_default()
                    .push(record.content.clone());
            }
        }
    }

    // Write core sections as a single file (ZeroClaw's memory_store Core)
    if !core_sections.is_empty() {
        let content = core_sections.join("\n\n");
        let target = workspace.join("memory").join("core.md");
        fs::write(&target, content)?;
    }

    // Write daily files
    for (file_path, sections) in &daily_groups {
        let content = sections.join("\n\n");
        let target = workspace.join(file_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, content)?;
    }

    // Write other memory files
    for (file_path, sections) in &other_files {
        let content = sections.join("\n\n");
        let target = workspace.join(file_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, content)?;
    }

    if !all_records.is_empty() {
        warnings.push(format!(
            "Reconstructed {} memory record(s) as Markdown files. \
             Run `zeroclaw` to ingest into SQLite if desired.",
            all_records.len()
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export;
    use rusqlite::Connection;
    use std::fs;
    use std::sync::OnceLock;
    use tempfile::TempDir;

    /// Point `HOME` at a clean temp dir for the whole test process, set once
    /// before any vault access. `import()` writes credentials to `$HOME/.alf/vault`
    /// and auth profiles under `$HOME`; without this, these tests would rewrite
    /// the developer's real vault. Call at the start of any import test.
    fn isolate_home() {
        static TEST_HOME: OnceLock<TempDir> = OnceLock::new();
        TEST_HOME.get_or_init(|| {
            let home = TempDir::new().unwrap();
            std::env::set_var("HOME", home.path());
            home
        });
    }

    /// Minimal vault document JSON: one `alf-vault`-tagged record, optionally
    /// carrying an unknown doc-level extra field.
    fn vault_doc(agent_id: &str, label: &str, with_extra: bool) -> String {
        let extra = if with_extra {
            r#","future_doc_field":"kept""#.to_string()
        } else {
            String::new()
        };
        format!(
            r#"{{"credentials":[{{
                "id":"{agent_id}","agent_id":"{agent_id}",
                "service":"email","credential_type":"account",
                "encrypted_payload":"Q0lQSEVS",
                "encryption":{{"algorithm":"xchacha20-poly1305","nonce":"Tk9OQ0U="}},
                "created_at":"2026-01-01T00:00:00Z",
                "label":"{label}","tags":["alf-vault"]
            }}]{extra}}}"#
        )
    }

    /// A-1 (WP1): export reads ONLY the exporting agent's per-agent vault —
    /// a decoy under another agent's directory must not leak into Layer 4.
    #[test]
    fn export_reads_per_agent_vault() {
        isolate_home();
        let config = "[memory]\nbackend = \"markdown\"";
        let (_dir, ws) = create_zeroclaw_home(config, &[("SOUL.md", "# ZCBot\n\nhi\n")]);
        let my_id = "cfef1150-bbbb-4bbb-8bbb-0000000000b1";
        let other_id = "cfef1150-bbbb-4bbb-8bbb-0000000000b2";
        fs::write(ws.join(".alf-agent-id"), my_id).unwrap();

        let home = alf_core::home_dir().unwrap();
        let mine = alf_core::agent_vault_path(&home, my_id.parse().unwrap());
        fs::create_dir_all(mine.parent().unwrap()).unwrap();
        fs::write(&mine, vault_doc(my_id, "mine", false)).unwrap();
        let decoy = alf_core::agent_vault_path(&home, other_id.parse().unwrap());
        fs::create_dir_all(decoy.parent().unwrap()).unwrap();
        fs::write(&decoy, vault_doc(other_id, "decoy", false)).unwrap();

        let alf_file = _dir.path().join("export.alf");
        export::export(&ws, &alf_file).unwrap();

        let mut reader = alf_core::AlfReader::new(fs::File::open(&alf_file).unwrap()).unwrap();
        let doc = reader.read_credentials().unwrap().expect("Layer 4 present");
        assert_eq!(doc.credentials.len(), 1);
        assert_eq!(doc.credentials[0].label.as_deref(), Some("mine"));
    }

    /// A-2 (WP1): import restores alf-vault records to the archive agent's
    /// own per-agent vault path, fully overwriting a stale local vault (D6)
    /// while preserving the archive doc's unknown extra fields.
    #[test]
    fn import_restores_vault_to_per_agent_path_full_overwrite_preserving_extra() {
        isolate_home();
        let config = "[memory]\nbackend = \"markdown\"";
        let (_dir, ws) = create_zeroclaw_home(config, &[("SOUL.md", "# ZCBot\n\nhi\n")]);
        let my_id = "cfef1150-bbbb-4bbb-8bbb-0000000000b3";
        fs::write(ws.join(".alf-agent-id"), my_id).unwrap();

        let home = alf_core::home_dir().unwrap();
        let vault = alf_core::agent_vault_path(&home, my_id.parse().unwrap());
        fs::create_dir_all(vault.parent().unwrap()).unwrap();
        fs::write(&vault, vault_doc(my_id, "from-archive", true)).unwrap();

        let alf_file = _dir.path().join("export.alf");
        export::export(&ws, &alf_file).unwrap();

        // Stale local state that must NOT survive the restore.
        fs::write(&vault, vault_doc(my_id, "stale-local", false)).unwrap();

        let target_dir = TempDir::new().unwrap();
        let target_ws = target_dir.path().join("workspace");
        fs::create_dir_all(&target_ws).unwrap();
        import(&alf_file, &target_ws, None, RestoreMode::Total).unwrap();

        let doc: alf_core::CredentialsDocument =
            serde_json::from_str(&fs::read_to_string(&vault).unwrap()).unwrap();
        assert_eq!(doc.credentials.len(), 1, "full overwrite, no merge");
        assert_eq!(doc.credentials[0].label.as_deref(), Some("from-archive"));
        assert_eq!(
            doc.extra.get("future_doc_field"),
            Some(&serde_json::json!("kept")),
            "unknown doc-level extra must survive restore"
        );
    }

    const A_ID: &str = "aaaaaaaa-0000-0000-0000-0000000000a1";

    /// Flat ZeroClaw install root (the real/harness layout): config.toml +
    /// files live directly under the root.
    fn create_zeroclaw_home(
        config_toml: &str,
        files: &[(&str, &str)],
    ) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        fs::write(root.join("config.toml"), config_toml).unwrap();
        for (name, content) in files {
            let path = root.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, content).unwrap();
        }
        (dir, root)
    }

    /// Seed a real-schema `brain.db` at `<root>/data/memory/brain.db` with one
    /// agent (`agent_a`) and one row.
    fn create_test_db(root: &Path) {
        let db = crate::brain_db::real_schema_db(
            &root.join("data").join("memory"),
            &[(A_ID, "agent_a")],
        );
        let conn = Connection::open(&db).unwrap();
        conn.execute(
            "INSERT INTO memories (id, key, content, category, embedding, created_at, \
             updated_at, session_id, namespace, importance, superseded_by, agent_id) \
             VALUES (?1,?2,?3,?4,NULL,?5,?5,NULL,'default',0.5,NULL,?6)",
            rusqlite::params![
                "00000000-0000-0000-0000-000000000001",
                "pref_lang",
                "User prefers Rust.",
                "core",
                "2026-01-15T10:00:00Z",
                A_ID
            ],
        )
        .unwrap();
    }

    #[test]
    fn round_trip_sqlite_workspace() {
        isolate_home();
        let config = "[memory]\nbackend = \"sqlite\"\nembedding_provider = \"none\"";
        let (dir, root) = create_zeroclaw_home(
            config,
            &[
                ("SOUL.md", "# ZCBot\n\nA ZeroClaw assistant.\n"),
                ("USER.md", "# Alice\n\n## Timezone\n\nAmerica/New_York\n"),
            ],
        );
        create_test_db(&root);

        // Export
        let alf_file = dir.path().join("export.alf");
        let export_report = export::export(&root, &alf_file).unwrap();
        assert!(export_report.memory_records > 0);

        // Import into a fresh install root (no config.toml yet → files land
        // directly under the given workspace).
        let target_dir = TempDir::new().unwrap();
        let target_ws = target_dir.path().join("install");
        fs::create_dir_all(&target_ws).unwrap();

        let import_report = import(&alf_file, &target_ws, None, RestoreMode::Total).unwrap();
        // Agent name round-trips as the per-agent alias (WP6 unique-name fix),
        // not the shared-install SOUL.md H1 "ZCBot".
        assert_eq!(import_report.agent_name, "agent_a");
        assert!(import_report.identity_imported);
        assert_eq!(import_report.principals_count, 1);

        // Raw sources restored under the install root.
        let soul = fs::read_to_string(target_ws.join("SOUL.md")).unwrap();
        assert!(soul.contains("ZCBot"));
        assert!(target_ws.join("config.toml").is_file());
        // Memory restored into a bootstrapped brain.db.
        let db = target_ws.join("data").join("memory").join("brain.db");
        assert!(
            db.is_file(),
            "brain.db bootstrapped from the captured schema"
        );
        let conn = Connection::open(&db).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE agent_id=?1 AND key='pref_lang'",
                [A_ID],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "the agent slice was restored into brain.db");
    }

    #[test]
    fn import_creates_workspace_dirs() {
        isolate_home();
        let config = "[memory]\nbackend = \"markdown\"";
        let (dir, root) = create_zeroclaw_home(
            config,
            &[
                ("SOUL.md", "# DirTest\n\nTest.\n"),
                ("memory/2026-01-15.md", "## Entry\n\nContent.\n"),
            ],
        );

        let alf_file = dir.path().join("export.alf");
        export::export(&root, &alf_file).unwrap();

        let target = TempDir::new().unwrap();
        let deep = target.path().join("deep/nested/workspace");
        let report = import(&alf_file, &deep, None, RestoreMode::Total).unwrap();
        assert_eq!(report.agent_name, "DirTest");
        assert!(deep.is_dir());
    }

    // -- Zip Slip / path-traversal regression (threat model A4.1/A1.1) ------

    /// Build a valid exported ZeroClaw archive, then append one malicious raw
    /// entry with an attacker-controlled name. Returns the home TempDir (kept
    /// alive so the `.alf` is not deleted) and the path to the archive.
    fn archive_with_malicious_entry(entry_name: &str) -> (TempDir, std::path::PathBuf) {
        use std::io::Write as _;
        let config = "[memory]\nbackend = \"sqlite\"\nembedding_provider = \"none\"";
        let (dir, ws) = create_zeroclaw_home(config, &[("SOUL.md", "# ZCBot\n\nhi\n")]);
        create_test_db(dir.path());
        let alf_file = dir.path().join("export.alf");
        export::export(&ws, &alf_file).unwrap();

        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&alf_file)
            .unwrap();
        let mut zw = zip::ZipWriter::new_append(file).unwrap();
        zw.start_file(entry_name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zw.write_all(b"owned").unwrap();
        zw.finish().unwrap();
        (dir, alf_file)
    }

    #[test]
    fn import_rejects_zip_slip_parent_traversal() {
        isolate_home();
        let (_keep, alf_file) = archive_with_malicious_entry("raw/zeroclaw/../../PWNED.txt");

        let root = TempDir::new().unwrap();
        let workspace = root.path().join("a/ws");
        // `../../` from `<root>/a/ws` lands at `<root>/PWNED.txt`.
        let escaped = root.path().join("PWNED.txt");

        let result = import(&alf_file, &workspace, None, RestoreMode::Total);
        assert!(
            result.is_err(),
            "import must reject a path-traversal archive"
        );
        assert!(
            !escaped.exists(),
            "Zip Slip escaped the workspace: {}",
            escaped.display()
        );
    }

    #[test]
    fn import_rejects_zip_slip_absolute() {
        isolate_home();
        let root = TempDir::new().unwrap();
        // Absolute target kept inside our sandbox, so even a regression cannot
        // touch the real filesystem.
        let escaped = root.path().join("ABS_PWNED.txt");
        let entry = format!("raw/zeroclaw/{}", escaped.display());
        let (_keep, alf_file) = archive_with_malicious_entry(&entry);

        // Workspace nested so its parent (the ZeroClaw `config.toml` target)
        // also stays inside the sandbox.
        let workspace = root.path().join("home/ws");
        let result = import(&alf_file, &workspace, None, RestoreMode::Total);
        assert!(
            result.is_err(),
            "import must reject an absolute-path archive entry"
        );
        assert!(
            !escaped.exists(),
            "absolute-path entry escaped: {}",
            escaped.display()
        );
    }
}
