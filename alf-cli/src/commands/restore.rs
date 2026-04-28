//! `alf restore` — download and restore from the cloud.
//!
//! Flow:
//! 1. Load config (check API key)
//! 2. Parse agent ID
//! 3. Call restore endpoint (gets snapshot URL + delta URLs in one call)
//! 4. Download snapshot, download deltas, merge into one `.alf` via [`merge_snapshot_with_deltas`]
//! 5. Resolve adapter, import into workspace
//! 6. Save state with latest sequence (only after a successful import)

use crate::adapter;
use crate::api_client::ApiClient;
use crate::config::Config;
use crate::output;
use crate::state::{resolve_agent_id, AgentState};

use alf_core::archive::AlfReader;
use alf_core::rebuild::rebuild_snapshot;

use anyhow::Context;
use anyhow::Result;
use chrono::Utc;
use colored::Colorize;
use serde::Serialize;
use std::fs;
use std::io::Cursor;
use std::path::Path;
use uuid::Uuid;

#[derive(Serialize)]
struct RestoreResult {
    ok: bool,
    agent_id: String,
    agent_name: String,
    sequence: u64,
    runtime: String,
    memory_records: u64,
    workspace: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

/// Merge a base snapshot with zero or more delta archives (same semantics as cloud restore).
pub(crate) fn merge_snapshot_with_deltas(
    snapshot_bytes: &[u8],
    delta_byte_vecs: &[Vec<u8>],
) -> Result<Vec<u8>> {
    let delta_refs: Vec<&[u8]> = delta_byte_vecs.iter().map(|v| v.as_slice()).collect();
    rebuild_snapshot(snapshot_bytes, &delta_refs).map_err(|e| anyhow::anyhow!(e))
}

fn merged_last_sequence(merged_bytes: &[u8], snapshot_sequence: u64) -> Result<u64> {
    let reader =
        AlfReader::new(Cursor::new(merged_bytes)).context("Failed to read merged restore archive")?;
    Ok(reader
        .manifest()
        .sync
        .as_ref()
        .map(|s| s.last_sequence)
        .unwrap_or(snapshot_sequence))
}

pub fn run(runtime: &str, workspace: &Path, agent_arg: Option<&str>) -> Result<()> {
    let human = output::human_mode();

    // 1. Load config and create API client
    let config = Config::load()?;
    let client = ApiClient::from_config(&config)?;

    // 2. Resolve agent ID (CLI arg or ~/.alf/state/*.toml)
    let agent_id: Uuid = resolve_agent_id(agent_arg)?;

    // 3. Resolve adapter
    let adapt = adapter::get_adapter(runtime).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown runtime '{}'. Supported: {}",
            runtime,
            adapter::supported_runtimes()
        )
    })?;

    if human {
        println!(
            "{} Restoring agent {} into {} workspace...",
            "▸".blue().bold(),
            &agent_id.to_string()[..8],
            adapt.name()
        );
        println!("  Agent:     {agent_id}");
        println!("  Runtime:   {}", adapt.name());
        println!("  Workspace: {}", workspace.display());
        println!();
    } else {
        output::progress(&format!(
            "Restoring agent {}...",
            &agent_id.to_string()[..8]
        ));
    }

    // 4. Call restore endpoint — gets snapshot + delta URLs in one call
    output::progress("  Fetching restore manifest...");
    let restore = client.restore(agent_id)?;

    let snapshot_bytes = match &restore.snapshot {
        Some(snap) => {
            output::progress(&format!(
                "  Downloading snapshot (sequence {})...",
                snap.sequence
            ));
            client.download_presigned(&snap.url)?
        }
        None => {
            anyhow::bail!(
                "No snapshot available for agent {}. \
                 The agent must be synced at least once before restoring.",
                agent_id
            );
        }
    };

    let snapshot_sequence = restore.snapshot.as_ref().map(|s| s.sequence).unwrap_or(0);

    // 5. Download deltas and merge into one snapshot archive
    let delta_byte_vecs: Vec<Vec<u8>> = if restore.deltas.is_empty() {
        output::progress("  No additional deltas to apply.");
        Vec::new()
    } else {
        output::progress(&format!("  Downloading {} delta(s)...", restore.deltas.len()));
        let mut out = Vec::with_capacity(restore.deltas.len());
        for (i, delta_info) in restore.deltas.iter().enumerate() {
            output::progress(&format!(
                "  Downloading delta {} of {} (sequence {})...",
                i + 1,
                restore.deltas.len(),
                delta_info.sequence
            ));
            out.push(client.download_presigned(&delta_info.url)?);
        }
        output::progress(&format!("  Merging {} delta(s) into snapshot...", restore.deltas.len()));
        out
    };

    let final_bytes = merge_snapshot_with_deltas(&snapshot_bytes, &delta_byte_vecs)
        .context("Failed to merge snapshot and deltas for restore")?;

    let latest_sequence = merged_last_sequence(&final_bytes, snapshot_sequence)?;

    // 6. Write snapshot to temp file and import
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    let temp_alf = temp_dir.path().join("restored.alf");
    fs::write(&temp_alf, &final_bytes)?;

    output::progress("  Importing into workspace...");
    let import_report = adapt.import(&temp_alf, workspace)?;

    // 7. Save state only after a successful import
    let state = AgentState {
        agent_id,
        last_synced_sequence: latest_sequence,
        last_synced_at: Some(Utc::now()),
        snapshot_path: None,
    };
    state.save()?;

    // 8. Output result
    if human {
        let state_path = AgentState::path_for(agent_id)?;
        println!();
        println!("  State file:   {}", state_path.display());
        println!("{} Restore complete", "✓".green().bold());
        println!();
        println!("  Agent:      {}", import_report.agent_name);
        println!("  Memories:   {}", import_report.memory_records);
        if import_report.identity_imported {
            println!("  Identity:   restored");
        }
        if import_report.principals_count > 0 {
            println!("  Principals: {}", import_report.principals_count);
        }
        if import_report.credentials_count > 0 {
            println!("  Credentials: {}", import_report.credentials_count);
        }
        println!("  Sequence:   {latest_sequence}");
        println!();
        println!("  Workspace: {}", workspace.display());

        if !import_report.warnings.is_empty() {
            println!();
            println!("  {} Warnings:", "⚠".yellow().bold());
            for w in &import_report.warnings {
                println!("    • {w}");
            }
        }
    } else {
        output::json(&RestoreResult {
            ok: true,
            agent_id: agent_id.to_string(),
            agent_name: import_report.agent_name.clone(),
            sequence: latest_sequence,
            runtime: runtime.to_string(),
            memory_records: import_report.memory_records,
            workspace: workspace.to_string_lossy().into(),
            warnings: import_report.warnings.clone(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alf_core::archive::{AlfWriter, DeltaMemoryEntry, DeltaWriter};
    use alf_core::manifest::{
        AgentMetadata, ChangeInventory, DeltaAgentRef, DeltaManifest, DeltaOperation,
        DeltaSyncCursor, LayerInventory, Manifest,
    };
    use alf_core::memory::{
        MemoryRecord, MemoryStatus, MemoryType, SourceProvenance, TemporalMetadata,
    };
    use chrono::{TimeZone, Utc};
    use std::collections::HashMap;

    fn make_agent_id() -> uuid::Uuid {
        uuid::Uuid::parse_str("a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d").unwrap()
    }

    fn make_manifest() -> Manifest {
        Manifest {
            alf_version: "1.0.0".into(),
            created_at: Utc::now(),
            agent: AgentMetadata {
                id: make_agent_id(),
                name: "Test Agent".into(),
                source_runtime: "test".into(),
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

    fn make_record(id_suffix: u8, content: &str) -> MemoryRecord {
        let mut id_bytes = [0u8; 16];
        id_bytes[15] = id_suffix;
        MemoryRecord {
            id: uuid::Uuid::from_bytes(id_bytes),
            agent_id: make_agent_id(),
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
                created_at: Utc.with_ymd_and_hms(2026, 1, 15, 10, 0, 0).unwrap(),
                updated_at: None,
                observed_at: Some(Utc.with_ymd_and_hms(2026, 1, 15, 10, 0, 0).unwrap()),
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

    fn build_minimal_snapshot(records: &[MemoryRecord]) -> Vec<u8> {
        use alf_core::manifest::MemoryPartitionInfo;
        use alf_core::partition::PartitionAssigner;
        use std::collections::BTreeMap;

        let buf = Cursor::new(Vec::new());
        let mut writer = AlfWriter::new(buf, make_manifest()).unwrap();

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

        writer.finish().unwrap().into_inner()
    }

    fn build_delta(base_sequence: u64, entries: &[DeltaMemoryEntry]) -> Vec<u8> {
        let delta_manifest = DeltaManifest {
            alf_version: "1.0.0".into(),
            created_at: Utc::now(),
            agent: DeltaAgentRef {
                id: make_agent_id(),
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
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        };

        let buf = Cursor::new(Vec::new());
        let mut writer = DeltaWriter::new(buf, delta_manifest).unwrap();
        if !entries.is_empty() {
            writer.add_memory_deltas(entries).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn read_record_contents(alf: &[u8]) -> Vec<String> {
        let mut r = AlfReader::new(Cursor::new(alf)).unwrap();
        r.read_all_memory()
            .unwrap()
            .into_iter()
            .map(|rec| rec.content)
            .collect()
    }

    #[test]
    fn merge_with_deltas_includes_delta_memory_changes() {
        let a = make_record(1, "A");
        let b = make_record(2, "B");
        let snap = build_minimal_snapshot(&[a.clone(), b.clone()]);

        let c = make_record(3, "C");
        let delta = build_delta(
            0,
            &[DeltaMemoryEntry {
                operation: DeltaOperation::Create,
                record: c,
            }],
        );

        let merged = merge_snapshot_with_deltas(&snap, &[delta]).unwrap();
        let contents = read_record_contents(&merged);

        assert_eq!(contents.len(), 3);
        assert!(contents.contains(&"A".into()));
        assert!(contents.contains(&"B".into()));
        assert!(contents.contains(&"C".into()));
    }

    #[test]
    fn merge_with_no_deltas_keeps_records() {
        let a = make_record(1, "A");
        let snap = build_minimal_snapshot(&[a.clone()]);
        let merged = merge_snapshot_with_deltas(&snap, &[]).unwrap();
        let contents = read_record_contents(&merged);
        assert_eq!(contents, vec!["A".to_string()]);
    }

    #[test]
    fn merge_rejects_corrupt_delta() {
        let a = make_record(1, "A");
        let snap = build_minimal_snapshot(&[a]);
        let bad = vec![1u8, 2, 3];
        assert!(merge_snapshot_with_deltas(&snap, &[bad]).is_err());
    }
}
