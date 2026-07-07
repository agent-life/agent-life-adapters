//! Import a generic `.alf` archive back into a workspace.
//!
//! Generic records are *derived state*: the source of truth is the raw tree.
//! Import therefore always prefers `raw/generic/` — a same-runtime restore is a
//! verbatim file write-back, so `restore → re-export` is a zero-delta no-op.
//! There is no structured cross-runtime reconstruction (design non-goal for
//! v1): a generic archive always carries `.alf-map.json` in its raw tree, so the
//! raw path is always taken. `ImportOptions.mode` (Total/Merge) is ignored — a
//! file-based workspace has no per-agent mutable store to reconcile.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use uuid::Uuid;

use alf_core::{AlfReader, CredentialRecord, CredentialsDocument, RestoreMode, VaultKey};

use crate::{ArchiveEnumeration, FileEntry, ImportReport};

const RAW_PREFIX: &str = "raw/generic/";

/// Enumerate the workspace files an [`import`] would write, without touching the
/// filesystem. Backs `alf restore --dry-run` for the generic runtime.
///
/// Generic archives are raw-preferred (import always writes the `raw/generic/`
/// tree verbatim), so the list is exact — paths and sizes come straight from the
/// archive. A generic archive always carries `.alf-map.json` under `raw/generic/`,
/// so `raw/generic/` is never empty in practice; the empty case is reported as a
/// warning rather than a silent no-op.
pub fn enumerate_archive(alf_file: &Path) -> Result<ArchiveEnumeration> {
    let file = fs::File::open(alf_file)
        .with_context(|| format!("Failed to open ALF file: {}", alf_file.display()))?;
    let mut alf = AlfReader::new(std::io::BufReader::new(file))?;

    let mut raw: Vec<String> = alf
        .file_names()
        .iter()
        .filter(|f| f.starts_with(RAW_PREFIX) && f.len() > RAW_PREFIX.len())
        .cloned()
        .collect();
    raw.sort();

    let mut warnings = Vec::new();
    if raw.is_empty() {
        warnings.push(
            "Archive has no raw/generic/ sources — a generic restore would write nothing."
                .to_string(),
        );
    }

    let mut files = Vec::with_capacity(raw.len());
    for name in &raw {
        let size = alf.entry_size(name)?;
        files.push(FileEntry {
            path: name[RAW_PREFIX.len()..].to_string(),
            size,
        });
    }

    Ok(ArchiveEnumeration { files, warnings })
}

/// Import a generic `.alf` archive into `workspace`.
///
/// `mode` (Total/Merge) is accepted for interface parity but has no effect: a
/// generic workspace is a plain file store with no per-agent mutable rows to
/// reconcile — the raw tree is restored wholesale either way (a Merge request is
/// surfaced as a warning so the no-op is not silent).
pub fn import(
    alf_file: &Path,
    workspace: &Path,
    _vault_key: Option<&VaultKey>,
    mode: RestoreMode,
) -> Result<ImportReport> {
    let file = fs::File::open(alf_file)
        .with_context(|| format!("Failed to open ALF file: {}", alf_file.display()))?;
    let mut alf = AlfReader::new(std::io::BufReader::new(file))?;

    let manifest = alf.manifest().clone();
    let agent_name = manifest.agent.name.clone();
    let agent_id = manifest.agent.id;
    let mut warnings = Vec::new();

    // S4: fail closed on agent-id drift BEFORE overlaying anything. A workspace
    // pinned to agent B must never be silently rebound to A by restoring A's raw
    // tree over B's files (mirrors `ensure_workspace_agent_id` / the
    // `import_agent` fail-closed check, which the direct `restore.rs` call path
    // would otherwise skip).
    let id_file = workspace.join(alf_core::AGENT_ID_FILE);
    if let Some(existing) = fs::read_to_string(&id_file)
        .ok()
        .and_then(|s| Uuid::parse_str(s.trim()).ok())
    {
        if existing != agent_id {
            anyhow::bail!(
                "Agent identity drift: {} is pinned to {existing} but the archive \
                 belongs to {agent_id}. Refusing to overlay a different agent's data. \
                 To deliberately re-home this workspace: echo {agent_id} > {}",
                id_file.display(),
                id_file.display()
            );
        }
    }

    if mode == RestoreMode::Merge {
        warnings.push(
            "generic runtime ignores --mode merge (file-based store; the raw tree \
             is restored wholesale)"
                .to_string(),
        );
    }

    fs::create_dir_all(workspace)?;

    let file_names = alf.file_names();
    let raw_prefix = "raw/generic/";
    if file_names.iter().any(|f| f.starts_with(raw_prefix)) {
        restore_raw_sources(&mut alf, workspace, raw_prefix, &file_names)?;
    } else {
        // A generic archive without a raw tree cannot be reconstructed (records
        // are derived state, no structured back-projection in v1). Surface it
        // rather than silently writing an empty workspace.
        warnings.push(
            "archive has no raw/generic/ sources — nothing to restore (generic \
             archives are raw-preferred and always carry their source files)"
                .to_string(),
        );
    }

    // Pin the agent id for future exports.
    fs::write(&id_file, agent_id.to_string())?;

    // Credentials: Layer 4 is the agent's own vault (alf-vault records, still
    // AEAD-encrypted). Restore them verbatim to the per-agent vault path.
    let credentials_count = manifest
        .layers
        .credentials
        .as_ref()
        .map(|c| c.count)
        .unwrap_or(0);
    if credentials_count > 0 {
        if let Some(doc) = alf.read_credentials()? {
            let doc_extra = doc.extra.clone();
            let (vault_records, other): (Vec<_>, Vec<_>) = doc
                .credentials
                .into_iter()
                .partition(|c| c.tags.iter().any(|t| t == "alf-vault"));
            let vaulted = restore_agent_vault(vault_records, doc_extra, agent_id, workspace)?;
            if vaulted > 0 {
                warnings.push(format!(
                    "Restored {vaulted} vaulted account(s) to the agent vault \
                     (inspect with `alf vault list`)."
                ));
            }
            if !other.is_empty() {
                warnings.push(format!(
                    "{} non-vault credential(s) in archive were not restored (generic \
                     runtimes have no native keystore).",
                    other.len()
                ));
            }
        }
    }

    let memory_records = manifest
        .layers
        .memory
        .as_ref()
        .map(|m| m.record_count)
        .unwrap_or(0);

    Ok(ImportReport {
        agent_name,
        memory_records,
        identity_imported: manifest.layers.identity.is_some(),
        principals_count: 0,
        credentials_count,
        warnings,
    })
}

/// Extract `raw/generic/` entries verbatim, with Zip-Slip + zip-bomb guards
/// (threat model A4.1/A1.1).
fn restore_raw_sources<R: std::io::Read + std::io::Seek>(
    alf: &mut AlfReader<R>,
    workspace: &Path,
    prefix: &str,
    file_names: &[String],
) -> Result<()> {
    let mut total_bytes: u64 = 0;
    for name in file_names {
        let Some(relative) = name.strip_prefix(prefix).filter(|r| !r.is_empty()) else {
            continue;
        };
        let target = alf_core::safe_extract_path(workspace, relative)
            .with_context(|| format!("refusing to extract archive entry {name:?}"))?;

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

/// Restore `alf-vault`-tagged records to the archive agent's own ALF vault
/// (`~/.alf/vault/{agent_id}/credentials.json`), full overwrite, records
/// AEAD-encrypted exactly as carried. The read-twin is `export::load_agent_vault`.
fn restore_agent_vault(
    records: Vec<CredentialRecord>,
    doc_extra: std::collections::HashMap<String, serde_json::Value>,
    agent_id: Uuid,
    workspace: &Path,
) -> Result<usize> {
    if records.is_empty() {
        return Ok(0);
    }
    let count = records.len();
    let doc = CredentialsDocument {
        credentials: records,
        extra: doc_extra,
    };
    let target = alf_core::home_dir()
        .map(|h| alf_core::agent_vault_path(&h, agent_id))
        .unwrap_or_else(|| workspace.join(".alf-restored-credentials.json"));
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&target, serde_json::to_string_pretty(&doc)?)
        .with_context(|| format!("Failed to write {}", target.display()))?;
    Ok(count)
}
