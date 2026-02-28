//! ZIP archive reader and writer for `.alf` and `.alf-delta` files.
//!
//! An `.alf` file is a ZIP archive with a defined internal layout:
//!
//! ```text
//! manifest.json                  # Always present, written last
//! identity.json                  # Optional (Layer 2)
//! principals.json                # Optional (Layer 3)
//! credentials.json               # Optional (Layer 4)
//! attachments.json               # Optional (artifact index)
//! memory/2025-Q4.jsonl           # Memory partitions (JSONL)
//! memory/2026-Q1.jsonl
//! artifacts/shares_tracker.csv   # Tier 2 artifacts
//! raw/openclaw/SOUL.md           # Raw source files
//! ```
//!
//! An `.alf-delta` file has the same structure but uses `delta-manifest.json`
//! and only includes changed layers.

use crate::credentials::CredentialsDocument;
use crate::identity::Identity;
use crate::manifest::*;
use crate::memory::MemoryRecord;
use crate::partition::{PartitionReader, PartitionWriter};
use crate::principals::PrincipalsDocument;

use std::collections::HashMap;
use std::io::{BufReader, Cursor, Read, Seek, Write};
use thiserror::Error;
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Partition error: {0}")]
    Partition(#[from] crate::partition::PartitionError),

    #[error("Missing required entry: {0}")]
    MissingEntry(String),

    #[error("Invalid archive: {0}")]
    Invalid(String),
}

// ===========================================================================
// Snapshot Writer
// ===========================================================================

/// Builds a valid `.alf` ZIP archive.
///
/// Layers are written to the ZIP as they are added. The manifest is computed
/// and written last when [`finish()`](AlfWriter::finish) is called.
///
/// # Example
///
/// ```ignore
/// let mut buf = Cursor::new(Vec::new());
/// let mut writer = AlfWriter::new(&mut buf, base_manifest)?;
/// writer.set_identity(&identity)?;
/// writer.add_memory_partition("memory/2026-Q1.jsonl", &records)?;
/// writer.finish()?;
/// ```
pub struct AlfWriter<W: Write + Seek> {
    zip: ZipWriter<W>,
    manifest: Manifest,
    // Tracking for manifest computation
    identity_info: Option<IdentityLayerInfo>,
    principals_info: Option<PrincipalsLayerInfo>,
    credentials_info: Option<CredentialsLayerInfo>,
    attachments_info: Option<AttachmentsLayerInfo>,
    memory_partitions: Vec<MemoryPartitionInfo>,
    total_memory_records: u64,
    has_embeddings: bool,
    has_raw_source: bool,
}

impl<W: Write + Seek> AlfWriter<W> {
    /// Create a new archive writer.
    ///
    /// The `manifest` provides the base metadata (alf_version, agent, etc.).
    /// Layer inventory fields (`manifest.layers`) will be overwritten by the
    /// writer based on what is actually added.
    pub fn new(writer: W, manifest: Manifest) -> Result<Self, ArchiveError> {
        Ok(Self {
            zip: ZipWriter::new(writer),
            manifest,
            identity_info: None,
            principals_info: None,
            credentials_info: None,
            attachments_info: None,
            memory_partitions: Vec::new(),
            total_memory_records: 0,
            has_embeddings: false,
            has_raw_source: false,
        })
    }

    /// Write the identity layer.
    pub fn set_identity(&mut self, identity: &Identity) -> Result<(), ArchiveError> {
        let path = "identity.json";
        let json = serde_json::to_vec_pretty(identity)?;
        self.write_entry(path, &json)?;
        self.identity_info = Some(IdentityLayerInfo {
            version: identity.version,
            file: path.into(),
            extra: HashMap::new(),
        });
        Ok(())
    }

    /// Write the principals layer.
    pub fn set_principals(&mut self, doc: &PrincipalsDocument) -> Result<(), ArchiveError> {
        let path = "principals.json";
        let json = serde_json::to_vec_pretty(doc)?;
        self.write_entry(path, &json)?;
        self.principals_info = Some(PrincipalsLayerInfo {
            count: doc.principals.len() as u32,
            file: path.into(),
            extra: HashMap::new(),
        });
        Ok(())
    }

    /// Write the credentials layer.
    pub fn set_credentials(&mut self, doc: &CredentialsDocument) -> Result<(), ArchiveError> {
        let path = "credentials.json";
        let json = serde_json::to_vec_pretty(doc)?;
        self.write_entry(path, &json)?;
        self.credentials_info = Some(CredentialsLayerInfo {
            count: doc.credentials.len() as u32,
            file: path.into(),
            extra: HashMap::new(),
        });
        Ok(())
    }

    /// Write a memory partition as JSONL.
    ///
    /// `partition_file` should be in the form `memory/2026-Q1.jsonl`.
    /// The partition info (from/to/sealed) is provided by the caller since
    /// only the caller knows the time boundaries.
    pub fn add_memory_partition(
        &mut self,
        info: MemoryPartitionInfo,
        records: &[MemoryRecord],
    ) -> Result<(), ArchiveError> {
        // Write JSONL to a buffer, then to the ZIP
        let mut buf = Vec::new();
        {
            let mut pw = PartitionWriter::new(&mut buf);
            for record in records {
                pw.write_record(record)?;
                if !record.embeddings.is_empty() {
                    self.has_embeddings = true;
                }
            }
            pw.flush()?;
        }
        self.write_entry(&info.file, &buf)?;
        self.total_memory_records += records.len() as u64;
        self.memory_partitions.push(info);
        Ok(())
    }

    /// Write the attachments index.
    pub fn set_attachments(&mut self, index: &AttachmentsIndex) -> Result<(), ArchiveError> {
        let path = "attachments.json";
        let json = serde_json::to_vec_pretty(index)?;
        self.write_entry(path, &json)?;

        let mut included_count: u32 = 0;
        let mut included_size: u64 = 0;
        let mut referenced_count: u32 = 0;
        let mut referenced_size: u64 = 0;

        for att in &index.attachments {
            if att.archive_path.is_some() {
                included_count += 1;
                included_size += att.size_bytes;
            } else {
                referenced_count += 1;
                referenced_size += att.size_bytes;
            }
        }

        self.attachments_info = Some(AttachmentsLayerInfo {
            count: index.attachments.len() as u32,
            file: path.into(),
            included_count: Some(included_count),
            included_size_bytes: Some(included_size),
            referenced_count: Some(referenced_count),
            referenced_size_bytes: Some(referenced_size),
            extra: HashMap::new(),
        });
        Ok(())
    }

    /// Write an artifact file (Tier 2) into the archive.
    ///
    /// `archive_path` should match the `archive_path` in the attachment
    /// reference (e.g., `"artifacts/shares_tracker.csv"`).
    pub fn add_artifact(&mut self, archive_path: &str, data: &[u8]) -> Result<(), ArchiveError> {
        self.write_entry(archive_path, data)
    }

    /// Write a raw source file into the archive.
    ///
    /// `runtime` is the runtime identifier (e.g., `"openclaw"`).
    /// `relative_path` is the path relative to `raw/{runtime}/`.
    pub fn add_raw_source(
        &mut self,
        runtime: &str,
        relative_path: &str,
        data: &[u8],
    ) -> Result<(), ArchiveError> {
        let path = format!("raw/{runtime}/{relative_path}");
        self.write_entry(&path, data)?;
        self.has_raw_source = true;
        Ok(())
    }

    /// Finalize the archive: compute and write the manifest, close the ZIP.
    ///
    /// Returns the underlying writer.
    pub fn finish(mut self) -> Result<W, ArchiveError> {
        // Build computed layer inventory
        let memory = if !self.memory_partitions.is_empty() || self.total_memory_records > 0 {
            Some(MemoryInventory {
                record_count: self.total_memory_records,
                index_file: "memory/index.json".into(),
                partitions: std::mem::take(&mut self.memory_partitions),
                has_embeddings: Some(self.has_embeddings),
                has_raw_source: Some(self.has_raw_source),
                extra: HashMap::new(),
            })
        } else {
            None
        };

        self.manifest.layers = LayerInventory {
            identity: self.identity_info.take(),
            principals: self.principals_info.take(),
            credentials: self.credentials_info.take(),
            memory,
            attachments: self.attachments_info.take(),
            extra: HashMap::new(),
        };

        // Write manifest last
        let manifest_json = serde_json::to_vec_pretty(&self.manifest)?;
        self.write_entry("manifest.json", &manifest_json)?;

        let writer = self.zip.finish()?;
        Ok(writer)
    }

    fn write_entry(&mut self, path: &str, data: &[u8]) -> Result<(), ArchiveError> {
        let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        self.zip.start_file(path, options)?;
        self.zip.write_all(data)?;
        Ok(())
    }
}

// ===========================================================================
// Snapshot Reader
// ===========================================================================

/// Reads a `.alf` ZIP archive with typed access to each layer.
///
/// The manifest is parsed on construction. Individual layers are read
/// on demand.
pub struct AlfReader<R: Read + Seek> {
    archive: ZipArchive<R>,
    manifest: Manifest,
}

impl<R: Read + Seek> AlfReader<R> {
    /// Open an `.alf` archive and parse the manifest.
    pub fn new(reader: R) -> Result<Self, ArchiveError> {
        let mut archive = ZipArchive::new(reader)?;
        let manifest = Self::read_json_entry(&mut archive, "manifest.json")?;
        Ok(Self { archive, manifest })
    }

    /// The parsed manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Read the identity layer, if present.
    pub fn read_identity(&mut self) -> Result<Option<Identity>, ArchiveError> {
        match &self.manifest.layers.identity {
            Some(info) => Ok(Some(Self::read_json_entry(&mut self.archive, &info.file)?)),
            None => Ok(None),
        }
    }

    /// Read the principals layer, if present.
    pub fn read_principals(&mut self) -> Result<Option<PrincipalsDocument>, ArchiveError> {
        match &self.manifest.layers.principals {
            Some(info) => Ok(Some(Self::read_json_entry(&mut self.archive, &info.file)?)),
            None => Ok(None),
        }
    }

    /// Read the credentials layer, if present.
    pub fn read_credentials(&mut self) -> Result<Option<CredentialsDocument>, ArchiveError> {
        match &self.manifest.layers.credentials {
            Some(info) => Ok(Some(Self::read_json_entry(&mut self.archive, &info.file)?)),
            None => Ok(None),
        }
    }

    /// Read a single memory partition, returning all records.
    pub fn read_memory_partition(
        &mut self,
        file: &str,
    ) -> Result<Vec<MemoryRecord>, ArchiveError> {
        let bytes = self.read_raw_entry(file)?;
        let buf_reader = BufReader::new(Cursor::new(bytes));
        let mut pr = PartitionReader::new(buf_reader);
        Ok(pr.read_all()?)
    }

    /// Read all memory records across all partitions.
    ///
    /// Partitions are read in the order listed in the manifest.
    pub fn read_all_memory(&mut self) -> Result<Vec<MemoryRecord>, ArchiveError> {
        let partition_files: Vec<String> = match &self.manifest.layers.memory {
            Some(mem) => mem.partitions.iter().map(|p| p.file.clone()).collect(),
            None => return Ok(Vec::new()),
        };

        let mut all_records = Vec::new();
        for file in &partition_files {
            let records = self.read_memory_partition(file)?;
            all_records.extend(records);
        }
        Ok(all_records)
    }

    /// Read the attachments index, if present.
    pub fn read_attachments(&mut self) -> Result<Option<AttachmentsIndex>, ArchiveError> {
        match &self.manifest.layers.attachments {
            Some(info) => Ok(Some(Self::read_json_entry(&mut self.archive, &info.file)?)),
            None => Ok(None),
        }
    }

    /// Read a raw file entry from the archive.
    pub fn read_raw_entry(&mut self, path: &str) -> Result<Vec<u8>, ArchiveError> {
        let mut entry = self
            .archive
            .by_name(path)
            .map_err(|_| ArchiveError::MissingEntry(path.into()))?;
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        Ok(buf)
    }

    /// List all file paths in the archive.
    pub fn file_names(&self) -> Vec<String> {
        (0..self.archive.len())
            .filter_map(|i| self.archive.name_for_index(i).map(|n| n.to_string()))
            .collect()
    }

    fn read_json_entry<T: serde::de::DeserializeOwned>(
        archive: &mut ZipArchive<R>,
        path: &str,
    ) -> Result<T, ArchiveError> {
        let entry = archive
            .by_name(path)
            .map_err(|_| ArchiveError::MissingEntry(path.into()))?;
        let value: T = serde_json::from_reader(entry)?;
        Ok(value)
    }
}

// ===========================================================================
// Delta Writer
// ===========================================================================

/// Builds a valid `.alf-delta` ZIP archive.
///
/// Only changed layers are included. The delta manifest is computed and
/// written last when [`finish()`](DeltaWriter::finish) is called.
pub struct DeltaWriter<W: Write + Seek> {
    zip: ZipWriter<W>,
    manifest: DeltaManifest,
    identity_change: Option<IdentityChange>,
    principals_change: Option<PrincipalsChange>,
    credentials_change: Option<CredentialsChange>,
    memory_change: Option<MemoryChange>,
}

impl<W: Write + Seek> DeltaWriter<W> {
    /// Create a new delta writer.
    ///
    /// The `manifest` provides base metadata. The `changes` field will be
    /// overwritten based on what is actually added.
    pub fn new(writer: W, manifest: DeltaManifest) -> Result<Self, ArchiveError> {
        Ok(Self {
            zip: ZipWriter::new(writer),
            manifest,
            identity_change: None,
            principals_change: None,
            credentials_change: None,
            memory_change: None,
        })
    }

    /// Write a changed identity layer.
    pub fn set_identity(
        &mut self,
        identity: &Identity,
        new_version: u32,
    ) -> Result<(), ArchiveError> {
        let path = "identity.json";
        let json = serde_json::to_vec_pretty(identity)?;
        self.write_entry(path, &json)?;
        self.identity_change = Some(IdentityChange {
            file: Some(path.into()),
            new_version: Some(new_version),
            extra: HashMap::new(),
        });
        Ok(())
    }

    /// Write changed principals.
    pub fn set_principals(
        &mut self,
        doc: &PrincipalsDocument,
        changed_ids: Vec<uuid::Uuid>,
    ) -> Result<(), ArchiveError> {
        let path = "principals.json";
        let json = serde_json::to_vec_pretty(doc)?;
        self.write_entry(path, &json)?;
        self.principals_change = Some(PrincipalsChange {
            file: Some(path.into()),
            changed_ids,
            extra: HashMap::new(),
        });
        Ok(())
    }

    /// Write changed credentials.
    pub fn set_credentials(&mut self, doc: &CredentialsDocument) -> Result<(), ArchiveError> {
        let path = "credentials.json";
        let json = serde_json::to_vec_pretty(doc)?;
        self.write_entry(path, &json)?;
        self.credentials_change = Some(CredentialsChange {
            file: Some(path.into()),
            extra: HashMap::new(),
        });
        Ok(())
    }

    /// Write memory delta records as JSONL.
    ///
    /// Records should include the `operation` field indicating the change
    /// type (create/update/delete). Since `MemoryRecord` doesn't have an
    /// `operation` field, the caller wraps records using
    /// [`DeltaMemoryEntry`].
    pub fn add_memory_deltas(
        &mut self,
        entries: &[DeltaMemoryEntry],
    ) -> Result<(), ArchiveError> {
        let path = "memory/delta.jsonl";
        let mut buf = Vec::new();
        for entry in entries {
            serde_json::to_writer(&mut buf, entry)?;
            buf.push(b'\n');
        }
        self.write_entry(path, &buf)?;
        self.memory_change = Some(MemoryChange {
            file: Some(path.into()),
            record_count: Some(entries.len() as u64),
            extra: HashMap::new(),
        });
        Ok(())
    }

    /// Finalize the delta archive.
    pub fn finish(mut self) -> Result<W, ArchiveError> {
        self.manifest.changes = ChangeInventory {
            identity: self.identity_change.take(),
            principals: self.principals_change.take(),
            credentials: self.credentials_change.take(),
            memory: self.memory_change.take(),
            extra: HashMap::new(),
        };

        let manifest_json = serde_json::to_vec_pretty(&self.manifest)?;
        self.write_entry("delta-manifest.json", &manifest_json)?;

        let writer = self.zip.finish()?;
        Ok(writer)
    }

    fn write_entry(&mut self, path: &str, data: &[u8]) -> Result<(), ArchiveError> {
        let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        self.zip.start_file(path, options)?;
        self.zip.write_all(data)?;
        Ok(())
    }
}

// ===========================================================================
// Delta Reader
// ===========================================================================

/// Reads a `.alf-delta` ZIP archive with typed access to changed layers.
pub struct DeltaReader<R: Read + Seek> {
    archive: ZipArchive<R>,
    manifest: DeltaManifest,
}

impl<R: Read + Seek> DeltaReader<R> {
    /// Open an `.alf-delta` archive and parse the delta manifest.
    pub fn new(reader: R) -> Result<Self, ArchiveError> {
        let mut archive = ZipArchive::new(reader)?;
        let manifest: DeltaManifest = {
            let entry = archive
                .by_name("delta-manifest.json")
                .map_err(|_| ArchiveError::MissingEntry("delta-manifest.json".into()))?;
            serde_json::from_reader(entry)?
        };
        Ok(Self { archive, manifest })
    }

    /// The parsed delta manifest.
    pub fn manifest(&self) -> &DeltaManifest {
        &self.manifest
    }

    /// Read the changed identity, if this delta includes identity changes.
    pub fn read_identity(&mut self) -> Result<Option<Identity>, ArchiveError> {
        match &self.manifest.changes.identity {
            Some(change) => match &change.file {
                Some(file) => {
                    let entry = self
                        .archive
                        .by_name(file)
                        .map_err(|_| ArchiveError::MissingEntry(file.clone()))?;
                    Ok(Some(serde_json::from_reader(entry)?))
                }
                None => Ok(None),
            },
            None => Ok(None),
        }
    }

    /// Read changed principals, if present.
    pub fn read_principals(&mut self) -> Result<Option<PrincipalsDocument>, ArchiveError> {
        match &self.manifest.changes.principals {
            Some(change) => match &change.file {
                Some(file) => {
                    let entry = self
                        .archive
                        .by_name(file)
                        .map_err(|_| ArchiveError::MissingEntry(file.clone()))?;
                    Ok(Some(serde_json::from_reader(entry)?))
                }
                None => Ok(None),
            },
            None => Ok(None),
        }
    }

    /// Read changed credentials, if present.
    pub fn read_credentials(&mut self) -> Result<Option<CredentialsDocument>, ArchiveError> {
        match &self.manifest.changes.credentials {
            Some(change) => match &change.file {
                Some(file) => {
                    let entry = self
                        .archive
                        .by_name(file)
                        .map_err(|_| ArchiveError::MissingEntry(file.clone()))?;
                    Ok(Some(serde_json::from_reader(entry)?))
                }
                None => Ok(None),
            },
            None => Ok(None),
        }
    }

    /// Read memory delta entries, if present.
    pub fn read_memory_deltas(&mut self) -> Result<Option<Vec<DeltaMemoryEntry>>, ArchiveError> {
        match &self.manifest.changes.memory {
            Some(change) => match &change.file {
                Some(file) => {
                    let mut entry = self
                        .archive
                        .by_name(file)
                        .map_err(|_| ArchiveError::MissingEntry(file.clone()))?;
                    let mut buf = Vec::new();
                    entry.read_to_end(&mut buf)?;
                    let reader = BufReader::new(Cursor::new(buf));
                    let mut entries = Vec::new();
                    for line in std::io::BufRead::lines(reader) {
                        let line = line?;
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        entries.push(serde_json::from_str(trimmed)?);
                    }
                    Ok(Some(entries))
                }
                None => Ok(None),
            },
            None => Ok(None),
        }
    }
}

// ===========================================================================
// Delta Memory Entry
// ===========================================================================

/// A memory record within a delta bundle, tagged with an operation.
///
/// Extends `MemoryRecord` with an `operation` field indicating the change
/// type. See `delta-manifest.schema.json` `DeltaMemoryRecord`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeltaMemoryEntry {
    /// The change operation.
    pub operation: DeltaOperation,

    /// The memory record. For `delete` operations, only `id` and `agent_id`
    /// are required.
    #[serde(flatten)]
    pub record: MemoryRecord,
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::{CredentialRecord, CredentialType, EncryptionMetadata};
    use crate::identity::*;
    use crate::memory::*;
    use crate::principals::*;
    use chrono::{NaiveDate, TimeZone, Utc};
    use pretty_assertions::assert_eq;
    use std::io::Cursor;
    use uuid::Uuid;

    // -- Test helpers -------------------------------------------------------

    fn base_manifest() -> Manifest {
        let now = Utc.with_ymd_and_hms(2026, 2, 15, 12, 0, 0).unwrap();
        Manifest {
            alf_version: "1.0.0".into(),
            created_at: now,
            agent: AgentMetadata {
                id: Uuid::new_v4(),
                name: "test-agent".into(),
                source_runtime: "openclaw".into(),
                source_runtime_version: None,
                extra: HashMap::new(),
            },
            layers: LayerInventory {
                identity: None,
                principals: None,
                credentials: None,
                memory: None,
                attachments: None,
                extra: HashMap::new(),
            },
            runtime_hints: None,
            sync: None,
            raw_sources: vec![],
            checksum: None,
            extra: HashMap::new(),
        }
    }

    fn make_record(content: &str) -> MemoryRecord {
        let now = Utc.with_ymd_and_hms(2026, 1, 15, 10, 30, 0).unwrap();
        MemoryRecord {
            id: Uuid::now_v7(),
            agent_id: Uuid::new_v4(),
            content: content.into(),
            memory_type: MemoryType::Semantic,
            source: SourceProvenance {
                runtime: "openclaw".into(),
                runtime_version: None,
                origin: None,
                origin_file: None,
                extraction_method: None,
                session_id: None,
                interaction_id: None,
                identity_version: None,
                extra: HashMap::new(),
            },
            temporal: TemporalMetadata {
                created_at: now,
                updated_at: None,
                observed_at: Some(now),
                valid_from: None,
                valid_until: None,
                last_accessed_at: None,
                access_count: None,
                extra: HashMap::new(),
            },
            status: MemoryStatus::Active,
            namespace: "default".into(),
            category: None,
            supersedes: None,
            confidence: None,
            entities: vec![],
            tags: vec![],
            embeddings: vec![],
            related_records: vec![],
            raw_source_format: None,
            extra: HashMap::new(),
        }
    }

    fn make_identity(agent_id: Uuid) -> Identity {
        let now = Utc.with_ymd_and_hms(2026, 2, 15, 12, 0, 0).unwrap();
        Identity {
            id: Uuid::new_v4(),
            agent_id,
            version: 1,
            updated_at: now,
            structured: Some(StructuredIdentity {
                names: Some(Names {
                    primary: "TestBot".into(),
                    nickname: None,
                    full: None,
                    extra: HashMap::new(),
                }),
                role: Some("Testing assistant".into()),
                goals: vec![],
                psychology: None,
                linguistics: None,
                capabilities: vec![],
                sub_agents: vec![],
                aieos_extensions: None,
                extra: HashMap::new(),
            }),
            prose: None,
            source_format: Some("openclaw".into()),
            raw_source: None,
            extra: HashMap::new(),
        }
    }

    fn make_principals(agent_id: Uuid) -> PrincipalsDocument {
        let now = Utc.with_ymd_and_hms(2026, 2, 15, 12, 0, 0).unwrap();
        let principal_id = Uuid::new_v4();
        PrincipalsDocument {
            principals: vec![Principal {
                id: principal_id,
                principal_type: PrincipalType::Human,
                agent_id: None,
                profile: PrincipalProfile {
                    id: Uuid::new_v4(),
                    agent_id,
                    principal_id,
                    version: 1,
                    updated_at: now,
                    structured: Some(StructuredProfile {
                        name: Some("Alice".into()),
                        principal_type: None,
                        timezone: None,
                        locale: None,
                        communication_preferences: None,
                        work_context: None,
                        relationships: vec![],
                        custom_fields: None,
                        extra: HashMap::new(),
                    }),
                    prose: None,
                    source_format: None,
                    raw_source: None,
                    extra: HashMap::new(),
                },
                extra: HashMap::new(),
            }],
            extra: HashMap::new(),
        }
    }

    fn make_credentials(agent_id: Uuid) -> CredentialsDocument {
        let now = Utc.with_ymd_and_hms(2026, 2, 15, 12, 0, 0).unwrap();
        CredentialsDocument {
            credentials: vec![CredentialRecord {
                id: Uuid::new_v4(),
                agent_id,
                service: "openai".into(),
                credential_type: CredentialType::ApiKey,
                encrypted_payload: "ciphertext==".into(),
                encryption: EncryptionMetadata {
                    algorithm: "xchacha20-poly1305".into(),
                    nonce: "nonce==".into(),
                    kdf: None,
                    kdf_params: None,
                    extra: HashMap::new(),
                },
                created_at: now,
                label: Some("Test Key".into()),
                capabilities_granted: vec![],
                updated_at: None,
                last_rotated_at: None,
                expires_at: None,
                tags: vec![],
                extra: HashMap::new(),
            }],
            extra: HashMap::new(),
        }
    }

    fn make_attachments() -> AttachmentsIndex {
        AttachmentsIndex {
            artifact_size_threshold: Some(102400),
            attachments: vec![AttachmentReference {
                id: Uuid::new_v4(),
                filename: "notes.txt".into(),
                media_type: "text/plain".into(),
                size_bytes: 256,
                hash: ContentHash {
                    algorithm: "sha256".into(),
                    value: "abcdef".into(),
                    extra: HashMap::new(),
                },
                source_path: "workspace/notes.txt".into(),
                archive_path: Some("artifacts/notes.txt".into()),
                remote_ref: None,
                referenced_by: vec![],
                extra: HashMap::new(),
            }],
            extra: HashMap::new(),
        }
    }

    // -- Snapshot tests -----------------------------------------------------

    #[test]
    fn full_archive_round_trip() {
        let manifest = base_manifest();
        let agent_id = manifest.agent.id;

        let records_q4: Vec<MemoryRecord> =
            (0..3).map(|i| make_record(&format!("Q4 memory {i}"))).collect();
        let records_q1: Vec<MemoryRecord> =
            (0..2).map(|i| make_record(&format!("Q1 memory {i}"))).collect();

        let identity = make_identity(agent_id);
        let principals = make_principals(agent_id);
        let credentials = make_credentials(agent_id);
        let attachments = make_attachments();
        let artifact_data = b"These are my notes.";

        // Write
        let buf = Cursor::new(Vec::new());
        let mut writer = AlfWriter::new(buf, manifest.clone()).unwrap();
        writer.set_identity(&identity).unwrap();
        writer.set_principals(&principals).unwrap();
        writer.set_credentials(&credentials).unwrap();
        writer
            .add_memory_partition(
                MemoryPartitionInfo {
                    file: "memory/2025-Q4.jsonl".into(),
                    from: NaiveDate::from_ymd_opt(2025, 10, 1).unwrap(),
                    to: Some(NaiveDate::from_ymd_opt(2025, 12, 31).unwrap()),
                    record_count: 3,
                    sealed: true,
                    extra: HashMap::new(),
                },
                &records_q4,
            )
            .unwrap();
        writer
            .add_memory_partition(
                MemoryPartitionInfo {
                    file: "memory/2026-Q1.jsonl".into(),
                    from: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                    to: None,
                    record_count: 2,
                    sealed: false,
                    extra: HashMap::new(),
                },
                &records_q1,
            )
            .unwrap();
        writer.set_attachments(&attachments).unwrap();
        writer
            .add_artifact("artifacts/notes.txt", artifact_data)
            .unwrap();
        writer
            .add_raw_source("openclaw", "SOUL.md", b"# Soul\nBe helpful.")
            .unwrap();
        let buf = writer.finish().unwrap();

        // Read
        let mut reader = AlfReader::new(Cursor::new(buf.into_inner())).unwrap();
        let read_manifest = reader.manifest().clone();

        // Manifest checks
        assert_eq!(read_manifest.alf_version, "1.0.0");
        assert_eq!(read_manifest.agent.id, agent_id);
        assert_eq!(read_manifest.agent.name, "test-agent");

        // Layer inventory computed correctly
        let layers = &read_manifest.layers;
        assert_eq!(layers.identity.as_ref().unwrap().version, 1);
        assert_eq!(layers.principals.as_ref().unwrap().count, 1);
        assert_eq!(layers.credentials.as_ref().unwrap().count, 1);

        let mem = layers.memory.as_ref().unwrap();
        assert_eq!(mem.record_count, 5);
        assert_eq!(mem.partitions.len(), 2);
        assert_eq!(mem.partitions[0].record_count, 3);
        assert!(mem.partitions[0].sealed);
        assert_eq!(mem.partitions[1].record_count, 2);
        assert!(!mem.partitions[1].sealed);

        let att = layers.attachments.as_ref().unwrap();
        assert_eq!(att.count, 1);
        assert_eq!(att.included_count, Some(1));
        assert_eq!(att.referenced_count, Some(0));

        // Read layers back
        let read_identity = reader.read_identity().unwrap().unwrap();
        assert_eq!(read_identity, identity);

        let read_principals = reader.read_principals().unwrap().unwrap();
        assert_eq!(read_principals, principals);

        let read_credentials = reader.read_credentials().unwrap().unwrap();
        assert_eq!(read_credentials, credentials);

        let read_attachments = reader.read_attachments().unwrap().unwrap();
        assert_eq!(read_attachments, attachments);

        // Read memory
        let all_memory = reader.read_all_memory().unwrap();
        assert_eq!(all_memory.len(), 5);
        assert_eq!(all_memory[0].content, "Q4 memory 0");
        assert_eq!(all_memory[3].content, "Q1 memory 0");

        // Read individual partition
        let q4 = reader.read_memory_partition("memory/2025-Q4.jsonl").unwrap();
        assert_eq!(q4.len(), 3);

        // Read raw artifacts
        let artifact = reader.read_raw_entry("artifacts/notes.txt").unwrap();
        assert_eq!(artifact, artifact_data);

        let soul = reader.read_raw_entry("raw/openclaw/SOUL.md").unwrap();
        assert_eq!(soul, b"# Soul\nBe helpful.");
    }

    #[test]
    fn minimal_archive_round_trip() {
        let manifest = base_manifest();

        let records: Vec<MemoryRecord> = vec![make_record("Only memory")];

        let buf = Cursor::new(Vec::new());
        let mut writer = AlfWriter::new(buf, manifest).unwrap();
        writer
            .add_memory_partition(
                MemoryPartitionInfo {
                    file: "memory/2026-Q1.jsonl".into(),
                    from: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                    to: None,
                    record_count: 1,
                    sealed: false,
                    extra: HashMap::new(),
                },
                &records,
            )
            .unwrap();
        let buf = writer.finish().unwrap();

        let mut reader = AlfReader::new(Cursor::new(buf.into_inner())).unwrap();
        let m = reader.manifest();

        // Only memory layer present
        assert!(m.layers.identity.is_none());
        assert!(m.layers.principals.is_none());
        assert!(m.layers.credentials.is_none());
        assert!(m.layers.attachments.is_none());
        assert_eq!(m.layers.memory.as_ref().unwrap().record_count, 1);

        // Read back
        assert!(reader.read_identity().unwrap().is_none());
        assert!(reader.read_principals().unwrap().is_none());
        assert!(reader.read_credentials().unwrap().is_none());
        assert!(reader.read_attachments().unwrap().is_none());

        let records_back = reader.read_all_memory().unwrap();
        assert_eq!(records_back.len(), 1);
        assert_eq!(records_back[0].content, "Only memory");
    }

    #[test]
    fn empty_archive() {
        let manifest = base_manifest();

        let buf = Cursor::new(Vec::new());
        let writer = AlfWriter::new(buf, manifest).unwrap();
        let buf = writer.finish().unwrap();

        let mut reader = AlfReader::new(Cursor::new(buf.into_inner())).unwrap();
        assert!(reader.manifest().layers.memory.is_none());
        assert_eq!(reader.read_all_memory().unwrap().len(), 0);
    }

    #[test]
    fn manifest_record_counts_match() {
        let manifest = base_manifest();
        let records: Vec<MemoryRecord> =
            (0..10).map(|i| make_record(&format!("mem {i}"))).collect();

        let buf = Cursor::new(Vec::new());
        let mut writer = AlfWriter::new(buf, manifest).unwrap();
        writer
            .add_memory_partition(
                MemoryPartitionInfo {
                    file: "memory/2026-Q1.jsonl".into(),
                    from: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                    to: None,
                    record_count: 10,
                    sealed: false,
                    extra: HashMap::new(),
                },
                &records,
            )
            .unwrap();
        let buf = writer.finish().unwrap();

        let reader = AlfReader::new(Cursor::new(buf.into_inner())).unwrap();
        let mem = reader.manifest().layers.memory.as_ref().unwrap();
        assert_eq!(mem.record_count, 10);
        assert_eq!(mem.partitions[0].record_count, 10);
    }

    #[test]
    fn has_embeddings_tracking() {
        let manifest = base_manifest();
        let now = Utc::now();

        let mut record = make_record("with embedding");
        record.embeddings = vec![Embedding {
            model: "openai/text-embedding-3-small".into(),
            dimensions: 4,
            vector: vec![0.1, 0.2, 0.3, 0.4],
            computed_at: now,
            source: EmbeddingSource::Runtime,
            extra: HashMap::new(),
        }];

        let buf = Cursor::new(Vec::new());
        let mut writer = AlfWriter::new(buf, manifest).unwrap();
        writer
            .add_memory_partition(
                MemoryPartitionInfo {
                    file: "memory/2026-Q1.jsonl".into(),
                    from: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                    to: None,
                    record_count: 1,
                    sealed: false,
                    extra: HashMap::new(),
                },
                &[record],
            )
            .unwrap();
        let buf = writer.finish().unwrap();

        let reader = AlfReader::new(Cursor::new(buf.into_inner())).unwrap();
        let mem = reader.manifest().layers.memory.as_ref().unwrap();
        assert_eq!(mem.has_embeddings, Some(true));
    }

    #[test]
    fn file_names_listing() {
        let manifest = base_manifest();
        let agent_id = manifest.agent.id;

        let buf = Cursor::new(Vec::new());
        let mut writer = AlfWriter::new(buf, manifest).unwrap();
        writer
            .set_identity(&make_identity(agent_id))
            .unwrap();
        writer
            .add_memory_partition(
                MemoryPartitionInfo {
                    file: "memory/2026-Q1.jsonl".into(),
                    from: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                    to: None,
                    record_count: 1,
                    sealed: false,
                    extra: HashMap::new(),
                },
                &[make_record("test")],
            )
            .unwrap();
        let buf = writer.finish().unwrap();

        let reader = AlfReader::new(Cursor::new(buf.into_inner())).unwrap();
        let names = reader.file_names();
        assert!(names.contains(&"manifest.json".to_string()));
        assert!(names.contains(&"identity.json".to_string()));
        assert!(names.contains(&"memory/2026-Q1.jsonl".to_string()));
    }

    // -- Delta tests --------------------------------------------------------

    #[test]
    fn delta_memory_only_round_trip() {
        let now = Utc.with_ymd_and_hms(2026, 2, 16, 9, 0, 0).unwrap();
        let agent_id = Uuid::new_v4();

        let delta_manifest = DeltaManifest {
            alf_version: "1.0.0".into(),
            created_at: now,
            agent: DeltaAgentRef {
                id: agent_id,
                source_runtime: Some("openclaw".into()),
                extra: HashMap::new(),
            },
            sync: DeltaSyncCursor {
                base_sequence: 5,
                new_sequence: 6,
                base_timestamp: None,
                new_timestamp: None,
                extra: HashMap::new(),
            },
            changes: ChangeInventory {
                identity: None,
                principals: None,
                credentials: None,
                memory: None,
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        };

        let entries = vec![
            DeltaMemoryEntry {
                operation: DeltaOperation::Create,
                record: make_record("new memory"),
            },
            DeltaMemoryEntry {
                operation: DeltaOperation::Update,
                record: make_record("updated memory"),
            },
        ];

        // Write
        let buf = Cursor::new(Vec::new());
        let mut writer = DeltaWriter::new(buf, delta_manifest).unwrap();
        writer.add_memory_deltas(&entries).unwrap();
        let buf = writer.finish().unwrap();

        // Read
        let mut reader = DeltaReader::new(Cursor::new(buf.into_inner())).unwrap();
        let dm = reader.manifest();
        assert_eq!(dm.sync.base_sequence, 5);
        assert_eq!(dm.sync.new_sequence, 6);

        // Only memory changed
        assert!(dm.changes.identity.is_none());
        assert!(dm.changes.principals.is_none());
        assert!(dm.changes.credentials.is_none());
        assert!(dm.changes.memory.is_some());
        assert_eq!(dm.changes.memory.as_ref().unwrap().record_count, Some(2));

        let read_entries = reader.read_memory_deltas().unwrap().unwrap();
        assert_eq!(read_entries.len(), 2);
        assert_eq!(read_entries[0].operation, DeltaOperation::Create);
        assert_eq!(read_entries[0].record.content, "new memory");
        assert_eq!(read_entries[1].operation, DeltaOperation::Update);
    }

    #[test]
    fn delta_all_layers_round_trip() {
        let now = Utc.with_ymd_and_hms(2026, 2, 16, 9, 0, 0).unwrap();
        let agent_id = Uuid::new_v4();
        let changed_principal_id = Uuid::new_v4();

        let delta_manifest = DeltaManifest {
            alf_version: "1.0.0".into(),
            created_at: now,
            agent: DeltaAgentRef {
                id: agent_id,
                source_runtime: None,
                extra: HashMap::new(),
            },
            sync: DeltaSyncCursor {
                base_sequence: 10,
                new_sequence: 11,
                base_timestamp: None,
                new_timestamp: None,
                extra: HashMap::new(),
            },
            changes: ChangeInventory {
                identity: None,
                principals: None,
                credentials: None,
                memory: None,
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        };

        let identity = make_identity(agent_id);
        let principals = make_principals(agent_id);
        let credentials = make_credentials(agent_id);
        let entries = vec![DeltaMemoryEntry {
            operation: DeltaOperation::Delete,
            record: make_record("deleted"),
        }];

        // Write
        let buf = Cursor::new(Vec::new());
        let mut writer = DeltaWriter::new(buf, delta_manifest).unwrap();
        writer.set_identity(&identity, 2).unwrap();
        writer
            .set_principals(&principals, vec![changed_principal_id])
            .unwrap();
        writer.set_credentials(&credentials).unwrap();
        writer.add_memory_deltas(&entries).unwrap();
        let buf = writer.finish().unwrap();

        // Read
        let mut reader = DeltaReader::new(Cursor::new(buf.into_inner())).unwrap();
        let dm = reader.manifest();

        assert_eq!(
            dm.changes.identity.as_ref().unwrap().new_version,
            Some(2)
        );
        assert_eq!(
            dm.changes.principals.as_ref().unwrap().changed_ids,
            vec![changed_principal_id]
        );
        assert!(dm.changes.credentials.is_some());
        assert_eq!(dm.changes.memory.as_ref().unwrap().record_count, Some(1));

        // Read each layer
        let read_identity = reader.read_identity().unwrap().unwrap();
        assert_eq!(read_identity, identity);

        let read_principals = reader.read_principals().unwrap().unwrap();
        assert_eq!(read_principals, principals);

        let read_credentials = reader.read_credentials().unwrap().unwrap();
        assert_eq!(read_credentials, credentials);

        let read_entries = reader.read_memory_deltas().unwrap().unwrap();
        assert_eq!(read_entries[0].operation, DeltaOperation::Delete);
    }

    #[test]
    fn delta_empty_changes() {
        let now = Utc::now();
        let delta_manifest = DeltaManifest {
            alf_version: "1.0.0".into(),
            created_at: now,
            agent: DeltaAgentRef {
                id: Uuid::new_v4(),
                source_runtime: None,
                extra: HashMap::new(),
            },
            sync: DeltaSyncCursor {
                base_sequence: 1,
                new_sequence: 2,
                base_timestamp: None,
                new_timestamp: None,
                extra: HashMap::new(),
            },
            changes: ChangeInventory {
                identity: None,
                principals: None,
                credentials: None,
                memory: None,
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        };

        let buf = Cursor::new(Vec::new());
        let writer = DeltaWriter::new(buf, delta_manifest).unwrap();
        let buf = writer.finish().unwrap();

        let mut reader = DeltaReader::new(Cursor::new(buf.into_inner())).unwrap();
        assert!(reader.read_identity().unwrap().is_none());
        assert!(reader.read_principals().unwrap().is_none());
        assert!(reader.read_credentials().unwrap().is_none());
        assert!(reader.read_memory_deltas().unwrap().is_none());
    }
}