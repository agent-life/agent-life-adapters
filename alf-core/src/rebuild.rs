//! Snapshot reconstruction: merge a base `.alf` with a sequence of `.alf-delta` archives.
//!
//! The core operation is `rebuild_snapshot`: it reads all layers from the base
//! snapshot, applies each delta in order, re-partitions memory records, and
//! writes a new `.alf` archive containing the fully merged state.
//!
//! This is used by:
//! - `alf restore` to produce an up-to-date snapshot from the service's
//!   base-snapshot + delta chain
//! - Server-side compaction (Phase 5) to collapse deltas into a new snapshot

use std::collections::BTreeMap;
use std::io::Cursor;

use chrono::Utc;

use crate::archive::{AlfReader, AlfWriter, DeltaReader};
use crate::delta::apply_delta;
use crate::manifest::{Manifest, MemoryPartitionInfo, SyncCursor};
use crate::memory::MemoryRecord;
use crate::partition::PartitionAssigner;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Errors that can occur during snapshot reconstruction.
#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("archive error: {0}")]
    Archive(#[from] crate::archive::ArchiveError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Rebuild a snapshot by applying a sequence of deltas to a base snapshot.
///
/// Returns the bytes of a new `.alf` archive containing the fully merged state.
/// All layers (identity, principals, credentials, memory, attachments, raw sources)
/// are carried forward from the base, with any delta-provided replacements applied.
///
/// Memory records are re-partitioned using `PartitionAssigner` after delta
/// application, so partition boundaries are always correct in the output.
///
/// If `delta_bytes_list` is empty, the base snapshot is re-serialized (which
/// normalizes partition layout and updates `created_at`).
pub fn rebuild_snapshot(
    base_bytes: &[u8],
    delta_bytes_list: &[&[u8]],
) -> Result<Vec<u8>, RebuildError> {
    // ── 1. Read all layers from the base snapshot ─────────────────

    let mut base = AlfReader::new(Cursor::new(base_bytes))?;
    let base_manifest = base.manifest().clone();

    let mut identity = base.read_identity()?;
    let mut principals = base.read_principals()?;
    let mut credentials = base.read_credentials()?;
    let mut memory_records = base.read_all_memory()?;
    let attachments = base.read_attachments()?;

    // Collect raw source files (everything under raw/). Keyed by archive path so
    // delta raw overlays (see below) can replace/remove individual files; the
    // BTreeMap also gives a deterministic write order.
    let all_files = base.file_names();
    let raw_paths: Vec<String> = all_files
        .iter()
        .filter(|p| p.starts_with("raw/") && !p.ends_with('/'))
        .cloned()
        .collect();

    let mut raw_sources: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for path in &raw_paths {
        let data = base.read_raw_entry(path)?;
        raw_sources.insert(path.clone(), data);
    }

    // Collect artifact files (everything under artifacts/)
    let artifact_paths: Vec<String> = all_files
        .iter()
        .filter(|p| p.starts_with("artifacts/") && !p.ends_with('/'))
        .cloned()
        .collect();

    let mut artifact_files: Vec<(String, Vec<u8>)> = Vec::new();
    for path in &artifact_paths {
        let data = base.read_raw_entry(path)?;
        artifact_files.push((path.clone(), data));
    }

    // ── 2. Apply each delta in order ──────────────────────────────

    let mut highest_sequence: u64 = base_manifest
        .sync
        .as_ref()
        .map(|s| s.last_sequence)
        .unwrap_or(0);

    for delta_bytes in delta_bytes_list {
        let mut delta = DeltaReader::new(Cursor::new(delta_bytes))?;
        let delta_manifest = delta.manifest().clone();

        // Replace identity if delta carries one
        if let Some(new_identity) = delta.read_identity()? {
            identity = Some(new_identity);
        }

        // Replace principals if delta carries them
        if let Some(new_principals) = delta.read_principals()? {
            principals = Some(new_principals);
        }

        // Replace credentials if delta carries them
        if let Some(new_credentials) = delta.read_credentials()? {
            credentials = Some(new_credentials);
        }

        // Apply memory deltas
        if let Some(entries) = delta.read_memory_deltas()? {
            memory_records = apply_delta(&memory_records, &entries);
        }

        // Overlay raw source changes: drop removed paths, then insert/replace
        // the verbatim files carried in this delta. Keeps a same-runtime,
        // raw-preferred restore current instead of frozen at the snapshot.
        if let Some(raw_change) = delta_manifest.changes.raw.clone() {
            for path in &raw_change.deleted {
                raw_sources.remove(path);
            }
            for path in &raw_change.changed {
                let data = delta.read_raw_entry(path)?;
                raw_sources.insert(path.clone(), data);
            }
        }

        // Track the highest sequence we've seen
        let delta_seq = delta_manifest.sync.new_sequence;
        if delta_seq > 0 && delta_seq > highest_sequence {
            highest_sequence = delta_seq;
        }
        let delta_base_seq = delta_manifest.sync.base_sequence;
        if delta_base_seq + 1 > highest_sequence {
            highest_sequence = delta_base_seq + 1;
        }
    }

    // ── 3. Re-partition memory records ────────────────────────────

    let partitions = partition_records(&memory_records);

    // ── 4. Build new manifest ─────────────────────────────────────

    let manifest = Manifest {
        alf_version: base_manifest.alf_version,
        created_at: Utc::now(),
        agent: base_manifest.agent,
        // layers is overwritten by AlfWriter::finish()
        layers: base_manifest.layers,
        runtime_hints: base_manifest.runtime_hints,
        sync: Some(SyncCursor {
            last_sequence: highest_sequence,
            last_sync_at: Some(Utc::now()),
            extra: std::collections::HashMap::new(),
        }),
        raw_sources: base_manifest.raw_sources,
        checksum: None, // recalculated on next export if needed
        extra: base_manifest.extra,
    };

    // ── 5. Write new archive ──────────────────────────────────────

    let buf = Cursor::new(Vec::new());
    let mut writer = AlfWriter::new(buf, manifest)?;

    if let Some(ref id) = identity {
        writer.set_identity(id)?;
    }

    if let Some(ref p) = principals {
        writer.set_principals(p)?;
    }

    if let Some(ref c) = credentials {
        writer.set_credentials(c)?;
    }

    for (info, records) in &partitions {
        writer.add_memory_partition(info.clone(), records)?;
    }

    if let Some(ref att) = attachments {
        writer.set_attachments(att)?;
    }

    for (path, data) in &artifact_files {
        writer.add_artifact(path, data)?;
    }

    // Write raw sources — parse "raw/{runtime}/{relative_path}"
    for (full_path, data) in &raw_sources {
        if let Some(rest) = full_path.strip_prefix("raw/") {
            if let Some(slash_pos) = rest.find('/') {
                let runtime = &rest[..slash_pos];
                let relative = &rest[slash_pos + 1..];
                writer.add_raw_source(runtime, relative, data)?;
            }
        }
    }

    let inner = writer.finish()?;
    Ok(inner.into_inner())
}

/// Rewrite a snapshot's memory layer with the given records, carrying every
/// other layer (identity, principals, credentials, attachments, raw sources,
/// artifacts) through unchanged.
///
/// Used by `alf sync` to persist reconciled record identities (WP4.1) into the
/// archive that is both uploaded and kept as the local delta base — the two
/// must be the same bytes, or local and cloud ids diverge on the re-snapshot
/// path.
///
/// Records are re-partitioned: reconciliation carries `created_at` backwards
/// to a record's first observation, so the export's original partition layout
/// may be stale. Unlike [`rebuild_snapshot`], the manifest is cloned from the
/// input — `created_at`, the agent block, and the sync cursor are NOT
/// restamped (the layer inventory is recomputed by `AlfWriter::finish()`).
pub fn replace_memory_records(
    snapshot_bytes: &[u8],
    records: &[MemoryRecord],
) -> Result<Vec<u8>, RebuildError> {
    let mut base = AlfReader::new(Cursor::new(snapshot_bytes))?;
    let manifest = base.manifest().clone();

    let identity = base.read_identity()?;
    let principals = base.read_principals()?;
    let credentials = base.read_credentials()?;
    let attachments = base.read_attachments()?;

    let all_files = base.file_names();
    let raw_paths: Vec<String> = all_files
        .iter()
        .filter(|p| p.starts_with("raw/") && !p.ends_with('/'))
        .cloned()
        .collect();
    let mut raw_sources: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for path in &raw_paths {
        raw_sources.insert(path.clone(), base.read_raw_entry(path)?);
    }

    let artifact_paths: Vec<String> = all_files
        .iter()
        .filter(|p| p.starts_with("artifacts/") && !p.ends_with('/'))
        .cloned()
        .collect();
    let mut artifact_files: Vec<(String, Vec<u8>)> = Vec::new();
    for path in &artifact_paths {
        artifact_files.push((path.clone(), base.read_raw_entry(path)?));
    }

    let partitions = partition_records(records);

    let buf = Cursor::new(Vec::new());
    let mut writer = AlfWriter::new(buf, manifest)?;

    if let Some(ref id) = identity {
        writer.set_identity(id)?;
    }
    if let Some(ref p) = principals {
        writer.set_principals(p)?;
    }
    if let Some(ref c) = credentials {
        writer.set_credentials(c)?;
    }
    for (info, group) in &partitions {
        writer.add_memory_partition(info.clone(), group)?;
    }
    if let Some(ref att) = attachments {
        writer.set_attachments(att)?;
    }
    for (path, data) in &artifact_files {
        writer.add_artifact(path, data)?;
    }
    for (full_path, data) in &raw_sources {
        if let Some(rest) = full_path.strip_prefix("raw/") {
            if let Some(slash_pos) = rest.find('/') {
                let runtime = &rest[..slash_pos];
                let relative = &rest[slash_pos + 1..];
                writer.add_raw_source(runtime, relative, data)?;
            }
        }
    }

    let inner = writer.finish()?;
    Ok(inner.into_inner())
}

/// Group records into quarterly partitions with computed partition metadata.
/// Shared by [`rebuild_snapshot`] and [`replace_memory_records`].
fn partition_records(records: &[MemoryRecord]) -> Vec<(MemoryPartitionInfo, Vec<MemoryRecord>)> {
    let mut partition_groups: BTreeMap<String, Vec<MemoryRecord>> = BTreeMap::new();
    for record in records {
        let file_path = PartitionAssigner::partition_for_record(record);
        partition_groups
            .entry(file_path)
            .or_default()
            .push(record.clone());
    }

    let today = Utc::now().date_naive();
    partition_groups
        .into_iter()
        .map(|(file_path, records)| {
            let (from, to) = PartitionAssigner::date_range_for_partition(&file_path)
                .unwrap_or_else(|| {
                    // Fallback: use the first record's timestamp for from, no to
                    let first = records.first().expect("partition group is non-empty");
                    let ts = first
                        .temporal
                        .observed_at
                        .unwrap_or(first.temporal.created_at);
                    (ts.date_naive(), today)
                });

            let sealed = to < today;

            let info = MemoryPartitionInfo {
                file: file_path,
                from,
                to: Some(to),
                record_count: records.len() as u64,
                sealed,
                extra: std::collections::HashMap::new(),
            };
            (info, records)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::{DeltaMemoryEntry, DeltaWriter};
    use crate::identity::{Identity, ProseIdentity};
    use crate::manifest::*;
    use crate::memory::*;
    use chrono::TimeZone;
    use std::collections::HashMap;

    // -- Test helpers ----------------------------------------------------------

    fn make_agent_metadata() -> AgentMetadata {
        AgentMetadata {
            id: uuid::Uuid::parse_str("a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d").unwrap(),
            name: "Test Agent".into(),
            source_runtime: "test".into(),
            source_runtime_version: None,
            extra: HashMap::new(),
        }
    }

    fn make_manifest() -> Manifest {
        Manifest {
            alf_version: "1.0.0".into(),
            created_at: Utc::now(),
            agent: make_agent_metadata(),
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

    fn make_record(id_suffix: u8, content: &str, month: u32) -> MemoryRecord {
        let mut id_bytes = [0u8; 16];
        id_bytes[15] = id_suffix;
        MemoryRecord {
            id: uuid::Uuid::from_bytes(id_bytes),
            agent_id: make_agent_metadata().id,
            content: content.into(),
            memory_type: MemoryType::Semantic,
            source: SourceProvenance {
                runtime: "test".into(),
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
                created_at: Utc.with_ymd_and_hms(2026, month, 15, 10, 0, 0).unwrap(),
                updated_at: None,
                observed_at: Some(Utc.with_ymd_and_hms(2026, month, 15, 10, 0, 0).unwrap()),
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

    fn make_identity(version: u32, soul: &str) -> Identity {
        Identity {
            id: uuid::Uuid::new_v4(),
            agent_id: make_agent_metadata().id,
            version,
            updated_at: Utc::now(),
            prose: Some(ProseIdentity {
                soul: Some(soul.into()),
                operating_instructions: None,
                identity_profile: None,
                custom_blocks: HashMap::new(),
                extra: HashMap::new(),
            }),
            structured: None,
            source_format: None,
            raw_source: None,
            extra: HashMap::new(),
        }
    }

    /// Build a snapshot archive in memory from layers.
    fn build_snapshot(identity: Option<&Identity>, records: &[MemoryRecord]) -> Vec<u8> {
        let buf = Cursor::new(Vec::new());
        let mut writer = AlfWriter::new(buf, make_manifest()).unwrap();

        if let Some(id) = identity {
            writer.set_identity(id).unwrap();
        }

        // Group records into partitions
        let mut groups: BTreeMap<String, Vec<MemoryRecord>> = BTreeMap::new();
        for r in records {
            let path = PartitionAssigner::partition_for_record(r);
            groups.entry(path).or_default().push(r.clone());
        }
        for (file_path, group_records) in &groups {
            let (from, to) = PartitionAssigner::date_range_for_partition(file_path).unwrap();
            let info = MemoryPartitionInfo {
                file: file_path.clone(),
                from,
                to: Some(to),
                record_count: group_records.len() as u64,
                sealed: false,
                extra: HashMap::new(),
            };
            writer.add_memory_partition(info, group_records).unwrap();
        }

        let inner = writer.finish().unwrap();
        inner.into_inner()
    }

    /// Build a delta archive in memory.
    fn build_delta(
        base_sequence: u64,
        identity: Option<(&Identity, u32)>,
        memory_entries: &[DeltaMemoryEntry],
    ) -> Vec<u8> {
        let delta_manifest = DeltaManifest {
            alf_version: "1.0.0".into(),
            created_at: Utc::now(),
            agent: DeltaAgentRef {
                id: make_agent_metadata().id,
                source_runtime: Some("test".into()),
                extra: HashMap::new(),
            },
            sync: DeltaSyncCursor {
                base_sequence,
                new_sequence: base_sequence + 1,
                base_timestamp: None,
                new_timestamp: None,
                extra: HashMap::new(),
            },
            changes: ChangeInventory {
                identity: None,
                principals: None,
                credentials: None,
                memory: None,
                raw: None,
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        };

        let buf = Cursor::new(Vec::new());
        let mut writer = DeltaWriter::new(buf, delta_manifest).unwrap();

        if let Some((id, version)) = identity {
            writer.set_identity(id, version).unwrap();
        }

        if !memory_entries.is_empty() {
            writer.add_memory_deltas(memory_entries).unwrap();
        }

        let inner = writer.finish().unwrap();
        inner.into_inner()
    }

    fn read_all_memory(bytes: &[u8]) -> Vec<MemoryRecord> {
        let mut reader = AlfReader::new(Cursor::new(bytes)).unwrap();
        reader.read_all_memory().unwrap()
    }

    fn read_identity(bytes: &[u8]) -> Option<Identity> {
        let mut reader = AlfReader::new(Cursor::new(bytes)).unwrap();
        reader.read_identity().unwrap()
    }

    // -- Credential helpers ----------------------------------------------------

    fn make_credential(
        suffix: u8,
        ciphertext: &str,
        nonce: &str,
    ) -> crate::credentials::CredentialRecord {
        use crate::credentials::{CredentialRecord, CredentialType, EncryptionMetadata};
        let mut id_bytes = [0u8; 16];
        id_bytes[15] = suffix;
        CredentialRecord {
            id: uuid::Uuid::from_bytes(id_bytes),
            agent_id: make_agent_metadata().id,
            service: "test".into(),
            credential_type: CredentialType::Account,
            encrypted_payload: ciphertext.into(),
            encryption: EncryptionMetadata {
                algorithm: "xchacha20-poly1305".into(),
                nonce: nonce.into(),
                kdf: None,
                kdf_params: None,
                extra: HashMap::new(),
            },
            created_at: Utc::now(),
            label: None,
            description: None,
            capabilities_granted: vec![],
            updated_at: None,
            last_rotated_at: None,
            expires_at: None,
            tags: vec![],
            extra: HashMap::new(),
        }
    }

    fn make_credentials_doc(
        records: Vec<crate::credentials::CredentialRecord>,
    ) -> crate::credentials::CredentialsDocument {
        crate::credentials::CredentialsDocument {
            credentials: records,
            extra: HashMap::new(),
        }
    }

    /// Build a snapshot whose only layer is credentials.
    fn build_snapshot_creds(creds: &crate::credentials::CredentialsDocument) -> Vec<u8> {
        let buf = Cursor::new(Vec::new());
        let mut writer = AlfWriter::new(buf, make_manifest()).unwrap();
        writer.set_credentials(creds).unwrap();
        writer.finish().unwrap().into_inner()
    }

    /// Build a delta carrying credentials (and nothing else).
    fn build_delta_creds(
        base_sequence: u64,
        creds: &crate::credentials::CredentialsDocument,
    ) -> Vec<u8> {
        let delta_manifest = DeltaManifest {
            alf_version: "1.0.0".into(),
            created_at: Utc::now(),
            agent: DeltaAgentRef {
                id: make_agent_metadata().id,
                source_runtime: Some("test".into()),
                extra: HashMap::new(),
            },
            sync: DeltaSyncCursor {
                base_sequence,
                new_sequence: base_sequence + 1,
                base_timestamp: None,
                new_timestamp: None,
                extra: HashMap::new(),
            },
            changes: ChangeInventory {
                identity: None,
                principals: None,
                credentials: None,
                memory: None,
                raw: None,
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        };
        let buf = Cursor::new(Vec::new());
        let mut writer = DeltaWriter::new(buf, delta_manifest).unwrap();
        writer.set_credentials(creds).unwrap();
        writer.finish().unwrap().into_inner()
    }

    fn read_credentials(bytes: &[u8]) -> Option<crate::credentials::CredentialsDocument> {
        let mut reader = AlfReader::new(Cursor::new(bytes)).unwrap();
        reader.read_credentials().unwrap()
    }

    // -- Tests ----------------------------------------------------------------

    #[test]
    fn rebuild_no_deltas_returns_equivalent_snapshot() {
        let records = vec![
            make_record(1, "First memory", 1),
            make_record(2, "Second memory", 1),
            make_record(3, "Third memory", 2),
        ];
        let identity = make_identity(1, "Test soul");
        let base = build_snapshot(Some(&identity), &records);

        let rebuilt = rebuild_snapshot(&base, &[]).unwrap();

        let rebuilt_records = read_all_memory(&rebuilt);
        assert_eq!(rebuilt_records.len(), 3);
        // Records should match by content (IDs are deterministic)
        let contents: Vec<&str> = rebuilt_records.iter().map(|r| r.content.as_str()).collect();
        assert!(contents.contains(&"First memory"));
        assert!(contents.contains(&"Second memory"));
        assert!(contents.contains(&"Third memory"));

        let rebuilt_identity = read_identity(&rebuilt);
        assert!(rebuilt_identity.is_some());
        assert_eq!(rebuilt_identity.unwrap().version, 1);
    }

    #[test]
    fn rebuild_with_memory_delta() {
        let record_a = make_record(1, "Record A", 1);
        let record_b = make_record(2, "Record B", 1);
        let record_c = make_record(3, "Record C", 1);
        let base = build_snapshot(
            None,
            &[record_a.clone(), record_b.clone(), record_c.clone()],
        );

        // Delta: create D, update B, delete A
        let record_d = make_record(4, "Record D", 1);
        let mut updated_b = record_b.clone();
        updated_b.content = "Record B updated".into();

        let entries = vec![
            DeltaMemoryEntry {
                operation: DeltaOperation::Create,
                record: record_d.clone(),
            },
            DeltaMemoryEntry {
                operation: DeltaOperation::Update,
                record: updated_b.clone(),
            },
            DeltaMemoryEntry {
                operation: DeltaOperation::Delete,
                record: record_a.clone(),
            },
        ];
        let delta = build_delta(0, None, &entries);

        let rebuilt = rebuild_snapshot(&base, &[delta.as_slice()]).unwrap();
        let rebuilt_records = read_all_memory(&rebuilt);

        assert_eq!(rebuilt_records.len(), 3); // B(updated) + C + D
        let contents: Vec<&str> = rebuilt_records.iter().map(|r| r.content.as_str()).collect();
        assert!(!contents.contains(&"Record A"), "A should be deleted");
        assert!(
            contents.contains(&"Record B updated"),
            "B should be updated"
        );
        assert!(contents.contains(&"Record C"), "C should be unchanged");
        assert!(contents.contains(&"Record D"), "D should be created");
    }

    #[test]
    fn rebuild_with_identity_delta() {
        let identity_v1 = make_identity(1, "Original soul");
        let base = build_snapshot(Some(&identity_v1), &[make_record(1, "Memory", 1)]);

        let identity_v2 = make_identity(2, "Updated soul");
        let delta = build_delta(0, Some((&identity_v2, 2)), &[]);

        let rebuilt = rebuild_snapshot(&base, &[delta.as_slice()]).unwrap();
        let rebuilt_identity = read_identity(&rebuilt).unwrap();
        assert_eq!(rebuilt_identity.version, 2);
        assert_eq!(
            rebuilt_identity.prose.unwrap().soul.unwrap(),
            "Updated soul"
        );
    }

    #[test]
    fn rebuild_with_multiple_deltas() {
        let record_a = make_record(1, "Record A", 1);
        let record_b = make_record(2, "Record B", 1);
        let base = build_snapshot(None, &[record_a.clone(), record_b.clone()]);

        // Delta 1: add C
        let record_c = make_record(3, "Record C", 1);
        let delta1 = build_delta(
            0,
            None,
            &[DeltaMemoryEntry {
                operation: DeltaOperation::Create,
                record: record_c.clone(),
            }],
        );

        // Delta 2: delete A, update B
        let mut updated_b = record_b.clone();
        updated_b.content = "Record B v2".into();
        let delta2 = build_delta(
            1,
            None,
            &[
                DeltaMemoryEntry {
                    operation: DeltaOperation::Delete,
                    record: record_a.clone(),
                },
                DeltaMemoryEntry {
                    operation: DeltaOperation::Update,
                    record: updated_b.clone(),
                },
            ],
        );

        let rebuilt = rebuild_snapshot(&base, &[delta1.as_slice(), delta2.as_slice()]).unwrap();

        let rebuilt_records = read_all_memory(&rebuilt);
        assert_eq!(rebuilt_records.len(), 2); // B(v2) + C
        let contents: Vec<&str> = rebuilt_records.iter().map(|r| r.content.as_str()).collect();
        assert!(contents.contains(&"Record B v2"));
        assert!(contents.contains(&"Record C"));
        assert!(!contents.contains(&"Record A"));
    }

    #[test]
    fn rebuild_preserves_raw_sources() {
        // Build a base snapshot with a raw source file
        let buf = Cursor::new(Vec::new());
        let mut writer = AlfWriter::new(buf, {
            let mut m = make_manifest();
            m.raw_sources = vec!["test-runtime".into()];
            m
        })
        .unwrap();

        writer
            .add_raw_source(
                "test-runtime",
                "config.toml",
                b"[settings]\nkey = \"value\"",
            )
            .unwrap();

        let record = make_record(1, "Memory", 1);
        let (from, to) =
            PartitionAssigner::date_range_for_partition("memory/2026-Q1.jsonl").unwrap();
        writer
            .add_memory_partition(
                MemoryPartitionInfo {
                    file: "memory/2026-Q1.jsonl".into(),
                    from,
                    to: Some(to),
                    record_count: 1,
                    sealed: false,
                    extra: HashMap::new(),
                },
                &[record],
            )
            .unwrap();

        let base = writer.finish().unwrap().into_inner();

        // Apply a memory-only delta
        let new_record = make_record(2, "New memory", 1);
        let delta = build_delta(
            0,
            None,
            &[DeltaMemoryEntry {
                operation: DeltaOperation::Create,
                record: new_record,
            }],
        );

        let rebuilt = rebuild_snapshot(&base, &[delta.as_slice()]).unwrap();

        // Verify raw source survived
        let mut reader = AlfReader::new(Cursor::new(&rebuilt)).unwrap();
        let files = reader.file_names();
        assert!(
            files
                .iter()
                .any(|f| f.contains("raw/test-runtime/config.toml")),
            "raw source should be preserved. Files: {:?}",
            files
        );
        let raw_data = reader
            .read_raw_entry("raw/test-runtime/config.toml")
            .unwrap();
        assert_eq!(raw_data, b"[settings]\nkey = \"value\"");

        // Verify memory has both records
        let records = reader.read_all_memory().unwrap();
        assert_eq!(records.len(), 2);
    }

    /// A delta carrying raw-source changes must overlay them onto the base raw
    /// tree: replace updated files, add new ones, drop deleted ones. Without
    /// this a same-runtime, raw-preferred `alf restore` rebuilds the workspace
    /// from a raw tree frozen at the snapshot — the bug this fixes.
    #[test]
    fn rebuild_applies_raw_source_deltas() {
        // Base snapshot: three raw files + one memory record.
        let buf = Cursor::new(Vec::new());
        let mut writer = AlfWriter::new(buf, {
            let mut m = make_manifest();
            m.raw_sources = vec!["test-runtime".into()];
            m
        })
        .unwrap();
        writer
            .add_raw_source("test-runtime", "SOUL.md", b"soul v0")
            .unwrap();
        writer
            .add_raw_source("test-runtime", "memory/2026-01-15.md", b"day 15")
            .unwrap();
        writer
            .add_raw_source("test-runtime", "stale.md", b"remove me")
            .unwrap();
        let record = make_record(1, "Memory", 1);
        let (from, to) =
            PartitionAssigner::date_range_for_partition("memory/2026-Q1.jsonl").unwrap();
        writer
            .add_memory_partition(
                MemoryPartitionInfo {
                    file: "memory/2026-Q1.jsonl".into(),
                    from,
                    to: Some(to),
                    record_count: 1,
                    sealed: false,
                    extra: HashMap::new(),
                },
                &[record],
            )
            .unwrap();
        let base = writer.finish().unwrap().into_inner();

        // Delta: update SOUL.md, add a new daily file, delete stale.md.
        let delta = {
            let delta_manifest = DeltaManifest {
                alf_version: "1.0.0".into(),
                created_at: Utc::now(),
                agent: DeltaAgentRef {
                    id: make_agent_metadata().id,
                    source_runtime: Some("test-runtime".into()),
                    extra: HashMap::new(),
                },
                sync: DeltaSyncCursor {
                    base_sequence: 0,
                    new_sequence: 1,
                    base_timestamp: None,
                    new_timestamp: None,
                    extra: HashMap::new(),
                },
                changes: ChangeInventory {
                    identity: None,
                    principals: None,
                    credentials: None,
                    memory: None,
                    raw: None,
                    extra: HashMap::new(),
                },
                extra: HashMap::new(),
            };
            let mut dw = DeltaWriter::new(Cursor::new(Vec::new()), delta_manifest).unwrap();
            dw.add_raw_change("raw/test-runtime/SOUL.md", b"soul v1")
                .unwrap();
            dw.add_raw_change("raw/test-runtime/memory/2026-01-16.md", b"day 16")
                .unwrap();
            dw.add_raw_deletion("raw/test-runtime/stale.md");
            dw.finish().unwrap().into_inner()
        };

        let rebuilt = rebuild_snapshot(&base, &[delta.as_slice()]).unwrap();
        let mut reader = AlfReader::new(Cursor::new(&rebuilt)).unwrap();

        // Updated file reflects the delta.
        assert_eq!(
            reader.read_raw_entry("raw/test-runtime/SOUL.md").unwrap(),
            b"soul v1"
        );
        // Unchanged file carried forward from the base.
        assert_eq!(
            reader
                .read_raw_entry("raw/test-runtime/memory/2026-01-15.md")
                .unwrap(),
            b"day 15"
        );
        // New file added by the delta.
        assert_eq!(
            reader
                .read_raw_entry("raw/test-runtime/memory/2026-01-16.md")
                .unwrap(),
            b"day 16"
        );
        // Deleted file is gone.
        assert!(
            !reader
                .file_names()
                .iter()
                .any(|f| f == "raw/test-runtime/stale.md"),
            "deleted raw path should not survive rebuild"
        );
    }

    #[test]
    fn rebuild_repartitions_across_quarters() {
        // Base has records in Q1
        let records_q1 = vec![
            make_record(1, "Q1 record A", 1),
            make_record(2, "Q1 record B", 2),
        ];
        let base = build_snapshot(None, &records_q1);

        // Delta adds records in Q2 (month=4) and Q3 (month=7)
        let record_q2 = make_record(3, "Q2 record", 4);
        let record_q3 = make_record(4, "Q3 record", 7);
        let delta = build_delta(
            0,
            None,
            &[
                DeltaMemoryEntry {
                    operation: DeltaOperation::Create,
                    record: record_q2,
                },
                DeltaMemoryEntry {
                    operation: DeltaOperation::Create,
                    record: record_q3,
                },
            ],
        );

        let rebuilt = rebuild_snapshot(&base, &[delta.as_slice()]).unwrap();
        let reader = AlfReader::new(Cursor::new(&rebuilt)).unwrap();

        // Check partition layout in the manifest
        let manifest = reader.manifest().clone();
        let memory = manifest.layers.memory.unwrap();
        assert_eq!(memory.record_count, 4);
        assert_eq!(
            memory.partitions.len(),
            3,
            "Should have Q1, Q2, Q3 partitions"
        );

        let partition_files: Vec<&str> =
            memory.partitions.iter().map(|p| p.file.as_str()).collect();
        assert!(partition_files.contains(&"memory/2026-Q1.jsonl"));
        assert!(partition_files.contains(&"memory/2026-Q2.jsonl"));
        assert!(partition_files.contains(&"memory/2026-Q3.jsonl"));

        // Verify total records
        let total: u64 = memory.partitions.iter().map(|p| p.record_count).sum();
        assert_eq!(total, 4);
    }

    #[test]
    fn rebuild_replaces_credentials_on_rekey() {
        // Base: 3 credentials. Delta: same 3 ids, re-keyed (every ciphertext
        // and nonce changed). Rebuild must carry the re-keyed envelopes.
        let base = build_snapshot_creds(&make_credentials_doc(vec![
            make_credential(1, "old-ct-1", "old-n-1"),
            make_credential(2, "old-ct-2", "old-n-2"),
            make_credential(3, "old-ct-3", "old-n-3"),
        ]));
        let delta = build_delta_creds(
            0,
            &make_credentials_doc(vec![
                make_credential(1, "new-ct-1", "new-n-1"),
                make_credential(2, "new-ct-2", "new-n-2"),
                make_credential(3, "new-ct-3", "new-n-3"),
            ]),
        );

        let rebuilt = rebuild_snapshot(&base, &[delta.as_slice()]).unwrap();
        let creds = read_credentials(&rebuilt).expect("credentials present");
        assert_eq!(creds.credentials.len(), 3);
        for c in &creds.credentials {
            assert!(
                c.encrypted_payload.starts_with("new-ct-"),
                "re-keyed ciphertext expected, got {}",
                c.encrypted_payload
            );
        }
    }

    #[test]
    fn credential_added_after_base_propagates_via_delta() {
        // Mirrors WP1 (b): base has 3 credentials, a 4th is added. The by-id
        // diff reports exactly one create; the delta carries the full current
        // Layer 4 (Design A); rebuild yields all four.
        let base_doc = make_credentials_doc(vec![
            make_credential(1, "ct-1", "n-1"),
            make_credential(2, "ct-2", "n-2"),
            make_credential(3, "ct-3", "n-3"),
        ]);
        let mut curr_doc = base_doc.clone();
        curr_doc.credentials.push(make_credential(4, "ct-4", "n-4"));

        let diff = crate::delta::diff_credentials(Some(&base_doc), Some(&curr_doc));
        assert_eq!(diff.created.len(), 1, "exactly one new credential");
        assert!(diff.updated.is_empty(), "unchanged three not re-listed");
        assert!(diff.deleted.is_empty());

        let base = build_snapshot_creds(&base_doc);
        let delta = build_delta_creds(0, &curr_doc); // Design A: full current Layer 4
        let rebuilt = rebuild_snapshot(&base, &[delta.as_slice()]).unwrap();

        let creds = read_credentials(&rebuilt).expect("credentials present");
        assert_eq!(creds.credentials.len(), 4);
        let mut id4 = [0u8; 16];
        id4[15] = 4;
        assert!(creds
            .credentials
            .iter()
            .any(|c| c.id == uuid::Uuid::from_bytes(id4)));
    }

    #[test]
    fn rebuild_applies_credential_delete() {
        // Base: 3 credentials. Delta: 2 of them (id 2 dropped). Rebuilt has 2.
        let base = build_snapshot_creds(&make_credentials_doc(vec![
            make_credential(1, "ct-1", "n-1"),
            make_credential(2, "ct-2", "n-2"),
            make_credential(3, "ct-3", "n-3"),
        ]));
        let delta = build_delta_creds(
            0,
            &make_credentials_doc(vec![
                make_credential(1, "ct-1", "n-1"),
                make_credential(3, "ct-3", "n-3"),
            ]),
        );

        let rebuilt = rebuild_snapshot(&base, &[delta.as_slice()]).unwrap();
        let creds = read_credentials(&rebuilt).expect("credentials present");
        assert_eq!(creds.credentials.len(), 2);
        let mut dropped_id = [0u8; 16];
        dropped_id[15] = 2;
        let dropped = uuid::Uuid::from_bytes(dropped_id);
        assert!(creds.credentials.iter().all(|c| c.id != dropped));
    }

    #[test]
    fn rebuild_applies_all_credentials_deleted() {
        // Base: 1 credential. Delta: empty credentials document (all deleted).
        // Rebuilt must NOT carry the base credential forward.
        let base = build_snapshot_creds(&make_credentials_doc(vec![make_credential(
            1, "ct-1", "n-1",
        )]));
        let delta = build_delta_creds(0, &make_credentials_doc(vec![]));

        let rebuilt = rebuild_snapshot(&base, &[delta.as_slice()]).unwrap();
        let count = read_credentials(&rebuilt).map_or(0, |d| d.credentials.len());
        assert_eq!(count, 0);
    }

    // -- replace_memory_records (WP4.1) ----------------------------------------

    /// Snapshot with an identity, records, and a raw source file — the layers
    /// replace_memory_records must carry through untouched.
    fn build_snapshot_with_raw(records: &[MemoryRecord]) -> Vec<u8> {
        let buf = Cursor::new(Vec::new());
        let mut writer = AlfWriter::new(buf, make_manifest()).unwrap();
        writer.set_identity(&make_identity(1, "soul text")).unwrap();
        let mut groups: BTreeMap<String, Vec<MemoryRecord>> = BTreeMap::new();
        for r in records {
            let path = PartitionAssigner::partition_for_record(r);
            groups.entry(path).or_default().push(r.clone());
        }
        for (file_path, group_records) in &groups {
            let (from, to) = PartitionAssigner::date_range_for_partition(file_path).unwrap();
            let info = MemoryPartitionInfo {
                file: file_path.clone(),
                from,
                to: Some(to),
                record_count: group_records.len() as u64,
                sealed: false,
                extra: HashMap::new(),
            };
            writer.add_memory_partition(info, group_records).unwrap();
        }
        writer
            .add_raw_source("test", "MEMORY.md", b"verbatim raw bytes")
            .unwrap();
        let inner = writer.finish().unwrap();
        inner.into_inner()
    }

    #[test]
    fn replace_memory_records_preserves_other_layers() {
        let base = build_snapshot_with_raw(&[make_record(1, "old", 1)]);
        let base_manifest = AlfReader::new(Cursor::new(&base))
            .unwrap()
            .manifest()
            .clone();

        let replacement = vec![make_record(2, "new content", 1)];
        let rewritten = replace_memory_records(&base, &replacement).unwrap();
        let mut reader = AlfReader::new(Cursor::new(&rewritten)).unwrap();

        // Memory layer replaced.
        let records = reader.read_all_memory().unwrap();
        assert_eq!(records, replacement);
        // Identity carried.
        let identity = reader.read_identity().unwrap().expect("identity present");
        assert_eq!(identity.prose.unwrap().soul.as_deref(), Some("soul text"));
        // Raw source byte-identical.
        let raw = reader.read_raw_entry("raw/test/MEMORY.md").unwrap();
        assert_eq!(raw, b"verbatim raw bytes");
        // Manifest metadata cloned, not restamped.
        let manifest = reader.manifest();
        assert_eq!(manifest.created_at, base_manifest.created_at);
        assert_eq!(manifest.agent.id, base_manifest.agent.id);
    }

    #[test]
    fn replace_memory_records_repartitions_across_quarters() {
        // Base holds one Q1 record; the replacement moves a record's anchor to
        // Q3 (reconcile carries created_at/observed_at backwards or forwards),
        // so the partition layout must follow the records, not the export.
        let base = build_snapshot_with_raw(&[make_record(1, "q1", 1)]);
        let replacement = vec![make_record(1, "q1", 1), make_record(2, "q3", 8)];
        let rewritten = replace_memory_records(&base, &replacement).unwrap();
        let reader = AlfReader::new(Cursor::new(&rewritten)).unwrap();
        let names = reader.file_names();
        assert!(names.contains(&"memory/2026-Q1.jsonl".to_string()));
        assert!(names.contains(&"memory/2026-Q3.jsonl".to_string()));
    }

    #[test]
    fn replace_memory_records_empty_set() {
        let base = build_snapshot_with_raw(&[make_record(1, "old", 1)]);
        let rewritten = replace_memory_records(&base, &[]).unwrap();
        let mut reader = AlfReader::new(Cursor::new(&rewritten)).unwrap();
        assert!(reader.read_all_memory().unwrap().is_empty());
        assert!(reader.read_identity().unwrap().is_some());
    }
}
