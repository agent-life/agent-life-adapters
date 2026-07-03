//! Import an `.alf` archive into a Hermes profile (`HERMES_HOME`).
//!
//! Two paths:
//! 1. **Raw source restore** (same-runtime, lossless): extract `raw/hermes/`
//!    verbatim, then **rebuild `state.db`** from session records + the captured
//!    schema sidecar (the binary is never archived — D7).
//! 2. **Cross-runtime migration**: reconstruct `SOUL.md`, `memories/USER.md`,
//!    `memories/MEMORY.md` from structured layers. Sessions become a markdown
//!    transcript (no Hermes `state.db` schema to rebuild against).

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use alf_core::{AlfReader, ArchiveEnumeration, FileEntry, MemoryRecord, VaultKey};

use crate::session_extractor::StateDbSchema;
use crate::session_rebuilder;
use crate::ImportReport;

const RAW_PREFIX: &str = "raw/hermes/";
const SCHEMA_SIDECAR: &str = ".alf-state-db-schema.json";

/// Import an `.alf` archive into a Hermes home.
pub fn import(alf_file: &Path, home: &Path, vault_key: Option<&VaultKey>) -> Result<ImportReport> {
    let file = std::fs::File::open(alf_file)
        .with_context(|| format!("Failed to open ALF file: {}", alf_file.display()))?;
    let mut alf = AlfReader::new(std::io::BufReader::new(file))?;

    let manifest = alf.manifest().clone();
    let agent_name = manifest.agent.name.clone();
    let agent_id = manifest.agent.id;
    let mut warnings = Vec::new();

    fs::create_dir_all(home)?;
    fs::create_dir_all(home.join("memories"))?;

    let file_names = alf.file_names();
    let has_raw = file_names.iter().any(|f| f.starts_with(RAW_PREFIX));

    if has_raw {
        restore_raw_sources(&mut alf, home, &file_names)?;
    } else {
        warnings.push(
            "No raw/hermes/ sources in archive — reconstructing from structured data.".to_string(),
        );
        reconstruct_from_structured(&mut alf, home, &mut warnings)?;
    }

    // Persist agent id for this profile.
    let _ = fs::write(home.join(".alf-agent-id"), agent_id.to_string());

    // Inert-on-restore (D3): external include entries come back unverified, so a
    // hostile/compromised archive's external entries are not packed on the next
    // sync until the local user re-confirms them with `alf add --external`.
    match mark_external_inert(home) {
        Ok(0) => {}
        Ok(n) => warnings.push(format!(
            "{n} external file entry(ies) imported as inert; re-add with \
             `alf add --external` to include them in sync."
        )),
        Err(e) => warnings.push(format!("could not mark external entries inert: {e}")),
    }

    // Read structured layers (for counts + session rebuild).
    let identity = alf.read_identity()?;
    let principals = alf.read_principals()?;
    let credentials = alf.read_credentials()?;
    let all_memory = alf.read_all_memory()?;

    // Rebuild state.db from session records + the captured schema. Only the
    // same-runtime (raw present) path carries the schema sidecar.
    if has_raw {
        match rebuild_sessions(&mut alf, home, &all_memory) {
            Ok(0) => {}
            Ok(n) => warnings.push(format!("Rebuilt state.db with {n} session(s).")),
            Err(e) => warnings.push(format!("state.db rebuild skipped: {e}")),
        }
    } else if all_memory.iter().any(is_session_record) {
        warnings.push(
            "Session records present but no Hermes schema in archive (cross-runtime); \
             sessions left as memory records, state.db not rebuilt."
                .to_string(),
        );
    }

    // Restore skill artifacts (D5) to skills/.
    match restore_skill_artifacts(&mut alf, home) {
        Ok(0) => {}
        Ok(n) => warnings.push(format!("Restored {n} skill file(s) to skills/.")),
        Err(e) => warnings.push(format!("skill artifact restore skipped: {e}")),
    }

    let credentials_count = credentials
        .as_ref()
        .map(|c| c.credentials.len() as u32)
        .unwrap_or(0);

    // Restore credentials: `alf-vault`-tagged records go back to the agent vault
    // as-is (still encrypted); any others need the vault key.
    if let Some(doc) = credentials {
        let doc_extra = doc.extra.clone();
        let (vault_records, other_records): (Vec<_>, Vec<_>) = doc
            .credentials
            .into_iter()
            .partition(|c| c.tags.iter().any(|t| t == "alf-vault"));
        let vaulted = restore_agent_vault(vault_records, doc_extra, agent_id, home)?;
        if vaulted > 0 {
            warnings.push(format!(
                "Restored {vaulted} vaulted account(s) to the agent vault \
                 (inspect with `alf vault list`)."
            ));
        }
        if !other_records.is_empty() {
            warnings.push(format!(
                "{} non-vault credential record(s) found; Hermes secrets restore to \
                 ~/.hermes/.env is manual. Re-add with `hermes` or `alf vault add`.",
                other_records.len()
            ));
            let _ = vault_key; // reserved for future runtime-keystore restore
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

/// Flip restored external include entries to `verified = false` (inert). Returns
/// the count changed.
fn mark_external_inert(home: &Path) -> Result<usize> {
    let path = home.join(alf_core::include::INCLUDE_FILE);
    if !path.is_file() {
        return Ok(0);
    }
    let mut list = alf_core::include::IncludeList::load(home)?;
    let mut changed = 0;
    for e in list.files.iter_mut() {
        if e.external && e.verified {
            e.verified = false;
            changed += 1;
        }
    }
    if changed > 0 {
        list.save(home)?;
    }
    Ok(changed)
}

fn is_session_record(r: &MemoryRecord) -> bool {
    r.namespace.starts_with("session")
        || r.raw_source_format
            .as_ref()
            .map(|v| v.get("session").is_some())
            .unwrap_or(false)
}

/// Rebuild `state.db` from session records + the schema sidecar. Returns the
/// session count written (0 when there is no schema or no session records).
fn rebuild_sessions<R: std::io::Read + std::io::Seek>(
    alf: &mut AlfReader<R>,
    home: &Path,
    all_memory: &[MemoryRecord],
) -> Result<usize> {
    let schema_path = format!("{RAW_PREFIX}{SCHEMA_SIDECAR}");
    if !alf.file_names().iter().any(|f| f == &schema_path) {
        return Ok(0);
    }
    let session_records: Vec<MemoryRecord> = all_memory
        .iter()
        .filter(|r| is_session_record(r))
        .cloned()
        .collect();
    if session_records.is_empty() {
        return Ok(0);
    }
    let schema_bytes = alf.read_raw_entry(&schema_path)?;
    let schema: StateDbSchema =
        serde_json::from_slice(&schema_bytes).context("parsing state.db schema sidecar")?;
    session_rebuilder::rebuild_state_db(&home.join("state.db"), &session_records, &schema)
}

/// Restore Tier-2 skill artifacts (those with an `archive_path`) to `skills/`.
/// Tier-3 (reference-only) entries are skipped — there are no bytes to write.
fn restore_skill_artifacts<R: std::io::Read + std::io::Seek>(
    alf: &mut AlfReader<R>,
    home: &Path,
) -> Result<usize> {
    let Some(index) = alf.read_attachments()? else {
        return Ok(0);
    };
    let mut n = 0;
    for att in &index.attachments {
        let Some(archive_path) = att.archive_path.clone() else {
            continue;
        };
        // Sanitize against Zip Slip (the source_path is attacker-influenceable).
        let target = alf_core::safe_extract_path(home, &att.source_path)
            .with_context(|| format!("refusing to extract artifact {}", att.source_path))?;
        let data = alf.read_raw_entry_capped(&archive_path, alf_core::MAX_RAW_ENTRY_BYTES)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, &data)
            .with_context(|| format!("Failed to write {}", target.display()))?;
        n += 1;
    }
    Ok(n)
}

// ---------------------------------------------------------------------------
// Dry-run archive enumeration
// ---------------------------------------------------------------------------

/// Enumerate the files an [`import`] would write. The schema sidecar is internal
/// and not surfaced as a restored file.
pub fn enumerate_archive(alf_file: &Path) -> Result<ArchiveEnumeration> {
    let file = std::fs::File::open(alf_file)
        .with_context(|| format!("Failed to open ALF file: {}", alf_file.display()))?;
    let mut alf = AlfReader::new(std::io::BufReader::new(file))?;
    let file_names = alf.file_names();
    let mut files = Vec::new();
    let mut warnings = Vec::new();

    let has_raw = file_names.iter().any(|f| f.starts_with(RAW_PREFIX));
    if has_raw {
        let mut raw: Vec<String> = file_names
            .iter()
            .filter(|f| f.starts_with(RAW_PREFIX) && f.len() > RAW_PREFIX.len())
            .cloned()
            .collect();
        raw.sort();
        for name in &raw {
            let rel = &name[RAW_PREFIX.len()..];
            if rel == SCHEMA_SIDECAR {
                continue; // internal — consumed by the state.db rebuild
            }
            files.push(FileEntry {
                path: rel.to_string(),
                size: alf.entry_size(name)?,
            });
        }
        let records = alf
            .manifest()
            .layers
            .memory
            .as_ref()
            .map(|m| m.record_count)
            .unwrap_or(0);
        if records > 0
            && file_names
                .iter()
                .any(|f| f == &format!("{RAW_PREFIX}{SCHEMA_SIDECAR}"))
        {
            files.push(FileEntry {
                path: "state.db (rebuilt from records)".to_string(),
                size: 0,
            });
        }
    } else {
        warnings.push(
            "Archive has no raw/hermes/ sources — restore would reconstruct files from \
             structured data; the list below is approximate and sizes are unknown."
                .to_string(),
        );
        let manifest = alf.manifest();
        if manifest.layers.identity.is_some() {
            files.push(FileEntry {
                path: "SOUL.md".to_string(),
                size: 0,
            });
        }
        if manifest.layers.principals.is_some() {
            files.push(FileEntry {
                path: "memories/USER.md".to_string(),
                size: 0,
            });
        }
        if manifest
            .layers
            .memory
            .as_ref()
            .map(|m| m.record_count)
            .unwrap_or(0)
            > 0
        {
            files.push(FileEntry {
                path: "memories/MEMORY.md".to_string(),
                size: 0,
            });
        }
    }

    Ok(ArchiveEnumeration { files, warnings })
}

// ---------------------------------------------------------------------------
// Path 1: Raw source restore (lossless)
// ---------------------------------------------------------------------------

fn restore_raw_sources<R: std::io::Read + std::io::Seek>(
    alf: &mut AlfReader<R>,
    home: &Path,
    file_names: &[String],
) -> Result<()> {
    let mut total_bytes: u64 = 0;
    for name in file_names {
        if !name.starts_with(RAW_PREFIX) {
            continue;
        }
        let relative = &name[RAW_PREFIX.len()..];
        if relative.is_empty() || relative == SCHEMA_SIDECAR {
            // The schema sidecar is internal — consumed by the rebuild, not
            // written into the user's home.
            continue;
        }
        // Reject path traversal / absolute names (Zip Slip — threat model A4.1).
        let target = alf_core::safe_extract_path(home, relative)
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

/// Full overwrite of the archive agent's own vault
/// (`~/.alf/vault/{agent_id}/credentials.json`, D6), preserving the archive
/// doc's `extra` verbatim (unknown doc-level fields survive every restore).
fn restore_agent_vault(
    records: Vec<alf_core::CredentialRecord>,
    doc_extra: std::collections::HashMap<String, serde_json::Value>,
    agent_id: uuid::Uuid,
    home: &Path,
) -> Result<usize> {
    if records.is_empty() {
        return Ok(0);
    }
    let count = records.len();
    let doc = alf_core::CredentialsDocument {
        credentials: records,
        extra: doc_extra,
    };
    let target = alf_core::home_dir()
        .map(|h| alf_core::agent_vault_path(&h, agent_id))
        .unwrap_or_else(|| home.join(".alf-restored-credentials.json"));
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&target, serde_json::to_string_pretty(&doc)?)
        .with_context(|| format!("Failed to write {}", target.display()))?;
    Ok(count)
}

// ---------------------------------------------------------------------------
// Path 2: Cross-runtime reconstruction
// ---------------------------------------------------------------------------

fn reconstruct_from_structured<R: std::io::Read + std::io::Seek>(
    alf: &mut AlfReader<R>,
    home: &Path,
    warnings: &mut Vec<String>,
) -> Result<()> {
    if let Some(identity) = alf.read_identity()? {
        if let Some(ref prose) = identity.prose {
            if let Some(ref soul) = prose.soul {
                fs::write(home.join("SOUL.md"), soul)?;
            }
        } else if let Some(ref structured) = identity.structured {
            let name = structured
                .names
                .as_ref()
                .map(|n| n.primary.as_str())
                .unwrap_or("Agent");
            fs::write(home.join("SOUL.md"), format!("# {name}\n"))?;
        }
    }

    if let Some(principals) = alf.read_principals()? {
        if let Some(principal) = principals.principals.first() {
            if let Some(ref prose) = principal.profile.prose {
                if let Some(ref user_profile) = prose.user_profile {
                    fs::write(home.join("memories").join("USER.md"), user_profile)?;
                }
            }
        }
    }

    let all_records = alf.read_all_memory()?;
    let mut curated: Vec<String> = Vec::new();
    let mut transcripts: Vec<String> = Vec::new();
    for record in &all_records {
        if is_session_record(record) {
            transcripts.push(record.content.clone());
        } else {
            // Curated and any other non-session memory → MEMORY.md.
            curated.push(record.content.clone());
        }
    }
    if !curated.is_empty() {
        fs::write(
            home.join("memories").join("MEMORY.md"),
            curated.join("\n§\n"),
        )?;
    }
    if !transcripts.is_empty() {
        // No native Hermes schema cross-runtime → preserve as a readable file.
        fs::write(
            home.join("memories").join("imported-sessions.md"),
            transcripts.join("\n\n---\n\n"),
        )?;
        warnings.push(format!(
            "Reconstructed {} session(s) as a markdown transcript (cross-runtime; \
             not loaded into state.db).",
            transcripts.len()
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
    use std::sync::OnceLock;
    use tempfile::TempDir;

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
        let src = make_home(false);
        let my_id = "cfef1150-cccc-4ccc-8ccc-0000000000c1";
        let other_id = "cfef1150-cccc-4ccc-8ccc-0000000000c2";
        fs::write(src.path().join(".alf-agent-id"), my_id).unwrap();

        let home = alf_core::home_dir().unwrap();
        let mine = alf_core::agent_vault_path(&home, my_id.parse().unwrap());
        fs::create_dir_all(mine.parent().unwrap()).unwrap();
        fs::write(&mine, vault_doc(my_id, "mine", false)).unwrap();
        let decoy = alf_core::agent_vault_path(&home, other_id.parse().unwrap());
        fs::create_dir_all(decoy.parent().unwrap()).unwrap();
        fs::write(&decoy, vault_doc(other_id, "decoy", false)).unwrap();

        let alf_file = src.path().join("export.alf");
        export::export(src.path(), &alf_file).unwrap();

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
        let src = make_home(false);
        let my_id = "cfef1150-cccc-4ccc-8ccc-0000000000c3";
        fs::write(src.path().join(".alf-agent-id"), my_id).unwrap();

        let home = alf_core::home_dir().unwrap();
        let vault = alf_core::agent_vault_path(&home, my_id.parse().unwrap());
        fs::create_dir_all(vault.parent().unwrap()).unwrap();
        fs::write(&vault, vault_doc(my_id, "from-archive", true)).unwrap();

        let alf_file = src.path().join("export.alf");
        export::export(src.path(), &alf_file).unwrap();

        // Stale local state that must NOT survive the restore.
        fs::write(&vault, vault_doc(my_id, "stale-local", false)).unwrap();

        let target = TempDir::new().unwrap();
        import(&alf_file, target.path(), None).unwrap();

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

    fn make_home(with_db: bool) -> TempDir {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        fs::write(home.join("SOUL.md"), "# Atlas\n\nSteadfast.\n").unwrap();
        fs::write(
            home.join("config.yaml"),
            "agent:\n  system_prompt: \"Be terse.\"\n",
        )
        .unwrap();
        let mem = home.join("memories");
        fs::create_dir_all(&mem).unwrap();
        fs::write(mem.join("MEMORY.md"), "Fact one.\n§\nAlways fmt.").unwrap();
        fs::write(mem.join("USER.md"), "# Johan\n").unwrap();
        if with_db {
            let db = home.join("state.db");
            let c = Connection::open(&db).unwrap();
            c.execute_batch(
                "CREATE TABLE schema_version (version INTEGER NOT NULL);
                 INSERT INTO schema_version VALUES (16);
                 CREATE TABLE sessions (id TEXT PRIMARY KEY, source TEXT NOT NULL, title TEXT,
                     parent_session_id TEXT, started_at REAL NOT NULL, ended_at REAL);
                 CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
                     role TEXT NOT NULL, content TEXT, tool_name TEXT, timestamp REAL NOT NULL);
                 CREATE VIRTUAL TABLE messages_fts USING fts5(content);
                 CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages BEGIN
                    INSERT INTO messages_fts(rowid, content) VALUES (new.id, COALESCE(new.content,''));
                 END;",
            )
            .unwrap();
            c.execute("INSERT INTO sessions (id,source,title,started_at) VALUES ('20260101_120000_aa','cli','S',1767268800.0)", []).unwrap();
            c.execute("INSERT INTO messages (session_id,role,content,timestamp) VALUES ('20260101_120000_aa','user','search retry markers',1767268800.0)", []).unwrap();
        }
        dir
    }

    #[test]
    fn round_trip_raw_and_state_db() {
        isolate_home();
        let src = make_home(true);
        let alf_file = src.path().join("export.alf");
        export::export(src.path(), &alf_file).unwrap();

        let target = TempDir::new().unwrap();
        let report = import(&alf_file, target.path(), None).unwrap();
        assert_eq!(report.agent_name, "Atlas");
        assert!(report.identity_imported);
        assert_eq!(report.principals_count, 1);

        // Raw files restored.
        assert!(fs::read_to_string(target.path().join("SOUL.md"))
            .unwrap()
            .contains("Atlas"));
        assert!(target.path().join("memories/MEMORY.md").is_file());
        // The internal schema sidecar is NOT written to the user's home.
        assert!(!target.path().join(SCHEMA_SIDECAR).is_file());

        // state.db rebuilt + FTS works.
        let db = target.path().join("state.db");
        assert!(db.is_file());
        let c = Connection::open(&db).unwrap();
        let sessions: i64 = c
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sessions, 1);
        let hits: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'retry'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
    }

    #[test]
    fn round_trip_skills_as_artifacts() {
        isolate_home();
        let src = make_home(false);
        fs::create_dir_all(src.path().join("skills/custom/deploy/scripts")).unwrap();
        fs::write(
            src.path().join("skills/custom/deploy/SKILL.md"),
            "# deploy\nship it",
        )
        .unwrap();
        fs::write(
            src.path().join("skills/custom/deploy/scripts/go.sh"),
            "echo go",
        )
        .unwrap();
        let alf_file = src.path().join("export.alf");
        let report = export::export(src.path(), &alf_file).unwrap();
        assert!(
            report.attachments_count >= 2,
            "skills should be exported as artifacts"
        );

        let target = TempDir::new().unwrap();
        import(&alf_file, target.path(), None).unwrap();
        assert!(target
            .path()
            .join("skills/custom/deploy/SKILL.md")
            .is_file());
        assert!(target
            .path()
            .join("skills/custom/deploy/scripts/go.sh")
            .is_file());
        assert_eq!(
            fs::read_to_string(target.path().join("skills/custom/deploy/SKILL.md")).unwrap(),
            "# deploy\nship it"
        );
    }

    #[test]
    fn external_file_packs_and_restores_inert() {
        isolate_home();
        let src = make_home(false);
        // A project dir OUTSIDE the home with an AGENTS.md.
        let project = TempDir::new().unwrap();
        let agents = project.path().join("AGENTS.md");
        fs::write(&agents, "# Ops\nbe careful").unwrap();

        // Bless the project root, then track the external file (verified).
        alf_core::include::add_allowed_root(project.path()).unwrap();
        let roots = alf_core::include::load_allowed_roots();
        let canon = alf_core::include::validate_external_source(&agents, &roots).unwrap();
        let sanitized = alf_core::include::sanitized_external_name(&canon);
        let mut list = alf_core::include::IncludeList::load(src.path()).unwrap();
        list.add_external(&sanitized, &canon.to_string_lossy());
        list.save(src.path()).unwrap();

        // Export packs it under raw/hermes/external/.
        let alf_file = src.path().join("export.alf");
        let report = export::export(src.path(), &alf_file).unwrap();
        assert!(
            report
                .raw_sources
                .iter()
                .any(|s| s.starts_with("external/")),
            "external file must be packed, got {:?}",
            report.raw_sources
        );

        // Import restores it and marks the entry inert (verified=false).
        let target = TempDir::new().unwrap();
        import(&alf_file, target.path(), None).unwrap();
        let restored = alf_core::include::IncludeList::load(target.path()).unwrap();
        let ext = restored
            .externals()
            .next()
            .expect("external entry restored");
        assert!(!ext.verified, "restored external entry must be inert");
    }

    #[test]
    fn import_creates_home_dirs() {
        isolate_home();
        let src = make_home(false);
        let alf_file = src.path().join("export.alf");
        export::export(src.path(), &alf_file).unwrap();
        let target = TempDir::new().unwrap();
        let deep = target.path().join("deep/nested/.hermes");
        let report = import(&alf_file, &deep, None).unwrap();
        assert_eq!(report.agent_name, "Atlas");
        assert!(deep.is_dir());
    }

    // -- Zip Slip regression (threat model A4.1) ---------------------------

    fn archive_with_malicious_entry(entry_name: &str) -> (TempDir, std::path::PathBuf) {
        use std::io::Write as _;
        let src = make_home(false);
        let alf_file = src.path().join("export.alf");
        export::export(src.path(), &alf_file).unwrap();
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
        (src, alf_file)
    }

    #[test]
    fn import_rejects_zip_slip() {
        isolate_home();
        let (_keep, alf_file) = archive_with_malicious_entry("raw/hermes/../../PWNED.txt");
        let root = TempDir::new().unwrap();
        let home = root.path().join("a/.hermes");
        let escaped = root.path().join("PWNED.txt");
        assert!(import(&alf_file, &home, None).is_err());
        assert!(!escaped.exists(), "Zip Slip escaped: {}", escaped.display());
    }
}
