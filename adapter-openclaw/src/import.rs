//! Import an `.alf` archive into an OpenClaw workspace.
//!
//! Two paths:
//! 1. **Raw source restore** (preferred): if `raw/openclaw/` entries exist in
//!    the archive, extract them directly. This is the lossless path for
//!    OpenClaw-to-OpenClaw restores.
//! 2. **Cross-runtime migration**: reconstruct workspace files from ALF
//!    structured data (identity prose, principals prose, memory records).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use alf_core::{AlfReader, ArchiveEnumeration, FileEntry, VaultKey};

use crate::ImportReport;

// ---------------------------------------------------------------------------
// Import entry point
// ---------------------------------------------------------------------------

/// Import an `.alf` archive into an OpenClaw workspace.
///
/// Creates the workspace directory if it doesn't exist. Prefers raw source
/// files when available (lossless restore). Falls back to reconstructing
/// workspace files from structured ALF data (cross-runtime migration).
///
/// `vault_key`, when supplied, decrypts `CredentialRecord` payloads and
/// writes a fresh `auth-profiles.json` next to the workspace. When
/// absent, credentials are reported but not restored.
pub fn import(
    alf_file: &Path,
    workspace: &Path,
    vault_key: Option<&VaultKey>,
    preview: bool,
) -> Result<ImportReport> {
    let file = std::fs::File::open(alf_file)
        .with_context(|| format!("Failed to open ALF file: {}", alf_file.display()))?;
    let reader = std::io::BufReader::new(file);
    let mut alf = AlfReader::new(reader)?;

    let manifest = alf.manifest().clone();
    let agent_name = manifest.agent.name.clone();
    let agent_id = manifest.agent.id;

    let mut warnings = Vec::new();

    // Ensure workspace directory exists
    fs::create_dir_all(workspace)?;
    fs::create_dir_all(workspace.join("memory"))?;

    // Check if raw/openclaw/ sources are available
    let file_names = alf.file_names();
    let raw_prefix = "raw/openclaw/";
    let has_raw = file_names.iter().any(|f| f.starts_with(raw_prefix));

    if has_raw {
        // Path 1: Raw source restore (lossless)
        restore_raw_sources(&mut alf, workspace, raw_prefix, &file_names)?;
    } else {
        // Path 2: Cross-runtime migration
        warnings.push(
            "No raw/openclaw/ sources in archive — reconstructing from structured data."
                .to_string(),
        );
        reconstruct_from_structured(&mut alf, workspace, &mut warnings)?;
    }

    // Inert-on-restore (security design §3.5, manual §7): external include
    // entries arriving in an archive come back unverified — a hostile archive
    // must never conscript this machine into packing files outside the
    // workspace. In-workspace entries are untouched.
    match alf_core::include::mark_external_inert(workspace) {
        Ok(0) => {}
        Ok(n) => warnings.push(format!(
            "{n} external file entry(ies) imported as inert; re-add with \
             `alf add --external` to include them in sync."
        )),
        Err(e) => warnings.push(format!("could not mark external entries inert: {e}")),
    }

    // Write the agent ID for future exports
    let id_file = workspace.join(".alf-agent-id");
    fs::write(&id_file, agent_id.to_string())?;

    // Credentials: decrypt and restore when a key is supplied; otherwise
    // emit the legacy "re-authenticate" warning.
    let credentials_count = manifest
        .layers
        .credentials
        .as_ref()
        .map(|c| c.count)
        .unwrap_or(0);
    if credentials_count > 0 {
        if let Some(doc) = alf.read_credentials()? {
            // Records the agent added with `alf vault add` carry the
            // `alf-vault` tag — restore them to the agent vault file as-is
            // (still encrypted, no key needed). The rest came from the
            // runtime's auth profiles and need the key to decrypt.
            let doc_extra = doc.extra.clone();
            let (vault_records, auth_records): (Vec<_>, Vec<_>) = doc
                .credentials
                .into_iter()
                .partition(|c| c.tags.iter().any(|t| t == "alf-vault"));

            let vaulted =
                restore_agent_vault(vault_records, doc_extra, agent_id, workspace, preview)?;
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
                    Some(key) => {
                        let restored =
                            restore_credentials(&auth_doc, key, workspace, &mut warnings)?;
                        warnings.push(format!(
                            "Restored {restored} credential(s) from the vault. \
                             Verify with OpenClaw."
                        ));
                    }
                    None => {
                        warnings.push(format!(
                            "{} credential(s) found in archive (metadata only). \
                             Pass --vault-key-file or set ALF_VAULT_KEY to restore \
                             secret material, or re-authenticate in OpenClaw.",
                            auth_doc.credentials.len()
                        ));
                    }
                }
            }
        }
    }

    let identity_imported = manifest.layers.identity.is_some();
    let principals_count = manifest
        .layers
        .principals
        .as_ref()
        .map(|p| p.count)
        .unwrap_or(0);
    let memory_records = manifest
        .layers
        .memory
        .as_ref()
        .map(|m| m.record_count)
        .unwrap_or(0);

    Ok(ImportReport {
        agent_name,
        memory_records,
        identity_imported,
        principals_count,
        credentials_count,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Dry-run archive enumeration
// ---------------------------------------------------------------------------

/// Enumerate the workspace files an [`import`] would write, without touching
/// the filesystem. Backs `alf restore --dry-run`.
///
/// Mirrors the path decision in [`import`]: when the archive carries
/// `raw/openclaw/` entries the list is exact (paths and sizes from the
/// archive); otherwise restore reconstructs files from structured layers, and
/// the preview is a coarse, `size: 0` approximation with a warning.
pub fn enumerate_archive(alf_file: &Path) -> Result<ArchiveEnumeration> {
    let file = std::fs::File::open(alf_file)
        .with_context(|| format!("Failed to open ALF file: {}", alf_file.display()))?;
    let reader = std::io::BufReader::new(file);
    let mut alf = AlfReader::new(reader)?;

    let file_names = alf.file_names();
    let raw_prefix = "raw/openclaw/";
    let mut files = Vec::new();
    let mut warnings = Vec::new();

    let has_raw = file_names.iter().any(|f| f.starts_with(raw_prefix));
    if has_raw {
        let mut raw: Vec<String> = file_names
            .iter()
            .filter(|f| f.starts_with(raw_prefix) && f.len() > raw_prefix.len())
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
            "Archive has no raw/openclaw/ sources — restore would reconstruct files \
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

// ---------------------------------------------------------------------------
// Credential restore (decrypt + write auth-profiles.json)
// ---------------------------------------------------------------------------

fn restore_credentials(
    doc: &alf_core::CredentialsDocument,
    key: &VaultKey,
    workspace: &Path,
    warnings: &mut Vec<String>,
) -> Result<usize> {
    use serde_json::{Map, Value};

    let mut profiles = Map::new();
    let mut restored = 0;

    for cred in &doc.credentials {
        if cred.encryption.algorithm == "none" || cred.encrypted_payload == "<not-exported>" {
            warnings.push(format!(
                "Skipping legacy metadata-only credential {} (service={}); no ciphertext.",
                cred.id, cred.service
            ));
            continue;
        }

        let plaintext = match alf_core::decrypt_record(cred, key) {
            Ok(p) => p,
            Err(e) => {
                warnings.push(format!(
                    "Failed to decrypt credential {} (service={}): {e}. Skipping.",
                    cred.id, cred.service
                ));
                continue;
            }
        };

        let payload = match alf_core::VaultPayload::from_json_bytes(&plaintext) {
            Ok(p) => p,
            Err(e) => {
                warnings.push(format!(
                    "Credential {} (service={}) had non-envelope plaintext: {e}",
                    cred.id, cred.service
                ));
                continue;
            }
        };

        // Reconstruct the original auth-profiles.json entry when we
        // stashed it during export; otherwise build a minimal one from
        // the payload fields.
        let profile_value = payload
            .extra
            .get("openclaw_profile")
            .cloned()
            .unwrap_or_else(|| {
                let mut m = Map::new();
                m.insert("provider".into(), Value::String(cred.service.clone()));
                if let Some(u) = &payload.username {
                    m.insert("email".into(), Value::String(u.clone()));
                }
                m.insert("secret".into(), Value::String(payload.secret.clone()));
                Value::Object(m)
            });

        let key_name = cred
            .label
            .clone()
            .unwrap_or_else(|| format!("{}:{}", cred.service, cred.id));
        profiles.insert(key_name, profile_value);
        restored += 1;
    }

    if restored == 0 {
        return Ok(0);
    }

    // Write to ~/.openclaw/agents/main/agent/auth-profiles.json if HOME
    // exists; otherwise drop a copy at workspace/.alf-restored-auth-profiles.json
    // so the user can move it manually.
    let openclaw_target = alf_core::home_dir().map(|h| {
        h.join(".openclaw")
            .join("agents")
            .join("main")
            .join("agent")
            .join("auth-profiles.json")
    });

    let serialized = serde_json::to_string_pretty(&Value::Object(profiles))?;

    let target = match openclaw_target {
        Some(p) => p,
        None => workspace.join(".alf-restored-auth-profiles.json"),
    };
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
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
    preview: bool,
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
    // A point-in-time PREVIEW must not touch the live vault: the restore is
    // a full overwrite (D6), so writing the historical document to
    // `~/.alf/vault/{id}/` would drop every credential added since that
    // sequence and reinstate pre-rotation ciphertext — from a command
    // documented as read-only. Keep it inside the preview tree instead.
    let target = if preview {
        workspace.join(".alf-restored-credentials.json")
    } else {
        alf_core::home_dir()
            .map(|h| alf_core::agent_vault_path(&h, agent_id))
            .unwrap_or_else(|| workspace.join(".alf-restored-credentials.json"))
    };

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    // 0600 + atomic: the document carries ciphertext AND plaintext
    // descriptors (service, username, label), and a crash must never
    // truncate a just-restored vault.
    alf_core::fs_atomic::write_private_atomic(
        &target,
        serde_json::to_string_pretty(&doc)?.as_bytes(),
    )
    .with_context(|| format!("Failed to write {}", target.display()))?;

    Ok(count)
}

// ---------------------------------------------------------------------------
// Path 1: Raw source restore
// ---------------------------------------------------------------------------

/// Extract raw/openclaw/ files directly into the workspace.
fn restore_raw_sources<R: std::io::Read + std::io::Seek>(
    alf: &mut AlfReader<R>,
    workspace: &Path,
    prefix: &str,
    file_names: &[String],
) -> Result<()> {
    let mut total_bytes: u64 = 0;
    for name in file_names {
        if !name.starts_with(prefix) {
            continue;
        }
        let relative = &name[prefix.len()..];
        if relative.is_empty() {
            continue;
        }

        // Reject path-traversal / absolute entry names before any write —
        // a hostile or compromised-server archive must not escape the
        // workspace (Zip Slip; see threat model A4.1/A1.1).
        let target = alf_core::safe_extract_path(workspace, relative)
            .with_context(|| format!("refusing to extract archive entry {name:?}"))?;

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

/// Reconstruct OpenClaw workspace files from structured ALF data.
fn reconstruct_from_structured<R: std::io::Read + std::io::Seek>(
    alf: &mut AlfReader<R>,
    workspace: &Path,
    warnings: &mut Vec<String>,
) -> Result<()> {
    // Identity → SOUL.md
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
            // Synthesize a minimal SOUL.md from structured data
            let name = structured
                .names
                .as_ref()
                .map(|n| n.primary.as_str())
                .unwrap_or("Agent");
            let role = structured.role.as_deref().unwrap_or("AI Assistant");
            let soul = format!("# {name}\n\n{role}\n");
            fs::write(workspace.join("SOUL.md"), soul)?;
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
                // Synthesize minimal USER.md
                let name = structured.name.as_deref().unwrap_or("User");
                let mut content = format!("# {name}\n");
                if let Some(ref tz) = structured.timezone {
                    content.push_str(&format!("\n## Timezone\n\n{tz}\n"));
                }
                fs::write(workspace.join("USER.md"), content)?;
            }
        }
    }

    // Memory records → MEMORY.md + memory/YYYY-MM-DD.md
    let all_records = alf.read_all_memory()?;
    if all_records.is_empty() {
        return Ok(());
    }

    // Separate by namespace
    let mut curated_sections: Vec<String> = Vec::new();
    let mut daily_groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut other_files: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for record in &all_records {
        // Skip tombstones/replaced records: a Superseded record's replacement
        // is already in this set, and a Deleted record is gone — materializing
        // either would resurrect content the agent removed (WP4.1 §8.1). This
        // only affects the structured fallback; a same-runtime restore prefers
        // the verbatim raw tree and never reaches here.
        if !record.status.is_materialized() {
            continue;
        }
        let origin_file = record.source.origin_file.as_deref().unwrap_or("");

        match record.namespace.as_str() {
            "curated" => {
                curated_sections.push(record.content.clone());
            }
            "daily" => {
                // Group by origin file or by observed_at date
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
            _ => {
                // Use origin_file if available, otherwise namespace-based path
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

    // Write MEMORY.md
    if !curated_sections.is_empty() {
        let content = curated_sections.join("\n\n");
        fs::write(workspace.join("MEMORY.md"), content)?;
    }

    // Write daily log files
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
            "Reconstructed {} memory record(s) from structured data.",
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
    use std::fs;
    use std::sync::OnceLock;
    use tempfile::TempDir;

    fn create_workspace(files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().unwrap();
        for (name, content) in files {
            let path = dir.path().join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, content).unwrap();
        }
        dir
    }

    /// Point `HOME` at a clean temp dir for the whole test process, set once
    /// before any vault access. `import()` writes credentials to `$HOME/.alf/vault`
    /// and auth profiles to `$HOME/.openclaw/...`; without this, these tests would
    /// rewrite the developer's real vault. Call at the start of any import test.
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
        let ws = create_workspace(&[
            ("SOUL.md", "# Bot\n\nhi"),
            ("IDENTITY.md", "# Identity\n\nName: Bot"),
        ]);
        let my_id = "cfef1150-aaaa-4aaa-8aaa-0000000000a1";
        let other_id = "cfef1150-aaaa-4aaa-8aaa-0000000000a2";
        fs::write(ws.path().join(".alf-agent-id"), my_id).unwrap();

        let home = alf_core::home_dir().unwrap();
        let mine = alf_core::agent_vault_path(&home, my_id.parse().unwrap());
        fs::create_dir_all(mine.parent().unwrap()).unwrap();
        fs::write(&mine, vault_doc(my_id, "mine", false)).unwrap();
        let decoy = alf_core::agent_vault_path(&home, other_id.parse().unwrap());
        fs::create_dir_all(decoy.parent().unwrap()).unwrap();
        fs::write(&decoy, vault_doc(other_id, "decoy", false)).unwrap();

        let alf_file = ws.path().join("export.alf");
        export::export(ws.path(), &alf_file).unwrap();

        let mut reader = alf_core::AlfReader::new(fs::File::open(&alf_file).unwrap()).unwrap();
        let doc = reader.read_credentials().unwrap().expect("Layer 4 present");
        assert_eq!(doc.credentials.len(), 1);
        assert_eq!(doc.credentials[0].label.as_deref(), Some("mine"));
    }

    /// MIN-12 regression: a PREVIEW import must not touch the live vault.
    /// Restore is a full overwrite (D6), so writing a historical Layer 4 to
    /// `~/.alf/vault/{id}/` would silently drop every credential added since
    /// that sequence — from `alf restore --at-sequence N`, which is documented
    /// as a read-only preview. The restored document belongs in the preview
    /// tree instead.
    #[test]
    fn preview_import_never_writes_the_live_vault() {
        isolate_home();
        let ws = create_workspace(&[
            ("SOUL.md", "# Bot\n\nhi"),
            ("IDENTITY.md", "# Identity\n\nName: Bot"),
        ]);
        let my_id = "cfef1150-aaaa-4aaa-8aaa-0000000000a7";
        fs::write(ws.path().join(".alf-agent-id"), my_id).unwrap();

        let home = alf_core::home_dir().unwrap();
        let vault = alf_core::agent_vault_path(&home, my_id.parse().unwrap());
        fs::create_dir_all(vault.parent().unwrap()).unwrap();
        // The archive carries the OLD credential…
        fs::write(&vault, vault_doc(my_id, "historical", true)).unwrap();
        let alf_file = ws.path().join("export.alf");
        export::export(ws.path(), &alf_file).unwrap();
        // …while the live vault has moved on (a credential added since, or a
        // rotation). This is what a preview must not destroy.
        fs::write(&vault, vault_doc(my_id, "current-live", false)).unwrap();

        let preview = TempDir::new().unwrap();
        import(&alf_file, preview.path(), None, /* preview: */ true).unwrap();

        let live: alf_core::CredentialsDocument =
            serde_json::from_str(&fs::read_to_string(&vault).unwrap()).unwrap();
        assert_eq!(
            live.credentials[0].label.as_deref(),
            Some("current-live"),
            "a preview must leave the live vault exactly as it was"
        );
        // The historical document is materialized inside the preview instead.
        let sandboxed = preview.path().join(".alf-restored-credentials.json");
        assert!(sandboxed.is_file(), "preview vault copy missing");
        let copy: alf_core::CredentialsDocument =
            serde_json::from_str(&fs::read_to_string(&sandboxed).unwrap()).unwrap();
        assert_eq!(copy.credentials[0].label.as_deref(), Some("historical"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&sandboxed).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "restored vault must be owner-only (MIN-14)");
        }
    }

    /// A-2 (WP1): import restores alf-vault records to the archive agent's
    /// own per-agent vault path, fully overwriting a stale local vault (D6)
    /// while preserving the archive doc's unknown extra fields.
    #[test]
    fn import_restores_vault_to_per_agent_path_full_overwrite_preserving_extra() {
        isolate_home();
        let ws = create_workspace(&[
            ("SOUL.md", "# Bot\n\nhi"),
            ("IDENTITY.md", "# Identity\n\nName: Bot"),
        ]);
        let my_id = "cfef1150-aaaa-4aaa-8aaa-0000000000a3";
        fs::write(ws.path().join(".alf-agent-id"), my_id).unwrap();

        let home = alf_core::home_dir().unwrap();
        let vault = alf_core::agent_vault_path(&home, my_id.parse().unwrap());
        fs::create_dir_all(vault.parent().unwrap()).unwrap();
        fs::write(&vault, vault_doc(my_id, "from-archive", true)).unwrap();

        let alf_file = ws.path().join("export.alf");
        export::export(ws.path(), &alf_file).unwrap();

        // Stale local state that must NOT survive the restore.
        fs::write(&vault, vault_doc(my_id, "stale-local", false)).unwrap();

        let target = TempDir::new().unwrap();
        import(&alf_file, target.path(), None, false).unwrap();

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

    #[test]
    fn round_trip_with_raw_sources() {
        isolate_home();
        // Create a workspace, export, then import into a fresh directory
        let ws = create_workspace(&[
            ("SOUL.md", "# Clawd\n\nA helpful lobster."),
            ("IDENTITY.md", "# Identity\n\nName: Clawd"),
            ("USER.md", "# Alice\n\n## Timezone\n\nAmerica/New_York\n"),
            ("MEMORY.md", "## Preferences\n\nLikes Rust."),
            ("memory/2026-01-15.md", "## Morning\n\nBuilt the adapter."),
        ]);
        let alf_file = ws.path().join("export.alf");

        // Export
        let report = export::export(ws.path(), &alf_file).unwrap();
        assert!(report.memory_records > 0);

        // Import into fresh workspace
        let target = TempDir::new().unwrap();
        let import_report = import(&alf_file, target.path(), None, false).unwrap();

        assert_eq!(import_report.agent_name, "Clawd");
        assert!(import_report.identity_imported);
        assert_eq!(import_report.principals_count, 1);
        assert_eq!(import_report.memory_records, report.memory_records);

        // Raw source files should be restored
        let soul = fs::read_to_string(target.path().join("SOUL.md")).unwrap();
        assert!(soul.contains("Clawd"));
        assert!(soul.contains("helpful lobster"));

        let user = fs::read_to_string(target.path().join("USER.md")).unwrap();
        assert!(user.contains("Alice"));

        let memory = fs::read_to_string(target.path().join("MEMORY.md")).unwrap();
        assert!(memory.contains("Likes Rust"));

        let daily = fs::read_to_string(target.path().join("memory/2026-01-15.md")).unwrap();
        assert!(daily.contains("adapter"));

        // Agent ID file should exist
        assert!(target.path().join(".alf-agent-id").is_file());
    }

    #[test]
    fn import_creates_workspace_dirs() {
        isolate_home();
        let ws = create_workspace(&[
            ("SOUL.md", "# Bot\n\nTest."),
            ("IDENTITY.md", "# Identity\n\nName: Bot"),
        ]);
        let alf_file = ws.path().join("export.alf");
        export::export(ws.path(), &alf_file).unwrap();

        let target = TempDir::new().unwrap();
        let deep_path = target.path().join("deep/nested/workspace");
        let report = import(&alf_file, &deep_path, None, false).unwrap();
        assert_eq!(report.agent_name, "Bot");
        assert!(deep_path.is_dir());
    }

    // -- Zip Slip / path-traversal regression (threat model A4.1/A1.1) ------

    /// Build a valid exported archive, then append one malicious raw entry
    /// with an attacker-controlled name. Returns the workspace TempDir (kept
    /// alive so the `.alf` is not deleted) and the path to the archive.
    fn archive_with_malicious_entry(entry_name: &str) -> (TempDir, std::path::PathBuf) {
        use std::io::Write as _;
        let ws = create_workspace(&[
            ("SOUL.md", "# Bot\n\nhi"),
            ("IDENTITY.md", "# Identity\n\nName: Bot"),
        ]);
        let alf_file = ws.path().join("export.alf");
        export::export(ws.path(), &alf_file).unwrap();

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
        (ws, alf_file)
    }

    #[test]
    fn import_rejects_zip_slip_parent_traversal() {
        isolate_home();
        let (_keep, alf_file) = archive_with_malicious_entry("raw/openclaw/../../PWNED.txt");

        let root = TempDir::new().unwrap();
        let workspace = root.path().join("a/ws");
        // `../../` from `<root>/a/ws` lands at `<root>/PWNED.txt`.
        let escaped = root.path().join("PWNED.txt");

        let result = import(&alf_file, &workspace, None, false);
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
        let entry = format!("raw/openclaw/{}", escaped.display());
        let (_keep, alf_file) = archive_with_malicious_entry(&entry);

        let workspace = root.path().join("ws");
        let result = import(&alf_file, &workspace, None, false);
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
